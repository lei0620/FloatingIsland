use crate::app::{App, Cmd};
use crate::config_sync::{Bundle, Section};
use crate::config_webdav::{Client, Connection};
use crate::extras::{setting_row, settings_card, settings_hint, settings_label};
use eframe::egui;
use std::{collections::HashSet, path::Path, sync::mpsc};

type WebResult = Result<WebEvent, String>;

enum WebEvent {
    Connected(Connection),
    Uploaded,
    Downloaded(Box<Bundle>),
}

#[derive(Default)]
struct WebDavView {
    draft: Connection,
    saved: Option<Connection>,
    password: String,
    pending: Option<mpsc::Receiver<WebResult>>,
}

impl WebDavView {
    fn load() -> Self {
        let saved = Connection::load();
        Self {
            draft: saved.clone().unwrap_or_default(),
            saved,
            ..Default::default()
        }
    }

    fn ready(&self) -> bool {
        self.saved.as_ref() == Some(&self.draft) && self.password.is_empty()
    }
}

pub(crate) struct SyncView {
    export: HashSet<Section>,
    scope_loaded: bool,
    preview: Option<Bundle>,
    import: HashSet<Section>,
    feedback: Option<(bool, String)>,
    webdav: Option<WebDavView>,
}

impl Default for SyncView {
    fn default() -> Self {
        Self {
            export: Section::ALL.into_iter().collect(),
            scope_loaded: false,
            preview: None,
            import: HashSet::new(),
            feedback: None,
            webdav: None,
        }
    }
}

fn include(
    ui: &mut egui::Ui,
    chosen: &mut HashSet<Section>,
    section: Section,
    description: &str,
) -> bool {
    let was_enabled = chosen.contains(&section);
    let mut enabled = was_enabled;
    setting_row(ui, section.title(), description, &mut enabled);
    if enabled != was_enabled {
        if enabled {
            chosen.insert(section);
        } else {
            chosen.remove(&section);
        }
    }
    enabled != was_enabled
}

fn scope_path() -> std::path::PathBuf {
    crate::model::config_dir().join("config-sync-scope.json")
}

fn save_scope(selected: &HashSet<Section>) -> Result<(), String> {
    let path = scope_path();
    let ordered: Vec<_> = Section::ALL
        .into_iter()
        .filter(|section| selected.contains(section))
        .collect();
    let bytes = serde_json::to_vec(&ordered).map_err(|_| "无法生成同步范围")?;
    std::fs::create_dir_all(crate::model::config_dir())
        .and_then(|_| crate::model::atomic_write(&path, &bytes))
        .map_err(|_| "无法保存同步范围，下次启动请重新选择".into())
}

fn input(ui: &mut egui::Ui, label: &str, value: &mut String, hint: &str, password: bool) {
    ui.label(label);
    ui.add(
        egui::TextEdit::singleline(value)
            .desired_width(ui.available_width())
            .hint_text(hint)
            .password(password),
    );
}

impl App {
    pub(crate) fn config_sync_settings(&mut self, ui: &mut egui::Ui) {
        let view = &mut self.config_sync;
        if !view.scope_loaded {
            view.scope_loaded = true;
            if let Ok(bytes) = std::fs::read(scope_path()) {
                if let Ok(selected) = serde_json::from_slice(&bytes) {
                    view.export = selected;
                }
            }
        }
        if let Some((success, message)) = &view.feedback {
            let color = if *success {
                ui.visuals().text_color()
            } else {
                ui.visuals().error_fg_color
            };
            ui.label(egui::RichText::new(message).color(color));
        }

        if view.preview.is_some() {
            settings_hint(ui, "开启要导入的类别，再应用到这台电脑。");
            ui.horizontal_wrapped(|ui| {
                if ui
                    .add_enabled(!view.import.is_empty(), egui::Button::new("应用所选配置"))
                    .clicked()
                {
                    self.pending.push(Cmd::ApplyConfig);
                }
                if ui.button("取消导入").clicked() {
                    view.preview = None;
                    view.import.clear();
                    view.feedback = None;
                }
            });
            if let Some(bundle) = &view.preview {
                settings_label(ui, "待导入的配置");
                settings_card(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 4.0;
                    let mut first = true;
                    for section in Section::ALL {
                        if let Some(summary) = bundle.summary(section, &self.mail.accounts) {
                            if !first {
                                ui.separator();
                            }
                            first = false;
                            include(ui, &mut view.import, section, &summary);
                            if section == Section::Mail && bundle.email_addresses().next().is_some()
                            {
                                ui.collapsing("查看文件中的邮箱", |ui| {
                                    for email in bundle.email_addresses() {
                                        settings_hint(ui, email);
                                    }
                                });
                            }
                        }
                    }
                });
                if bundle.email_addresses().next().is_some() {
                    settings_hint(ui, "新邮箱先关闭监控，可在「邮箱」点击「配置授权」。");
                }
            }
            return;
        }

