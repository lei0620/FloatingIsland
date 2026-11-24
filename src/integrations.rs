//! Local integrations share the existing island queue and a small in-memory inbox.
use crate::{
    app::{App, Cmd},
    codex_tasks::{self, Phase},
    messengers::{self, Source},
};
use eframe::egui;
use serde_json::json;
use std::{
    collections::{HashMap, HashSet, VecDeque},
    sync::mpsc::{self, Receiver},
    time::{Duration, Instant},
};

const DEFAULT_CODEX_NOTICE_SECONDS: u32 = 8;
pub(crate) const MIN_CODEX_NOTICE_SECONDS: u32 = 3;
pub(crate) const MAX_CODEX_NOTICE_SECONDS: u32 = 120;

#[derive(Clone, Debug, PartialEq)]
pub struct Config {
    pub codex: bool,
    pub codex_notice_seconds: u32,
    pub wechat: bool,
    pub wecom: bool,
    pub clipboard: bool,
    pub images: bool,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            codex: true,
            codex_notice_seconds: DEFAULT_CODEX_NOTICE_SECONDS,
            wechat: true,
            wecom: true,
            clipboard: true,
            images: true,
        }
    }
}
impl Config {
    pub fn load() -> Self {
        let mut c = Self::default();
        if let Ok(bytes) = std::fs::read(crate::model::config_dir().join("integrations.json")) {
            if let Ok(v) = serde_json::from_slice::<serde_json::Value>(&bytes) {
                for (key, slot) in [
                    ("codex", &mut c.codex),
                    ("wechat", &mut c.wechat),
                    ("wecom", &mut c.wecom),
                    ("clipboard", &mut c.clipboard),
                    ("images", &mut c.images),
                ] {
                    if let Some(value) = v[key].as_bool() {
                        *slot = value;
                    }
                }
                if let Some(seconds) = v["codex_notice_seconds"].as_u64() {
                    c.codex_notice_seconds = seconds.clamp(
                        u64::from(MIN_CODEX_NOTICE_SECONDS),
                        u64::from(MAX_CODEX_NOTICE_SECONDS),
                    ) as u32;
                }
            }
        }
        c
    }
    pub fn save(&self) -> Result<(), String> {
        let dir = crate::model::config_dir();
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let value = serde_json::Value::Object(self.fields());
        crate::model::atomic_write(&dir.join("integrations.json"), value.to_string().as_bytes())
            .map_err(|e| e.to_string())
    }

