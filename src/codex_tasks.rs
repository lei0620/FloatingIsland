//! Codex lifecycle events. Hooks write metadata-only files; no transcript or token is read.
use serde_json::{json, Value};
use std::{
    collections::{HashMap, HashSet},
    fs,
    io::{self, Read},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// Build a local chat link from one opaque, URL-safe path segment.
pub fn thread_url(session: &str) -> Option<String> {
    if session.is_empty()
        || session.len() > 160
        || session.eq_ignore_ascii_case("new")
        || !session.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
    {
        return None;
    }
    Some(format!("codex://threads/{session}"))
}

#[test]
fn thread_url_accepts_only_safe_chat_path_segments() {
    assert_eq!(thread_url("thr_123-ab").as_deref(), Some("codex://threads/thr_123-ab"));
    for invalid in ["", "new", "NEW", "../settings", "abc?prompt=hi", "abc%2Fnew", "abc\n"] {
        assert!(thread_url(invalid).is_none(), "{invalid:?}");
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ThreadLinkError {
    InvalidId,
    MissingRollout,
}

/// Resolve a desktop link only when the local session store has a nonempty rollout.
/// Check filenames and metadata only. The conversation itself is never opened here.
pub fn local_thread_url(home: &Path, session: &str) -> Result<String, ThreadLinkError> {
    let url = thread_url(session).ok_or(ThreadLinkError::InvalidId)?;
    if has_rollout(&home.join("sessions"), session, 3)
        || has_rollout(&home.join("archived_sessions"), session, 3)
    {
        Ok(url)
    } else {
        Err(ThreadLinkError::MissingRollout)
    }
}

fn has_rollout(dir: &Path, session: &str, depth: u8) -> bool {
    any_rollout(dir, depth, &mut |id, entry| {
        id.eq_ignore_ascii_case(session) && entry.metadata().is_ok_and(|meta| meta.len() > 0)
    })
}

fn any_rollout(
    dir: &Path,
    depth: u8,
    matches: &mut impl FnMut(&str, &fs::DirEntry) -> bool,
) -> bool {
    let Ok(entries) = fs::read_dir(dir) else { return false; };
    entries.flatten().any(|entry| {
        let Ok(kind) = entry.file_type() else { return false; };
        if kind.is_dir() && depth > 0 {
            return any_rollout(&entry.path(), depth - 1, matches);
        }
        if !kind.is_file() { return false; }
        let name = entry.file_name();
        let id = name.to_str()
            .and_then(|name| name.strip_prefix("rollout-"))
            .and_then(|name| name.strip_suffix(".jsonl").or_else(|| name.strip_suffix(".jsonl.zst")))
            // The rollout filename starts with YYYY-MM-DDTHH-MM-SS-.
            .and_then(|name| name.get(19..))
            .and_then(|name| name.strip_prefix('-'));
        id.is_some_and(|id| matches(id, &entry))
    })
}

#[derive(Default)]
pub(crate) struct UnreadState {
    file_stamp: Option<(SystemTime, u64)>,
    local_ids: HashSet<String>,
}

impl UnreadState {
    /// Read the desktop client's persisted unread metadata. An unknown version is
    /// treated as unavailable so raw Hook events cannot surface as unread messages.
    fn parse(state: &Value) -> HashSet<String> {
        let read = &state["electron-thread-read-state-v1"];
        if read["version"].as_u64() != Some(1) {
            return HashSet::new();
        }
        let adopted_local = read["legacyMigration"]["adoptedHostIds"]["local"].as_str();
        let mut ids = HashSet::new();
        if let Some(identities) = read["unreadByIdentity"].as_object() {
            for hosts in identities.values().filter_map(Value::as_object) {
                for (host, unread) in hosts {
                    if host == "local" || Some(host.as_str()) == adopted_local {
                        ids.extend(unread.as_array().into_iter().flatten()
                            .filter_map(Value::as_str).map(str::to_ascii_lowercase));
                    }
                }
            }
        }
        ids
    }

    pub(crate) fn poll(&mut self, home: &Path) -> HashSet<String> {
        let file = home.join(".codex-global-state.json");
        let stamp = fs::metadata(&file).ok()
            .and_then(|meta| Some((meta.modified().ok()?, meta.len())));
        if stamp != self.file_stamp {
            let read = stamp.and_then(|_| fs::read(&file).ok())
                .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok());
            self.file_stamp = read.as_ref().and(stamp);
            self.local_ids = read.as_ref().map(Self::parse).unwrap_or_default();
        }
        let mut active = HashSet::new();
        if !self.local_ids.is_empty() {
            // The desktop unread set can retain archived IDs. Only active
            // rollouts can raise a live reminder.
            any_rollout(&home.join("sessions"), 3, &mut |id, entry| {
                let id = id.to_ascii_lowercase();
                if self.local_ids.contains(&id) && entry.metadata().is_ok_and(|meta| meta.len() > 0) {
                    active.insert(id);
                }
                false
            });
        }
        active
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Phase {
    Running,
    Completed,
    Interrupted,
    Attention,
}

#[derive(Clone, Debug)]
pub struct Event {
    pub session: String,
    pub turn: String,
    pub phase: Phase,
    pub workspace: String,
    pub at: u64,
}

fn field(v: &Value, key: &str, max: usize) -> String {
    v[key]
        .as_str()
        .unwrap_or_default()
        .chars()
        .filter(|c| !c.is_control())
        .take(max)
        .collect()
}

impl Event {
    pub fn from_hook(v: &Value, at: u64) -> Option<Self> {
        // Subagents have their own lifecycle and must not complete the parent task.
        if v.get("agent_id").is_some_and(|v| !v.is_null()) {
            return None;
        }
        let phase = match v["hook_event_name"].as_str()? {
            "UserPromptSubmit" => Phase::Running,
            "Stop" => Phase::Completed,
            "Interrupt" => Phase::Interrupted,
            "PermissionRequest" => Phase::Attention,
            _ => return None,
        };
        let session = field(v, "session_id", 160);
        let turn = field(v, "turn_id", 160);
        if session.is_empty() || turn.is_empty() {
            return None;
        }
        let cwd = field(v, "cwd", 2048);
        let workspace = cwd
            .trim_end_matches(['/', '\\'])
            .rsplit(['/', '\\'])
            .next()
            .unwrap_or("")
            .to_owned();
        Some(Self {
            session,
            turn,
            phase,
            workspace,
            at,
        })
    }
    fn json(&self) -> Value {
        json!({"session_id":self.session,"turn_id":self.turn,"phase":match self.phase {
            Phase::Running=>"running",Phase::Completed=>"completed",Phase::Interrupted=>"interrupted",Phase::Attention=>"attention"
        },"workspace":self.workspace,"at":self.at})
    }
    fn parse(v: &Value) -> Option<Self> {
        let phase = match v["phase"].as_str()? {
            "running" => Phase::Running,
            "completed" => Phase::Completed,
            "interrupted" => Phase::Interrupted,
            "attention" => Phase::Attention,
            _ => return None,
        };
        let session = field(v, "session_id", 160);
        let turn = field(v, "turn_id", 160);
        if session.is_empty() || turn.is_empty() {
            return None;
        }
        Some(Self {
            session,
            turn,
            phase,
            workspace: field(v, "workspace", 256),
            at: v["at"].as_u64()?,
        })
    }
}

pub fn event_dir() -> PathBuf {
    crate::model::config_dir().join("codex-events")
}

/// Used by the same executable with --codex-hook, before creating windows or services.
pub fn write_hook(input: impl Read, dir: &Path) -> Result<(), String> {
    let mut bytes = Vec::new();
    input
        .take(256 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() > 256 * 1024 {
        return Err("Codex 事件超过大小限制".into());
    }
    let value: Value = serde_json::from_slice(&bytes).map_err(|_| "Codex 事件不是有效 JSON")?;
    if let Some(event) = Event::from_hook(&value, now_ms()) {
        fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        let serial = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        crate::model::atomic_write(
            &dir.join(format!("{serial}-{}.json", std::process::id())),
            event.json().to_string().as_bytes(),
        )
        .map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[derive(Default)]
pub struct Tracker {
    turns: HashMap<(String, String), (Phase, u64)>,
}
impl Tracker {
    pub fn accept(&mut self, e: &Event, now: u64) -> bool {
        self.turns
            .retain(|_, (_, at)| now.saturating_sub(*at) < 24 * 60 * 60 * 1000);
        if now.saturating_sub(e.at) > 10 * 60 * 1000 || e.at > now.saturating_add(60_000) {
            return false;
        }
        let key = (e.session.clone(), e.turn.clone());
        if let Some((phase, at)) = self.turns.get(&key) {
            if e.at < *at
                || *phase == e.phase
                || matches!(phase, Phase::Completed | Phase::Interrupted)
            {
                return false;
            }
        }
        self.turns.insert(key, (e.phase.clone(), e.at));
        true
    }
    pub fn running(&self) -> usize {
        self.turns
            .values()
            .filter(|(p, _)| matches!(p, Phase::Running | Phase::Attention))
            .count()
    }
}

pub fn drain(dir: &Path) -> Vec<Event> {
    let Ok(entries) = fs::read_dir(dir) else {
        return vec![];
    };
    let mut events = Vec::new();
    for entry in entries.flatten().take(512) {
        let path = entry.path();
        if path.extension().is_none_or(|e| e != "json") {
            continue;
        }
        if entry.metadata().is_ok_and(|m| m.len() <= 4096) {
            if let Ok(bytes) = fs::read(&path) {
                if let Ok(v) = serde_json::from_slice(&bytes) {
                    if let Some(e) = Event::parse(&v) {
                        events.push(e);
                    }
                }
            }
        }
        let _ = fs::remove_file(path);
    }
    events.sort_by_key(|e| e.at);
    events
}

pub fn codex_home() -> PathBuf {
    std::env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(
                std::env::var_os("USERPROFILE")
                    .or_else(|| std::env::var_os("HOME"))
                    .unwrap_or_default(),
            )
            .join(".codex")
        })
}

/// Merge only our managed groups, preserving existing hooks and the legacy notify command.
pub fn merge_hooks(mut config: Value, executable: &Path) -> Result<Value, String> {
    let root = config.as_object_mut().ok_or("hooks.json 顶层必须是对象")?;
    let hooks = root
        .entry("hooks")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .ok_or("hooks 字段必须是对象")?;
    let exe = executable.to_string_lossy();
    if exe.contains(['"', '\r', '\n']) {
        return Err("程序路径不能包含引号或换行".into());
    }
    let events = event_dir().to_string_lossy().into_owned();
    if events.contains(['"', '\r', '\n']) {
        return Err("数据目录不能包含引号或换行".into());
    }
    let command = format!("\"{exe}\" --codex-hook \"{events}\"");
    // Windows hooks run in PowerShell: a quoted executable needs the call operator.
    let command_windows = format!(
        "& '{}' --codex-hook '{}'",
        exe.replace('\'', "''"),
        events.replace('\'', "''")
    );
    for event in ["UserPromptSubmit", "Stop", "Interrupt", "PermissionRequest"] {
        let groups = hooks
            .entry(event)
            .or_insert_with(|| json!([]))
            .as_array_mut()
            .ok_or_else(|| format!("{event} 必须是数组"))?;
        // Remove only groups installed by this application. Do not touch other handlers.
        for group in groups.iter_mut() {
            if let Some(handlers) = group["hooks"].as_array_mut() {
                handlers.retain(|h| {
                    h["statusMessage"].as_str() != Some("岛仔任务状态")
                        || !h["command"]
                            .as_str()
                            .is_some_and(|c| c.contains(" --codex-hook"))
                });
            }
        }
        groups.retain(|g| g["hooks"].as_array().is_none_or(|h| !h.is_empty()));
        groups.push(json!({"hooks":[{"type":"command","command":command,"commandWindows":command_windows,"timeout":3,"statusMessage":"岛仔任务状态"}]}));
    }
    Ok(config)
}

pub fn install_hooks(
    home: &Path,
    executable: &Path,
    backup_root: &Path,
) -> Result<PathBuf, String> {
    if !executable.is_file() {
        return Err("请先构建浮岛程序".into());
    }
    let path = home.join("hooks.json");
    let original = match fs::read(&path) {
        Ok(bytes) => Some(bytes),
        Err(e) if e.kind() == io::ErrorKind::NotFound => None,
        Err(e) => return Err(e.to_string()),
    };
    let value = match &original {
        Some(bytes) => {
            serde_json::from_slice(bytes).map_err(|_| "现有 hooks.json 格式错误，未修改")?
        }
        None => json!({}),
    };
    let next = merge_hooks(value, executable)?;
    let bytes = serde_json::to_vec_pretty(&next).map_err(|e| e.to_string())?;
    if original.as_deref() == Some(bytes.as_slice()) {
        return Ok(path);
    }
    if let Some(original) = &original {
        // create_new prevents an accidental backup overwrite during a repair.
        use std::io::Write;
        let mut backup = backup_root.join(".codex").join("hooks.json");
        if backup.exists() {
            backup = backup_root
                .join(now_ms().to_string())
                .join(".codex")
                .join("hooks.json");
        }
        fs::create_dir_all(backup.parent().unwrap()).map_err(|e| e.to_string())?;
        fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(backup)
            .and_then(|mut f| f.write_all(&original))
            .map_err(|e| format!("备份失败，未修改 Codex：{e}"))?;
    }
    if fs::read(&path).ok() != original {
        return Err("Codex 配置已被其他程序修改，请重新安装接入".into());
    }
    fs::create_dir_all(home).map_err(|e| e.to_string())?;
    crate::model::atomic_write(&path, &bytes).map_err(|e| e.to_string())?;
    Ok(path)
}

pub fn backup_dir() -> PathBuf {
    #[cfg(windows)]
    {
        let root = Path::new("D:/lei/xmcodex/beifen");
        if Path::new("D:/").exists() {
            return root.join(format!(
                "{}_岛仔Codex接入",
                chrono::Local::now().format("%Y-%m-%d_%H%M")
            ));
        }
    }
    crate::model::config_dir()
        .join("backups")
        .join(now_ms().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unread_snapshot_excludes_archived_remote_and_already_read_threads() {
        let home = std::env::temp_dir().join(format!("daozai-codex-unread-{}-{}", std::process::id(), now_ms()));
        let active = home.join("sessions").join("2026").join("11").join("09");
        let archived = home.join("archived_sessions");
        fs::create_dir_all(&active).unwrap();
        fs::create_dir_all(&archived).unwrap();
        let rollout = |dir: &Path, id: &str| dir.join(format!("rollout-2026-11-09T15-00-00-{id}.jsonl"));
        for id in ["fresh", "read", "remote", "legacy"] {
            fs::write(rollout(&active, id), b"fixture").unwrap();
        }
        let fresh = rollout(&active, "fresh");
        fs::write(rollout(&archived, "archive"), b"fixture").unwrap();
        let state = json!({"electron-thread-read-state-v1": {
            "version": 1,
            "unreadByIdentity": {"account": {"local:fixture": ["fresh", "archive"], "remote-host": ["remote"]}},
            "legacyMigration": {"adoptedHostIds": {"local": "local:fixture"}, "unreadThreadIdsByHostId": {"local": ["legacy"]}}
        }});
        let file = home.join(".codex-global-state.json");
        fs::write(&file, state.to_string()).unwrap();
        let mut unread = UnreadState::default();
        assert_eq!(unread.poll(&home), HashSet::from(["fresh".into()]));
        fs::rename(&fresh, rollout(&archived, "fresh")).unwrap();
        assert!(unread.poll(&home).is_empty(), "归档时即使未读文件未更新，也应清除提醒");
        fs::rename(rollout(&archived, "fresh"), &fresh).unwrap();
        let mut state = state;
        state["electron-thread-read-state-v1"]["unreadByIdentity"]["account"]["local:fixture"] = json!([]);
        fs::write(&file, state.to_string()).unwrap();
        assert!(unread.poll(&home).is_empty(), "已读后应清除提醒");
    }

    #[test]
    fn local_thread_link_requires_an_exact_nonempty_active_or_archived_rollout() {
        let home = std::env::temp_dir().join(format!("daozai-codex-link-{}-{}", std::process::id(), now_ms()));
        let active = home.join("sessions").join("2026").join("11").join("09");
        let archived = home.join("archived_sessions");
        fs::create_dir_all(&active).unwrap();
        fs::create_dir_all(&archived).unwrap();
        let id = "018f0000-0000-7000-8000-000000000001";
        let filename = format!("rollout-2026-11-09T15-00-00-{id}.jsonl");
        let url = format!("codex://threads/{id}");
        let active_file = active.join(&filename);
        let archived_file = archived.join(format!("{filename}.zst"));
        assert_eq!(local_thread_url(&home, "../other"), Err(ThreadLinkError::InvalidId));
        assert_eq!(local_thread_url(&home, id), Err(ThreadLinkError::MissingRollout));
        fs::write(&active_file, b"").unwrap();
        assert_eq!(local_thread_url(&home, id), Err(ThreadLinkError::MissingRollout));
        fs::write(&active_file, b"test fixture").unwrap();
        assert_eq!(local_thread_url(&home, id), Ok(url.clone()));
        assert_eq!(local_thread_url(&home, "000000000001"), Err(ThreadLinkError::MissingRollout));
        fs::rename(&active_file, &archived_file).unwrap();
        assert_eq!(local_thread_url(&home, id), Ok(url));
        fs::remove_file(&archived_file).unwrap();
        assert_eq!(local_thread_url(&home, id), Err(ThreadLinkError::MissingRollout));
    }

    fn event(phase: Phase, turn: &str, at: u64) -> Event {
        Event {
            session: "session".into(),
            turn: turn.into(),
            phase,
            workspace: "项目".into(),
            at,
        }
    }
    #[test]
    fn concurrent_turns_deduplicate_and_do_not_regress() {
        let mut t = Tracker::default();
        assert!(t.accept(&event(Phase::Running, "one", 1), 10));
        assert!(t.accept(&event(Phase::Running, "two", 2), 10));
        assert_eq!(t.running(), 2);
        assert!(t.accept(&event(Phase::Completed, "one", 3), 10));
        assert_eq!(t.running(), 1);
        assert!(!t.accept(&event(Phase::Completed, "one", 4), 10));
        assert!(!t.accept(&event(Phase::Running, "one", 2), 10));
        assert!(!t.accept(&event(Phase::Completed, "old", 1), 700_000));
    }
    #[test]
    fn hook_does_not_keep_prompts_transcripts_or_assistant_text() {
        let v = json!({"hook_event_name":"Stop","session_id":"s","turn_id":"t","cwd":"D:\\项目\\岛仔","last_assistant_message":"private","transcript_path":"secret","prompt":"private"});
        let e = Event::from_hook(&v, 5).unwrap();
        assert_eq!(e.workspace, "岛仔");
        assert!(!e.json().to_string().contains("private"));
        let mut child = v;
        child["agent_id"] = json!("child");
        assert!(Event::from_hook(&child, 5).is_none());
    }
    #[test]
    fn merge_preserves_user_hooks_and_is_repeatable() {
        let old = json!({"description":"keep","hooks":{"PermissionRequest":[{"matcher":"Bash","hooks":[{"command":"existing"}]}],"Stop":[{"hooks":[{"command":"other"}]}]}});
        let exe = Path::new("D:/测试 程序/daozai.exe");
        let next = merge_hooks(old, exe).unwrap();
        assert_eq!(
            next["hooks"]["PermissionRequest"][0]["hooks"][0]["command"],
            "existing"
        );
        assert_eq!(next, merge_hooks(next.clone(), exe).unwrap());
        assert_eq!(next["hooks"]["Stop"].as_array().unwrap().len(), 2);
        assert!(next["hooks"]["Stop"][1]["hooks"][0]["commandWindows"]
            .as_str()
            .unwrap()
            .starts_with("& 'D:/测试 程序/daozai.exe'"));
    }
    #[test]
    fn hook_files_are_metadata_only_and_drained_once() {
        let root = std::env::temp_dir().join(format!(
            "daozai-hook-test-{}-{}",
            std::process::id(),
            now_ms()
        ));
        let payload = json!({"session_id":"s","turn_id":"t","hook_event_name":"Stop","last_assistant_message":"private body"});
        write_hook(payload.to_string().as_bytes(), &root).unwrap();
        let events = drain(&root);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].phase, Phase::Completed);
        assert!(drain(&root).is_empty());
    }
    #[test]
    fn installer_backs_up_exact_original_and_invalid_config_is_not_changed() {
        let root = std::env::temp_dir().join(format!(
            "daozai-hook-install-{}-{}",
            std::process::id(),
            now_ms()
        ));
        let home = root.join("home");
        let backups = root.join("backups");
        fs::create_dir_all(&home).unwrap();
        let exe = root.join("daozai.exe");
        fs::write(&exe, b"test fixture").unwrap();
        let original = b"{\"hooks\":{\"PermissionRequest\":[]},\"description\":\"keep\"}";
        fs::write(home.join("hooks.json"), original).unwrap();
        install_hooks(&home, &exe, &backups).unwrap();
        let backup = backups.join(".codex").join("hooks.json");
        assert_eq!(fs::read(backup).unwrap(), original);
        let installed = fs::read(home.join("hooks.json")).unwrap();
        install_hooks(&home, &exe, &backups).unwrap();
        assert_eq!(fs::read(home.join("hooks.json")).unwrap(), installed);
        fs::write(home.join("hooks.json"), b"invalid").unwrap();
        assert!(install_hooks(&home, &exe, &backups).is_err());
        assert_eq!(fs::read(home.join("hooks.json")).unwrap(), b"invalid");
    }
}