        let webdav = view.webdav.get_or_insert_with(WebDavView::load);
        let busy = webdav.pending.is_some();
        let has_selection = !view.export.is_empty();

        settings_label(ui, "WebDAV 同步");
        settings_card(ui, |ui| {
            if let Some(saved) = &webdav.saved {
                let place = if saved.directory.is_empty() {
                    "根目录".into()
                } else {
                    saved.directory.replace('/', "\\")
                };
                settings_hint(ui, &format!("{place} · Leidao-config.json"));
            }
            ui.add_enabled_ui(!busy, |ui| {
                ui.collapsing("连接设置", |ui| {
                    input(
                        ui,
                        "服务器地址",
                        &mut webdav.draft.server,
                        "https://dav.example.net/webdav",
                        false,
                    );
                    input(
                        ui,
                        "文件存储路径",
                        &mut webdav.draft.directory,
                        "例如 配置备份/浮岛",
                        false,
                    );
                    input(ui, "账号", &mut webdav.draft.username, "", false);
                    let hint = if webdav.saved.as_ref().is_some_and(|saved| {
                        saved.username == webdav.draft.username
                            && saved.server == webdav.draft.server
                    }) {
                        "已保存，留空沿用"
                    } else {
                        "应用密码"
                    };
                    input(ui, "应用密码", &mut webdav.password, hint, true);
                    settings_hint(
                        ui,
                        "连接时自动创建缺失目录。应用密码保存在 Windows 凭据管理器。",
                    );
                    if ui.button("测试并保存连接").clicked() {
                        self.pending.push(Cmd::WebDavConnect);
                    }
                });
                ui.horizontal_wrapped(|ui| {
                    if ui
                        .add_enabled(
                            webdav.ready() && has_selection,
                            egui::Button::new("上传所选配置"),
                        )
                        .clicked()
                    {
                        self.pending.push(Cmd::WebDavUpload);
                    }
                    if ui
                        .add_enabled(webdav.ready(), egui::Button::new("下载并预览"))
                        .clicked()
                    {
                        self.pending.push(Cmd::WebDavDownload);
                    }
                });
            });
            if busy {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("正在与 WebDAV 通信…");
                });
            } else if !webdav.ready() {
                settings_hint(ui, "请先测试并保存连接。");
            }
        });

        settings_label(ui, "本地文件");
        settings_card(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                if ui
                    .add_enabled(has_selection && !busy, egui::Button::new("导出配置文件…"))
                    .clicked()
                {
                    self.pending.push(Cmd::ExportConfig);
                }
                if ui
                    .add_enabled(!busy, egui::Button::new("选择配置文件…"))
                    .clicked()
                {
                    self.pending.push(Cmd::ChooseConfig);
                }
            });
        });

        settings_label(
            ui,
            &format!(
                "同步范围（已开启 {} / {}）",
                view.export.len(),
                Section::ALL.len()
            ),
        );
        let mut scope_changed = false;
        settings_card(ui, |ui| {
            ui.add_enabled_ui(!busy, |ui| {
                for (i, section) in Section::ALL.into_iter().enumerate() {
                    if i > 0 {
                        ui.separator();
                    }
                    scope_changed |= include(ui, &mut view.export, section, section.description());
                }
            });
        });
        if scope_changed {
            if let Err(error) = save_scope(&view.export) {
                view.feedback = Some((false, error));
            }
        }
        settings_hint(ui, "此范围用于 WebDAV 上传和文件导出。邮箱凭据、AI 额度、系统权限与本机记录保留在各台电脑。");
    }

    pub fn config_sync_error(&mut self, message: impl Into<String>) {
        self.config_sync.feedback = Some((false, message.into()));
    }

    pub fn export_config(&mut self, path: &Path) {
        let bundle = Bundle::from_app(self, &self.config_sync.export);
        match bundle.write(path) {
            Ok(()) => self.config_sync.feedback = Some((true, "配置文件已导出。".into())),
            Err(error) => self.config_sync_error(error),
        }
    }

    fn show_config_preview(&mut self, bundle: Bundle) {
        self.config_sync.import = Section::ALL
            .into_iter()
            .filter(|section| bundle.contains(*section))
            .collect();
        self.config_sync.preview = Some(bundle);
        self.config_sync.feedback = None;
    }

    pub fn preview_config(&mut self, path: &Path) {
        self.config_sync.preview = None;
        self.config_sync.import.clear();
        self.config_sync.feedback = None;
        match Bundle::read(path) {
            Ok(bundle) => self.show_config_preview(bundle),
            Err(error) => self.config_sync_error(error),
        }
    }

    pub fn apply_config(&mut self) {
        let Some(bundle) = self.config_sync.preview.take() else {
            return;
        };
        let selected = std::mem::take(&mut self.config_sync.import);
        match bundle.apply(self, &selected) {
            Ok(added) => {
                let message = if added == 0 {
                    "所选配置已应用。".into()
                } else {
                    format!("所选配置已应用。已添加 {added} 个邮箱，请在「邮箱」配置授权。")
                };
                self.config_sync.feedback = Some((true, message));
                if selected.contains(&Section::Mail) {
                    self.pending.push(Cmd::MailSync);
                }
            }
            Err(error) => {
                self.config_sync.preview = Some(bundle);
                self.config_sync.import = selected;
                self.config_sync_error(error);
            }
        }
    }

    pub fn start_config_webdav(&mut self, command: Cmd, ctx: &egui::Context) {
        let Some(webdav) = &mut self.config_sync.webdav else {
            return;
        };
        if webdav.pending.is_some() {
            return;
        }
        let connect = command == Cmd::WebDavConnect;
        if !connect && !webdav.ready() {
            self.config_sync_error("请先测试并保存连接");
            return;
        }
        let connection = match webdav.draft.normalized() {
            Ok(connection) => connection,
            Err(message) => {
                self.config_sync_error(message);
                return;
            }
        };
        let password = std::mem::take(&mut webdav.password);
        let incoming = (command == Cmd::WebDavUpload)
            .then(|| Bundle::from_app(self, &self.config_sync.export));
        let (tx, rx) = mpsc::channel();
        let ctx = ctx.clone();
        let spawned = std::thread::Builder::new()
            .name("leidao-config-webdav".into())
            .spawn(move || {
                let result = (|| {
                    let password = if password.is_empty() {
                        connection.password()?
                    } else {
                        password
                    };
                    let client = Client::new(&connection, &password)?;
                    if connect {
                        client.ensure_directory()?;
                        connection.save(&password)?;
                        Ok(WebEvent::Connected(connection))
                    } else if let Some(bundle) = incoming {
                        client.upload(bundle)?;
                        Ok(WebEvent::Uploaded)
                    } else {
                        client
                            .download()
                            .map(|bundle| WebEvent::Downloaded(Box::new(bundle)))
                    }
                })();
                let _ = tx.send(result);
                ctx.request_repaint();
            });
        match spawned {
            Ok(_) => {
                if let Some(webdav) = &mut self.config_sync.webdav {
                    webdav.pending = Some(rx);
                }
                self.config_sync.feedback = None;
            }
            Err(_) => self.config_sync_error("无法启动 WebDAV 同步"),
        }
    }

    pub fn poll_config_webdav(&mut self) {
        let Some(webdav) = &mut self.config_sync.webdav else {
            return;
        };
        let Some(receiver) = &webdav.pending else {
            return;
        };
        let result = match receiver.try_recv() {
            Ok(result) => result,
            Err(mpsc::TryRecvError::Empty) => return,
            Err(mpsc::TryRecvError::Disconnected) => Err("WebDAV 同步已中断".into()),
        };
        webdav.pending = None;
        match result {
            Ok(WebEvent::Connected(connection)) => {
                webdav.draft = connection.clone();
                webdav.saved = Some(connection);
                self.config_sync.feedback =
                    Some((true, "WebDAV 连接已保存，存储目录可用。".into()));
            }
            Ok(WebEvent::Uploaded) => {
                self.config_sync.feedback = Some((true, "已上传所选配置。".into()));
            }
            Ok(WebEvent::Downloaded(bundle)) => self.show_config_preview(*bundle),
            Err(error) => self.config_sync_error(error),
        }
    }
}