    pub(crate) fn fields(&self) -> serde_json::Map<String, serde_json::Value> {
        let serde_json::Value::Object(fields) = json!({
            "codex": self.codex,
            "codex_notice_seconds": self.codex_notice_seconds,
            "wechat": self.wechat,
            "wecom": self.wecom,
            "clipboard": self.clipboard,
            "images": self.images,
        }) else { unreachable!("integration preferences are an object") };
        fields
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Action {
    OpenCodex(String),
    OpenMessenger(Source),
}

impl Action {
    pub(crate) fn command(&self) -> Cmd {
        match self {
            Self::OpenCodex(session) => Cmd::OpenCodexThread(session.clone()),
            Self::OpenMessenger(source) => Cmd::ActivateMessenger(*source),
        }
    }
}

#[derive(Clone)]
pub struct Record {
    pub title: String,
    pub body: String,
    pub(crate) action: Option<Action>,
    pub at: u64,
}
pub struct State {
    pub diagnostics: Diagnostics,
    pub config: Config,
    pub tasks: codex_tasks::Tracker,
    codex_unread: HashSet<String>,
    codex_waiting: VecDeque<codex_tasks::Event>,
    pub records: VecDeque<Record>,
    pub signal_status: String,
    pub hook_status: String,
    pub deduper: messengers::Deduper,
    pub clipboard: crate::clipboard_history::Snapshot,
    pub query: String,
    pub favorites: bool,
    pub confirm_clear: bool,
    pub confirm_delete: Option<String>,
    pub clipboard_note: String,
    pub textures: HashMap<String, egui::TextureHandle>,
}
impl Default for State {
    fn default() -> Self {
        Self {
            diagnostics: Default::default(),
            config: Config::load(),
            tasks: Default::default(),
            codex_unread: Default::default(),
            codex_waiting: Default::default(),
            records: Default::default(),
            signal_status: "正在启动任务栏监听…".into(),
            hook_status: String::new(),
            deduper: Default::default(),
            clipboard: Default::default(),
            query: String::new(),
            favorites: false,
            confirm_clear: false,
            confirm_delete: None,
            clipboard_note: String::new(),
            textures: Default::default(),
        }
    }
}

#[derive(Default)]
pub struct Diagnostics {
    pub task_events: u64,
    pub task_completions: u64,
    pub last_task: Option<(String, String, String, u64)>,
    pub wechat_attention: u64,
    pub wecom_attention: u64,
    pub wechat_notifications: u64,
    pub wecom_notifications: u64,
}

pub enum Event {
    Task(codex_tasks::Event),
    CodexUnread(HashSet<String>),
    Messenger(messengers::Signal),
}
pub struct Service {
    rx: Receiver<Event>,
}
impl Service {
    pub fn start(ctx: egui::Context) -> Self {
        let (tx, rx) = mpsc::channel();
        let dir = codex_tasks::event_dir();
        std::thread::spawn(move || {
            #[cfg(windows)]
            let signals = messengers::start();
            let codex_home = codex_tasks::codex_home();
            let mut unread_state = codex_tasks::UnreadState::default();
            let mut unread_clock = crate::poll_clock::PollClock::default();
            let mut previous_unread = HashSet::new();
            loop {
                let mut changed = false;
                if unread_clock.ready(Duration::from_millis(750)) {
                    unread_clock = crate::poll_clock::PollClock::now();
                    let unread = unread_state.poll(&codex_home);
                    if unread != previous_unread {
                        previous_unread = unread.clone();
                        if tx.send(Event::CodexUnread(unread)).is_err() { return; }
                        changed = true;
                    }
                }
                for event in codex_tasks::drain(&dir) {
                    if tx.send(Event::Task(event)).is_err() {
                        return;
                    }
                    changed = true;
                }
                #[cfg(windows)]
                while let Ok(signal) = signals.try_recv() {
                    if matches!(&signal,messengers::Signal::Status(s)if s.is_empty()) {
                        continue;
                    }
                    if tx.send(Event::Messenger(signal)).is_err() {
                        return;
                    }
                    changed = true;
                }
                if changed {
                    ctx.request_repaint();
                }
                // This heartbeat detects shutdown without keeping GUI state in the worker.
                if tx
                    .send(Event::Messenger(messengers::Signal::Status(String::new())))
                    .is_err()
                {
                    return;
                }
                std::thread::sleep(Duration::from_millis(250));
            }
        });
        Self { rx }
    }
    pub fn try_recv(&self) -> Option<Event> {
        self.rx.try_recv().ok()
    }
}

fn fresh_codex_notice(event: &codex_tasks::Event, now: u64) -> bool {
    // The Hook can precede the desktop client's unread-state write.
    now.saturating_sub(event.at) <= 20_000
}

impl App {
    /// Opt-in operational evidence, without chat, prompt, clipboard or account contents.
    pub fn write_integration_diagnostics(&self, path: &std::path::Path) -> Result<(), String> {
        let d = &self.integrations.diagnostics;
        let last=d.last_task.as_ref().map(|(session,turn,phase,at)|json!({"session":session,"turn":turn,"phase":phase,"at":at}));
        let value = json!({"pid":std::process::id(),"written_at":codex_tasks::now_ms(),"notifications_enabled":self.settings.notifications,
            "windows_notifications_requested":self.settings.read_notifications,"windows_status":self.listener_status,"taskbar_status":self.integrations.signal_status,
            "task_events":d.task_events,"task_completions":d.task_completions,"last_task":last,
            "wechat_attention":d.wechat_attention,"wecom_attention":d.wecom_attention,"wechat_notifications":d.wechat_notifications,"wecom_notifications":d.wecom_notifications,
            "queued_alerts":self.alerts.len(),"current_alert":self.notice.as_ref().map(|a|a.title.as_str()),"running_tasks":self.integrations.tasks.running()});
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        crate::model::atomic_write(
            path,
            serde_json::to_string_pretty(&value).unwrap().as_bytes(),
        )
        .map_err(|e| e.to_string())
    }
    pub fn integration_event(&mut self, event: Event) {
        match event {
            Event::CodexUnread(unread) => self.update_codex_unread(unread),
            Event::Task(event) => {
                let now = codex_tasks::now_ms();
                if !self
                    .integrations
                    .tasks
                    .accept(&event, now)
                    || !self.integrations.config.codex
                {
                    return;
                }
                self.integrations.diagnostics.task_events += 1;
                let phase = match event.phase {
                    Phase::Running => "running",
                    Phase::Completed => "completed",
                    Phase::Interrupted => "interrupted",
                    Phase::Attention => "attention",
                };
                if event.phase == Phase::Completed {
                    self.integrations.diagnostics.task_completions += 1;
                }
                self.integrations.diagnostics.last_task = Some((
                    event.session.clone(),
                    event.turn.clone(),
                    phase.into(),
                    event.at,
                ));
                if event.phase == Phase::Running || !fresh_codex_notice(&event, now)
                    || codex_tasks::thread_url(&event.session).is_none()
                {
                    return;
                }
                self.integrations.codex_waiting.retain(|old| {
                    !old.session.eq_ignore_ascii_case(&event.session) && fresh_codex_notice(old, now)
                });
                if self.integrations.codex_unread.contains(&event.session.to_ascii_lowercase()) {
                    self.show_codex_task(event);
                } else {
                    self.integrations.codex_waiting.push_back(event);
                    if self.integrations.codex_waiting.len() > 50 {
                        self.integrations.codex_waiting.pop_front();
                    }
                }
            }
            Event::Messenger(messengers::Signal::Status(status)) => {
                if !status.is_empty() {
                    self.integrations.signal_status = status;
                }
            }
            Event::Messenger(messengers::Signal::Activated(source)) => {
                self.integrations.deduper.activated(source);
                let matches_source = |a: &crate::extras::Alert| a.action == Some(Action::OpenMessenger(source));
                self.alerts.retain(|a| !matches_source(a));
                if self.notice.as_ref().is_some_and(matches_source) {
                    self.notice = None;
                }
                if self.hud_notice.as_ref().is_some_and(matches_source) {
                    self.hud_notice = None;
                }
                if self.notification_bell_messenger == Some(source) && (source != Source::WeChat || !self.wechat_listener_active) {
                    self.notification_bell_messenger = None;
                    self.notification_bell_until = 0.0;
                }
            }
            Event::Messenger(messengers::Signal::Attention(source)) => {
                if self.legacy_messenger_alerts_enabled(source)
                    && self.integrations.deduper.attention(source, Instant::now())
                {
                    match source {
                        Source::WeChat => self.integrations.diagnostics.wechat_attention += 1,
                        Source::WeCom => self.integrations.diagnostics.wecom_attention += 1,
                    }
                    self.integration_notice(
                        source.label().into(),
                        "有新提醒 · 点击打开客户端".into(),
                        Some(Action::OpenMessenger(source)),
                        false,
                    );
                }
            }
        }
    }
    fn show_codex_task(&mut self, event: codex_tasks::Event) {
        let title = match event.phase {
            Phase::Running => return,
            Phase::Completed => "Codex 任务完成",
            Phase::Interrupted => "Codex 任务已中断",
            Phase::Attention => "Codex 需要确认",
        };
        let body = if event.workspace.is_empty() { "点击查看任务提醒".into() } else { event.workspace };
        self.clear_codex_alerts(|id| id.eq_ignore_ascii_case(&event.session));
        self.integration_notice(title.into(), body, Some(Action::OpenCodex(event.session)),
            event.phase != Phase::Interrupted);
    }

    fn update_codex_unread(&mut self, unread: HashSet<String>) {
        self.clear_codex_alerts(|id| !unread.contains(&id.to_ascii_lowercase()));
        self.integrations.codex_unread = unread;
        let waiting = std::mem::take(&mut self.integrations.codex_waiting);
        if !self.integrations.config.codex { return; }
        let now = codex_tasks::now_ms();
        for event in waiting.into_iter().filter(|event| fresh_codex_notice(event, now)) {
            if self.integrations.codex_unread.contains(&event.session.to_ascii_lowercase()) {
                self.show_codex_task(event);
            } else {
                self.integrations.codex_waiting.push_back(event);
            }
        }
    }

    fn clear_codex_alerts(&mut self, handled: impl Fn(&str) -> bool) {
        let clear = |alert: &crate::extras::Alert| {
            matches!(&alert.action, Some(Action::OpenCodex(id)) if handled(id))
        };
        self.alerts.retain(|alert| !clear(alert));
        if self.notice.as_ref().is_some_and(clear) {
            self.dismiss_notice();
        }
        if self.hud_notice.as_ref().is_some_and(clear) {
            self.hud_notice = None;
        }
        if self.notification_bell_codex.as_deref().is_some_and(handled) {
            self.notification_bell_codex = None;
            self.notification_bell_until = 0.0;
        }
    }

    pub fn acknowledge_codex_notice(&mut self, session: &str) {
        self.integrations.codex_waiting.retain(|event| !event.session.eq_ignore_ascii_case(session));
        self.clear_codex_alerts(|id| id.eq_ignore_ascii_case(session));
    }
    pub(crate) fn messenger_alerts_enabled(&self, source: Source) -> bool {
        self.settings.notifications && match source {
            Source::WeChat => self.settings.wechat_notifications,
            Source::WeCom => self.integrations.config.wecom,
        }
    }

    pub(crate) fn legacy_messenger_alerts_enabled(&self, source: Source) -> bool {
        self.messenger_alerts_enabled(source) && match source {
            Source::WeChat => self.integrations.config.wechat && !self.wechat_listener_active,
            Source::WeCom => !self.settings.wecom_notifications || !self.wecom_listener_active,
        }
    }

    pub fn chat_notification(&mut self, source: Source, body: String) {
        if !self.legacy_messenger_alerts_enabled(source)
            || !self
                .integrations
                .deduper
                .content(source, &body, Instant::now())
        {
            return;
        }
        match source {
            Source::WeChat => self.integrations.diagnostics.wechat_notifications += 1,
            Source::WeCom => self.integrations.diagnostics.wecom_notifications += 1,
        }
        // Actual Windows content replaces an earlier taskbar hint from the same source.
        let matches_source = |a: &crate::extras::Alert| a.action == Some(Action::OpenMessenger(source));
        self.alerts.retain(|a| !matches_source(a));
        if self.notice.as_ref().is_some_and(matches_source) {
            self.notice = None;
        }
        let body: String = body
            .chars()
            .filter(|c| !c.is_control() || *c == '\n')
            .take(2000)
            .collect();
        self.integration_notice(source.label().into(), body, Some(Action::OpenMessenger(source)), false);
    }
    fn integration_notice(
        &mut self,
        title: String,
        body: String,
        action: Option<Action>,
        system: bool,
    ) {
        self.integrations.records.push_front(Record {
            title: title.clone(),
            body: body.clone(),
            action: action.clone(),
            at: codex_tasks::now_ms(),
        });
        self.integrations.records.truncate(50);
        let codex_duration = match &action {
            Some(Action::OpenCodex(_)) => Some(f64::from(self.integrations.config.codex_notice_seconds)),
            _ => None,
        };
        if self.settings.notifications {
            self.notification_bell_codex = match &action {
                Some(Action::OpenCodex(session)) => Some(session.clone()),
                _ => None,
            };
            self.notification_bell_messenger = match &action {
                Some(Action::OpenMessenger(source)) => Some(*source),
                _ => None,
            };
            self.notification_bell_at = self.t;
            let messenger_duration = self.notification_bell_messenger.and_then(|source| {
                let kind = match source {
                    Source::WeChat => crate::model::NoticeKind::Wechat,
                    Source::WeCom => crate::model::NoticeKind::Wecom,
                };
                self.settings.notice_preferences.override_seconds(kind)
            });
            self.notification_bell_until = self.t + codex_duration.or(messenger_duration).unwrap_or(5.0);
        }
        self.alert_with_action(title, body, 12, system, action, codex_duration);
    }

    pub(crate) fn open_codex_bell(&mut self) -> bool {
        let Some(session) = self.notification_bell_codex.take() else { return false; };
        self.pending.push(Cmd::OpenCodexThread(session));
        self.notification_bell_until = 0.0;
        true
    }

    pub fn clipboard_snapshot(&mut self, snapshot: crate::clipboard_history::Snapshot) {
        self.integrations
            .textures
            .retain(|id, _| snapshot.entries.iter().any(|e| e.id == *id));
        if !snapshot.error.is_empty() {
            self.integrations.clipboard_note = snapshot.error.clone();
        } else if !snapshot.notice.is_empty() {
            self.integrations.clipboard_note = snapshot.notice.clone();
            self.say(&snapshot.notice);
        }
        self.integrations.clipboard = snapshot;
    }
    pub fn open_integrations(&mut self, clipboard: bool) {
        self.settings_tab = if clipboard { 13 } else { 12 };
        self.open_settings();
    }

    pub(crate) fn save_integration_settings(&mut self, before: Config) {
        if before != self.integrations.config {
            if let Err(error) = self.integrations.config.save() {
                self.integrations.config = before;
                self.say(error);
            }
        }
    }
    pub(crate) fn integration_settings(&mut self, ui: &mut egui::Ui) {
        use crate::extras::{settings_card, settings_hint, settings_label};
        self.notification_settings_link(ui);
        settings_label(ui, "Codex 接入");
        settings_card(ui, |ui| {
            ui.label(format!("正在执行或等待确认：{} 项", self.integrations.tasks.running()));
            if ui.button("安装 / 修复 Codex 接入").clicked() {
                self.pending.push(Cmd::InstallCodexHooks);
            }
            settings_hint(ui, "首次接入后重启 Codex，并审阅信任新增 Hook。原有通知配置会保留。");
            if !self.integrations.hook_status.is_empty() {
                settings_hint(ui, &self.integrations.hook_status);
            }
        });
        settings_label(ui, "消息客户端");
        settings_card(ui, |ui| {
            if !self.integrations.signal_status.is_empty() {
                settings_hint(ui, &self.integrations.signal_status);
            }
            ui.horizontal_wrapped(|ui| {
                for source in [Source::WeChat, Source::WeCom] {
                    if ui.button(format!("打开{}", source.label())).clicked() {
                        self.pending.push(Cmd::ActivateMessenger(source));
                    }
                }
            });
        });
        settings_label(ui, "本次运行的提醒");
        if self.integrations.records.is_empty() {
            ui.label("暂时没有提醒。任务完成或收到消息后，会显示在这里。");
        }
        for record in self
            .integrations
            .records
            .iter()
            .cloned()
            .collect::<Vec<_>>()
        {
            settings_card(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.strong(&record.title);
                    ui.small(
                        chrono::DateTime::from_timestamp_millis(record.at as i64)
                            .map(|d| d.with_timezone(&chrono::Local).format("%H:%M").to_string())
                            .unwrap_or_default(),
                    );
                });
                ui.label(&record.body);
                if let Some(action) = &record.action {
                    let label = match action {
                        Action::OpenCodex(_) => "打开会话",
                        Action::OpenMessenger(_) => "打开客户端",
                    };
                    if ui.button(label).clicked() {
                        self.pending.push(action.command());
                    }
                }
            });
            ui.add_space(5.0);
        }
    }
    pub(crate) fn clipboard_settings(&mut self, ui: &mut egui::Ui) {
        use crate::clipboard_history::{Command, Content};
        use crate::extras::{setting_row, settings_card, settings_label};
        let before = self.integrations.config.clone();
        settings_label(ui, "记录设置");
        settings_card(ui, |ui| {
            setting_row(
                ui,
                "记录剪贴板",
                "保存本机文本与图片；暂停后保留已有记录",
                &mut self.integrations.config.clipboard,
            );
            ui.separator();
            setting_row(
                ui,
                "记录图片",
                "最多 100 项、共 64 MB，单张图片最多 16 MB",
                &mut self.integrations.config.images,
            );
            ui.small("剪贴板历史保存在本机，可能包含验证码等私人内容，可随时暂停或清空。");
        });
        if before != self.integrations.config {
            if let Err(e) = self.integrations.config.save() {
                self.say(e);
            }
            self.pending.push(Cmd::Clipboard(Command::Configure {
                enabled: self.integrations.config.clipboard,
                images: self.integrations.config.images,
            }));
        }
        settings_label(ui, "历史记录");
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.integrations.query)
                    .hint_text("搜索文本或图片")
                    .desired_width((ui.available_width() - 86.0).max(100.0)),
            );
            ui.checkbox(&mut self.integrations.favorites, "只看收藏");
        });
        ui.horizontal_wrapped(|ui| {
            ui.small(format!(
                "{} 项 · {}",
                self.integrations.clipboard.entries.len(),
                if self.integrations.clipboard.enabled {
                    "正在记录"
                } else {
                    "已暂停"
                }
            ));
            if ui
                .button(if self.integrations.confirm_clear {
                    "确认清空全部"
                } else {
                    "清空历史"
                })
                .clicked()
            {
                if self.integrations.confirm_clear {
                    self.pending.push(Cmd::Clipboard(Command::Clear));
                    self.integrations.confirm_clear = false;
                } else {
                    self.integrations.confirm_clear = true;
                }
            }
            if self.integrations.confirm_clear && ui.button("取消").clicked() {
                self.integrations.confirm_clear = false;
            }
        });
        if !self.integrations.clipboard_note.is_empty() {
            ui.label(&self.integrations.clipboard_note);
        }
        let entries: Vec<_> = self
            .integrations
            .clipboard
            .entries
            .iter()
            .filter(|e| {
                (!self.integrations.favorites || e.favorite) && e.matches(&self.integrations.query)
            })
            .cloned()
            .collect();
        if entries.is_empty() {
            ui.label(if self.integrations.query.is_empty() {
                "还没有记录，复制文本或图片后会出现在这里。"
            } else {
                "没有找到匹配的记录。"
            });
        }
        for entry in entries {
            ui.push_id(&entry.id, |ui| {
                settings_card(ui, |ui| {
                    match &entry.content {
                        Content::Text(text) => {
                            let preview: String = text.chars().take(260).collect();
                            ui.label(preview);
                        }
                        Content::Image { .. } => {
                            if !self.integrations.textures.contains_key(&entry.id) {
                                let path = crate::clipboard_history::directory()
                                    .join("images")
                                    .join(format!("{}-thumb.png", entry.id));
                                if let Ok(image) = image::open(path) {
                                    let rgba = image.into_rgba8();
                                    let color = egui::ColorImage::from_rgba_unmultiplied(
                                        [rgba.width() as usize, rgba.height() as usize],
                                        &rgba,
                                    );
                                    self.integrations.textures.insert(
                                        entry.id.clone(),
                                        ui.ctx().load_texture(
                                            &entry.id,
                                            color,
                                            egui::TextureOptions::LINEAR,
                                        ),
                                    );
                                }
                            }
                            if let Some(texture) = self.integrations.textures.get(&entry.id) {
                                ui.image((texture.id(), texture.size_vec2()));
                            }
                            ui.small(entry.summary());
                        }
                    }
                    ui.horizontal_wrapped(|ui| {
                        if ui.button("复制").clicked() {
                            self.pending
                                .push(Cmd::Clipboard(Command::Copy(entry.id.clone())));
                            self.integrations.clipboard_note.clear();
                        }
                        if ui
                            .selectable_label(
                                entry.favorite,
                                if entry.favorite {
                                    "已收藏"
                                } else {
                                    "收藏"
                                },
                            )
                            .clicked()
                        {
                            self.pending
                                .push(Cmd::Clipboard(Command::Favorite(entry.id.clone())));
                        }
                        let confirming =
                            self.integrations.confirm_delete.as_deref() == Some(&entry.id);
                        if ui
                            .button(if confirming { "确认删除" } else { "删除" })
                            .clicked()
                        {
                            if confirming {
                                self.pending
                                    .push(Cmd::Clipboard(Command::Delete(entry.id.clone())));
                                self.integrations.confirm_delete = None;
                            } else {
                                self.integrations.confirm_delete = Some(entry.id.clone());
                            }
                        }
                        if confirming && ui.button("取消").clicked() {
                            self.integrations.confirm_delete = None;
                        }
                    });
                })
            });
            ui.add_space(6.0);
        }
    }
}
