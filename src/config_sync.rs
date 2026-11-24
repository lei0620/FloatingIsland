use crate::app::App;
use crate::config_sync_preferences::ExtraPreferences;
use crate::mail::{self, Account, CustomImap, Provider};
use crate::model::{self, NoticeKind};
use crate::sedentary;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

const FORMAT: &str = "leidao-config";
const VERSION: u32 = 2;
pub(crate) const MAX_BYTES: u64 = 256 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Section {
    Appearance,
    Pages,
    Music,
    Notifications,
    Calendar,
    Clipboard,
    Mail,
    Sedentary,
    Water,
    Focus,
}

impl Section {
    pub(crate) const ALL: [Self; 10] = [
        Self::Appearance,
        Self::Pages,
        Self::Music,
        Self::Notifications,
        Self::Mail,
        Self::Calendar,
        Self::Sedentary,
        Self::Water,
        Self::Focus,
        Self::Clipboard,
    ];

    pub(crate) fn title(self) -> &'static str {
        match self {
            Self::Appearance => "外观与行为",
            Self::Pages => "页面与常驻组件",
            Self::Music => "音乐与歌词",
            Self::Notifications => "通知与消息",
            Self::Calendar => "日程偏好",
            Self::Clipboard => "剪贴板偏好",
            Self::Mail => "邮箱配置",
            Self::Sedentary => "久坐提醒",
            Self::Water => "喝水设置",
            Self::Focus => "专注计时",
        }
    }

    pub(crate) fn description(self) -> &'static str {
        match self {
            Self::Appearance => "皮肤、形态、主题、大小、停靠、时钟和自动避让",
            Self::Pages => "功能页开关、顺序和常驻组件排列",
            Self::Music => "歌词开关、岛上歌词和时间偏移",
            Self::Notifications => "通知时长、声音、待办、系统与应用提醒",
            Self::Mail => "邮箱地址、服务器和收信偏好",
            Self::Calendar => "自动同步与会前提醒",
            Self::Sedentary => "起身提醒开关和间隔",
            Self::Water => "目标、单次用量、时段和提醒",
            Self::Focus => "专注与休息时长和提醒",
            Self::Clipboard => "文字与图片记录开关",
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Bundle {
    format: String,
    version: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    mail: Option<Mail>,
    #[serde(skip_serializing_if = "Option::is_none")]
    sedentary: Option<Sedentary>,
    #[serde(skip_serializing_if = "Option::is_none")]
    water: Option<Water>,
    #[serde(skip_serializing_if = "Option::is_none")]
    focus: Option<Focus>,
    #[serde(default, skip_serializing_if = "ExtraPreferences::is_empty")]
    preferences: ExtraPreferences,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Mail {
    accounts: Vec<Mailbox>,
    interval: u32,
    show_code: bool,
    hide_code_seconds: u32,
    notice_seconds: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    enabled: Option<bool>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Mailbox {
    email: String,
    provider: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    imap: Option<Imap>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Imap {
    host: String,
    port: u16,
    username: String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Sedentary {
    enabled: bool,
    minutes: u32,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Water {
    remind: bool,
    goal_ml: u32,
    serving_ml: u32,
    minutes: u32,
    start_hour: u32,
    end_hour: u32,
    notice_seconds: Option<f64>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Focus {
    minutes: u32,
    rest_minutes: u32,
    remind: bool,
    notice_seconds: Option<f64>,
}

impl Mailbox {
    fn from_account(account: &Account) -> Self {
        Self {
            email: account.email.clone(),
            provider: account.provider.key().into(),
            imap: account.custom.as_ref().map(|imap| Imap {
                host: imap.host.clone(),
                port: imap.port,
                username: imap.username.clone(),
            }),
        }
    }

    fn to_account(&self) -> Result<Account, &'static str> {
        let provider = Provider::ALL
            .into_iter()
            .find(|p| p.key() == self.provider)
            .ok_or("配置文件包含不支持的邮箱类型")?;
        let email = self.email.trim();
        let valid_email = email.split_once('@').is_some_and(|(name, domain)| {
            !name.is_empty() && !domain.is_empty() && !domain.contains('@')
        });
        if !valid_email
            || email.len() > 320
            || email.chars().any(|c| c.is_whitespace() || c.is_control())
        {
            return Err("配置文件包含无效的邮箱地址");
        }
        if self.imap.is_some() != (provider == Provider::Custom) {
            return Err("配置文件中的 IMAP 设置与邮箱类型不匹配");
        }
        let account = Account {
            email: email.into(),
            provider,
            enabled: false,
            custom: self.imap.as_ref().map(|imap| CustomImap {
                host: imap.host.clone(),
                port: imap.port,
                username: imap.username.clone(),
            }),
        };
        account
            .endpoint()
            .map_err(|_| "配置文件包含无效的 IMAP 连接设置")?;
        Ok(account)
    }
}

impl Bundle {
    pub(crate) fn from_app(app: &App, selected: &HashSet<Section>) -> Self {
        let settings = &app.settings;
        let prefs = &settings.notice_preferences;
        Self {
            format: FORMAT.into(),
            version: VERSION,
            mail: selected.contains(&Section::Mail).then(|| Mail {
                accounts: app
                    .mail
                    .accounts
                    .iter()
                    .map(Mailbox::from_account)
                    .collect(),
                interval: settings.mail_interval,
                show_code: settings.mail_code,
                hide_code_seconds: settings.mail_code_hide_seconds,
                notice_seconds: prefs.override_seconds(NoticeKind::Mail),
                enabled: Some(settings.mail_enabled),
            }),
            sedentary: selected.contains(&Section::Sedentary).then_some(Sedentary {
                enabled: settings.sedentary_enabled,
                minutes: settings.sedentary_minutes,
            }),
            water: selected.contains(&Section::Water).then_some(Water {
                remind: app.water.remind,
                goal_ml: app.water.goal_ml,
                serving_ml: settings.serving_ml,
                minutes: settings.water_minutes,
                start_hour: settings.water_start,
                end_hour: settings.water_end,
                notice_seconds: prefs.override_seconds(NoticeKind::Water),
            }),
            focus: selected.contains(&Section::Focus).then_some(Focus {
                minutes: settings.focus_minutes,
                rest_minutes: settings.rest_minutes,
                remind: prefs.focus,
                notice_seconds: prefs.override_seconds(NoticeKind::Focus),
            }),
            preferences: ExtraPreferences::capture(app, selected),
        }
    }

    pub(crate) fn contains(&self, section: Section) -> bool {
        match section {
            Section::Mail => self.mail.is_some(),
            Section::Sedentary => self.sedentary.is_some(),
            Section::Water => self.water.is_some(),
            Section::Focus => self.focus.is_some(),
            _ => self.preferences.contains(section),
        }
    }

    pub(crate) fn merge(&mut self, incoming: Self) {
        self.version = VERSION;
        if incoming.mail.is_some() {
            self.mail = incoming.mail;
        }
        if incoming.sedentary.is_some() {
            self.sedentary = incoming.sedentary;
        }
        if incoming.water.is_some() {
            self.water = incoming.water;
        }
        if incoming.focus.is_some() {
            self.focus = incoming.focus;
        }
        self.preferences.merge(incoming.preferences);
    }

    pub(crate) fn summary(&self, section: Section, existing: &[Account]) -> Option<String> {
        let status = |enabled| if enabled { "已开启" } else { "已关闭" };
        match section {
            Section::Mail => self.mail.as_ref().map(|mail| {
                let existing: HashSet<_> = existing.iter().map(mail::key).collect();
                let added = mail
                    .accounts
                    .iter()
                    .filter(|account| {
                        !existing.contains(&account.email.trim().to_ascii_lowercase())
                    })
                    .count();
                format!(
                    "{} 个账号可新增 · {} 个同地址账号保留本机设置 · 每 {} 秒检查",
                    added,
                    mail.accounts.len() - added,
                    mail.interval
                )
            }),
            Section::Sedentary => self
                .sedentary
                .as_ref()
                .map(|v| format!("{} · 每 {} 分钟", status(v.enabled), v.minutes)),
            Section::Water => self.water.as_ref().map(|v| {
                let hours = if v.start_hour == v.end_hour {
                    "全天".into()
                } else {
                    format!("{:02}:00–{:02}:00", v.start_hour, v.end_hour)
                };
                format!(
                    "{} · 每 {} 分钟 · {} · 每次 {} ml · 目标 {} ml",
                    status(v.remind),
                    v.minutes,
                    hours,
                    v.serving_ml,
                    v.goal_ml
                )
            }),
            Section::Focus => self.focus.as_ref().map(|v| {
                format!(
                    "专注 {} 分钟 · 休息 {} 分钟 · 提醒{}",
                    v.minutes,
                    v.rest_minutes,
                    status(v.remind)
                )
            }),
            _ => self
                .contains(section)
                .then(|| section.description().to_string()),
        }
    }

    pub(crate) fn email_addresses(&self) -> impl Iterator<Item = &str> {
        self.mail
            .iter()
            .flat_map(|mail| mail.accounts.iter().map(|account| account.email.as_str()))
    }

    pub(crate) fn read(path: &Path) -> Result<Self, String> {
        let mut bytes = Vec::new();
        fs::File::open(path)
            .and_then(|file| file.take(MAX_BYTES + 1).read_to_end(&mut bytes))
            .map_err(|e| format!("无法读取配置文件（{e}）"))?;
        if bytes.len() as u64 > MAX_BYTES {
            return Err("配置文件超过 256 KB".into());
        }
        Self::decode(bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(&bytes))
    }

    pub(crate) fn decode(bytes: &[u8]) -> Result<Self, String> {
        let bytes = bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(bytes);
        #[derive(Deserialize)]
        struct Header {
            format: String,
            version: u32,
        }
        let header: Header =
            serde_json::from_slice(bytes).map_err(|_| "无法识别浮岛配置文件")?;
        if header.format != FORMAT || ![1, VERSION].contains(&header.version) {
            return Err("此文件的配置格式或版本暂不支持".into());
        }
        let bundle: Self =
            serde_json::from_slice(bytes).map_err(|_| "配置文件内容不完整或格式有误")?;
        bundle.validate()?;
        Ok(bundle)
    }

    fn validate(&self) -> Result<(), String> {
        self.preferences.validate()?;
        let valid_duration = |value: Option<f64>| {
            value.is_none_or(|seconds| {
                (model::NoticePreferences::MIN_SECONDS..=model::NoticePreferences::MAX_SECONDS)
                    .contains(&seconds)
            })
        };
        if let Some(mail) = &self.mail {
            if ![60, 120, 300].contains(&mail.interval)
                || !(15..=300).contains(&mail.hide_code_seconds)
                || !valid_duration(mail.notice_seconds)
            {
                return Err("配置文件中的邮箱偏好超出范围".into());
            }
            let mut addresses = HashSet::new();
            for mailbox in &mail.accounts {
                if !addresses.insert(mail::key(&mailbox.to_account()?)) {
                    return Err("配置文件包含重复的邮箱地址".into());
                }
            }
        }
        if self.sedentary.as_ref().is_some_and(|v| {
            !(sedentary::MIN_MINUTES..=sedentary::MAX_MINUTES).contains(&v.minutes)
        }) {
            return Err("配置文件中的久坐间隔超出范围".into());
        }
        if self.water.as_ref().is_some_and(|v| {
            !(500..=6000).contains(&v.goal_ml)
                || !(50..=1000).contains(&v.serving_ml)
                || !(5..=180).contains(&v.minutes)
                || v.start_hour > 23
                || v.end_hour > 24
                || !valid_duration(v.notice_seconds)
        }) {
            return Err("配置文件中的喝水设置超出范围".into());
        }
        if self.focus.as_ref().is_some_and(|v| {
            !(1..=180).contains(&v.minutes)
                || !(1..=60).contains(&v.rest_minutes)
                || !valid_duration(v.notice_seconds)
        }) {
            return Err("配置文件中的专注设置超出范围".into());
        }
        if self.preferences.is_empty()
            && self.mail.is_none()
            && self.water.is_none()
            && self.sedentary.is_none()
            && self.focus.is_none()
        {
            return Err("配置文件没有可导入的项目".into());
        }
        Ok(())
    }

    pub(crate) fn encode(&self) -> Result<Vec<u8>, String> {
        self.validate()?;
        let bytes = serde_json::to_vec_pretty(self).map_err(|_| "无法生成配置文件")?;
        if bytes.len() as u64 > MAX_BYTES {
            return Err("导出内容超过 256 KB".into());
        }
        Ok(bytes)
    }

    pub(crate) fn write(&self, path: &Path) -> Result<(), String> {
        let bytes = self.encode()?;
        match fs::symlink_metadata(path) {
            Ok(meta) if !meta.is_file() || Self::read(path).is_err() => {
                return Err("只能覆盖已有的浮岛配置文件，请选择新的文件名".into());
            }
            Ok(_) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(format!("无法检查保存位置（{e}）")),
        }
        model::atomic_write(path, &bytes).map_err(|e| format!("无法保存配置文件（{e}）"))
    }

    pub(crate) fn apply(
        &self,
        app: &mut App,
        selected: &HashSet<Section>,
    ) -> Result<usize, String> {
        let root = model::config_dir();
        let mut settings = app.settings.clone();
        let mut water = app.water.clone();
        let mut edits = Vec::new();
        let mut changes = Vec::new();
        let mut added = Vec::new();
        let previous_integrations = &app.integrations.config;
        let mut integrations = previous_integrations.clone();
        let applied =
            self.preferences
                .apply(&mut settings, &mut integrations, selected, &mut changes);

        if let Some(mail) = self
            .mail
            .as_ref()
            .filter(|_| selected.contains(&Section::Mail))
        {
            let mut file = Edit::read(root.join("mail-accounts.json"))?;
            let mut accounts: Vec<serde_json::Value> = match file.before.as_ref() {
                Some(bytes) => {
                    serde_json::from_slice(bytes).map_err(|_| "本机邮箱清单无法读取，导入已停止")?
                }
                None => Vec::new(),
            };
            let mut seen: HashSet<_> = app.mail.accounts.iter().map(mail::key).collect();
            for item in &accounts {
                let address = item
                    .get("email")
                    .and_then(serde_json::Value::as_str)
                    .filter(|address| !address.trim().is_empty())
                    .ok_or("本机邮箱清单内容有误，导入已停止")?;
                seen.insert(address.trim().to_ascii_lowercase());
            }
            for entry in &mail.accounts {
                let account = entry.to_account()?;
                if seen.insert(mail::key(&account)) {
                    added.push(account);
                }
            }
            if !added.is_empty() {
                let new_entries: Vec<_> = serde_json::from_slice(&mail::encode_accounts(&added))
                    .map_err(|_| "无法写入导入的邮箱配置")?;
                accounts.extend(new_entries);
                file.after = serde_json::to_vec(&accounts).map_err(|_| "无法写入本机邮箱清单")?;
                edits.push(file);
            }
            settings.mail_interval = mail.interval;
            settings.mail_code = mail.show_code;
            settings.mail_code_hide_seconds = mail.hide_code_seconds;
            settings
                .notice_preferences
                .replace_seconds(NoticeKind::Mail, mail.notice_seconds);
            changes.extend([
                ("mail_interval", mail.interval.to_string()),
                ("mail_code", flag(mail.show_code)),
                ("mail_code_hide_seconds", mail.hide_code_seconds.to_string()),
            ]);
            if let Some(enabled) = mail.enabled {
                settings.mail_enabled = enabled;
                changes.push(("mail_enabled", flag(enabled)));
            }
        }
        if let Some(v) = self
            .sedentary
            .as_ref()
            .filter(|_| selected.contains(&Section::Sedentary))
        {
            settings.sedentary_enabled = v.enabled;
            settings.sedentary_minutes = v.minutes;
            changes.extend([
                ("sedentary_enabled", flag(v.enabled)),
                ("sedentary_minutes", v.minutes.to_string()),
            ]);
        }
        let water_selected = self
            .water
            .as_ref()
            .filter(|_| selected.contains(&Section::Water));
        if let Some(v) = water_selected {
            settings.water_minutes = v.minutes;
            settings.serving_ml = v.serving_ml;
            settings.water_start = v.start_hour;
            settings.water_end = v.end_hour;
            settings
                .notice_preferences
                .replace_seconds(NoticeKind::Water, v.notice_seconds);
            water.goal_ml = v.goal_ml;
            water.remind = v.remind;
            changes.extend([
                ("water_minutes", v.minutes.to_string()),
                ("serving_ml", v.serving_ml.to_string()),
                ("water_start", v.start_hour.to_string()),
                ("water_end", v.end_hour.to_string()),
            ]);
            edits.push(Edit::settings(
                &root,
                "water.txt",
                &[("goal", v.goal_ml.to_string())],
            )?);
            edits.push(Edit::settings(
                &root,
                "water-extra.txt",
                &[("remind", flag(v.remind))],
            )?);
        }
        let focus_selected = self
            .focus
            .as_ref()
            .filter(|_| selected.contains(&Section::Focus));
        if let Some(v) = focus_selected {
            settings.focus_minutes = v.minutes;
            settings.rest_minutes = v.rest_minutes;
            settings.notice_preferences.focus = v.remind;
            settings
                .notice_preferences
                .replace_seconds(NoticeKind::Focus, v.notice_seconds);
            changes.extend([
                ("focus_minutes", v.minutes.to_string()),
                ("rest_minutes", v.rest_minutes.to_string()),
            ]);
        }
        if changes.is_empty() && !applied {
            return Err("请选择至少一项要导入的配置".into());
        }
        if settings.notice_preferences != app.settings.notice_preferences {
            changes.push((
                "notice_preferences",
                serde_json::to_string(&settings.notice_preferences)
                    .map_err(|_| "无法保存提醒偏好")?,
            ));
        }
        if !changes.is_empty() {
            edits.push(Edit::settings(&root, "settings.txt", &changes)?);
        }
        let clipboard_changed = integrations.clipboard != previous_integrations.clipboard
            || integrations.images != previous_integrations.images;
        if &integrations != previous_integrations {
            let mut edit = Edit::read(root.join("integrations.json"))?;
            let mut data: serde_json::Map<String, serde_json::Value> = match edit.before.as_ref() {
                Some(bytes) => serde_json::from_slice(bytes).map_err(|_| "本机接入偏好无法读取")?,
                None => serde_json::Map::new(),
            };
            data.extend(integrations.fields());
            edit.after = serde_json::to_vec(&data).map_err(|_| "无法写入本机接入偏好")?;
            edits.push(edit);
        }
        commit(edits)?;

        app.settings = settings;
        app.show_lyrics = app.settings.lyrics_enabled;
        if !app.settings.visible_pages[app.page] {
            if let Some(page) = app
                .settings
                .page_order
                .iter()
                .copied()
                .find(|page| app.settings.visible_pages[*page])
            {
                app.go(page, 1.0);
            }
        }
        if clipboard_changed {
            app.pending.push(crate::app::Cmd::Clipboard(
                crate::clipboard_history::Command::Configure {
                    enabled: integrations.clipboard,
                    images: integrations.images,
                },
            ));
        }
        app.integrations.config = integrations;
        if water_selected.is_some() {
            app.water = water;
            app.last_reminder = app.t;
        }
        if selected.contains(&Section::Sedentary) {
            app.sedentary = Default::default();
        }
        let count = added.len();
        app.mail.accounts.extend(added);
        Ok(count)
    }
}

fn flag(value: bool) -> String {
    u8::from(value).to_string()
}

struct Edit {
    path: PathBuf,
    before: Option<Vec<u8>>,
    after: Vec<u8>,
}

impl Edit {
    fn read(path: PathBuf) -> Result<Self, String> {
        let before = match fs::read(&path) {
            Ok(bytes) => Some(bytes),
            Err(e) if e.kind() == io::ErrorKind::NotFound => None,
            Err(e) => return Err(format!("无法读取本机配置（{e}）")),
        };
        Ok(Self {
            path,
            before,
            after: Vec::new(),
        })
    }

    fn settings(root: &Path, name: &str, pairs: &[(&str, String)]) -> Result<Self, String> {
        let mut edit = Self::read(root.join(name))?;
        let original = std::str::from_utf8(edit.before.as_deref().unwrap_or_default())
            .map_err(|_| "本机配置编码有误，导入已停止")?;
        let mut body = String::new();
        let bom = '\u{feff}';
        if original.starts_with(bom) {
            body.push(bom);
        }
        for line in original.trim_start_matches(bom).split_inclusive('\n') {
            let key = line.split_once('=').map(|(key, _)| key.trim());
            if !pairs.iter().any(|(name, _)| Some(*name) == key) {
                body.push_str(line);
            }
        }
        if !body.is_empty() && !body.ends_with('\n') && body != bom.to_string() {
            body.push('\n');
        }
        for (key, value) in pairs {
            body.push_str(&format!("{key}={value}\n"));
        }
        edit.after = body.into_bytes();
        Ok(edit)
    }

    fn restore(&self) -> io::Result<()> {
        match &self.before {
            Some(bytes) => model::atomic_write(&self.path, bytes),
            None => fs::remove_file(&self.path),
        }
    }
}

fn commit(edits: Vec<Edit>) -> Result<(), String> {
    let mut written: Vec<&Edit> = Vec::new();
    for edit in &edits {
        if edit.before.as_deref() == Some(edit.after.as_slice()) {
            continue;
        }
        let write = fs::create_dir_all(edit.path.parent().ok_or("本机配置目录无效")?)
            .and_then(|_| model::atomic_write(&edit.path, &edit.after));
        if let Err(error) = write {
            let mut restore_failed = false;
            for prior in written.into_iter().rev() {
                restore_failed |= prior.restore().is_err();
            }
            return Err(if restore_failed {
                format!("导入写入失败，部分本机配置无法恢复（{error}）")
            } else {
                format!("导入写入失败，已恢复本机配置（{error}）")
            });
        }
        written.push(edit);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_sync_rolls_back_after_a_later_write_fails() {
        let temp =
            std::env::temp_dir().join(format!("leidao-sync-rollback-{}", std::process::id()));
        fs::create_dir(&temp).unwrap();
        let good = temp.join("settings.txt");
        let blocked = temp.join("blocked");
        fs::write(&good, b"before").unwrap();
        fs::create_dir(&blocked).unwrap();
        let failure = commit(vec![
            Edit {
                path: good.clone(),
                before: Some(b"before".to_vec()),
                after: b"changed".to_vec(),
            },
            Edit {
                path: blocked.clone(),
                before: None,
                after: b"changed".to_vec(),
            },
        ]);
        assert!(failure.unwrap_err().contains("已恢复"));
        assert_eq!(fs::read(&good).unwrap(), b"before");
        fs::remove_file(good).unwrap();
        fs::remove_dir(blocked).unwrap();
        fs::remove_dir(temp).unwrap();
    }
}
