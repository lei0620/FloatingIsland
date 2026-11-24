//! Outlook sign-in settings and completion. No mailbox password is requested.
use crate::{
    app::{App, Cmd},
    mail::{Account, Provider},
    outlook_oauth,
    settings_theme::color as settings_color,
};
use eframe::egui::{self, Color32, RichText};

impl App {
    pub(crate) fn outlook_mail_settings(&mut self, ui: &mut egui::Ui) {
        if !self.mail.form.outlook_loaded {
            self.mail.form.outlook_client_id = outlook_oauth::load_client_id();
            self.mail.form.outlook_loaded = true;
        }
        let busy = self.mail.outlook_busy;
        ui.label(RichText::new("微软账户登录").size(14.0));
        ui.label(RichText::new("在微软页面授权收信；登录密码不会交给浮岛。").size(13.0));
        ui.add_space(4.0);
        let width = (ui.available_width() - 72.0).max(100.0);
        ui.add_enabled_ui(!busy, |ui| {
            ui.horizontal(|ui| {
                ui.add_sized([64.0, 34.0], egui::Label::new("邮箱"));
                ui.add_sized(
                    [width, 34.0],
                    egui::TextEdit::singleline(&mut self.mail.form.email)
                        .hint_text("完整的 Outlook.com / Hotmail 地址"),
                );
            });
            ui.horizontal(|ui| {
                ui.add_sized([64.0, 34.0], egui::Label::new("客户端 ID"));
                ui.add_sized(
                    [width, 34.0],
                    egui::TextEdit::singleline(&mut self.mail.form.outlook_client_id)
                        .hint_text("应用程序（客户端）ID"),
                );
            });
        });
        ui.horizontal_wrapped(|ui| {
            if ui
                .add_enabled(!busy, egui::Button::new("使用微软账户登录并连接"))
                .clicked()
            {
                self.begin_outlook_login();
            }
            if busy && ui.button("取消登录").clicked() {
                self.mail.outlook_busy = false;
                self.mail.outlook_attempt = self.mail.outlook_attempt.wrapping_add(1);
                self.pending.push(Cmd::OutlookCancel);
                self.mail.note = "已取消微软登录。".into();
            }
        });
        if busy {
            ui.spinner();
        }
        ui.collapsing("首次使用：配置微软应用",|ui|{
            ui.label("1. 在 Microsoft Entra 的「应用注册」新建“浮岛”，账户类型选择支持个人 Microsoft 账户的选项。");
            ui.label("2. 添加「移动和桌面应用程序」平台，重定向 URI 填 http://localhost。");
            ui.label("3. 添加 Office 365 Exchange Online 的 IMAP.AccessAsUser.All 委托权限。");
            ui.label("4. 将概述页的“应用程序（客户端）ID”填入上方。无需创建客户端密钥。");
            if ui.link("打开微软应用注册门户").clicked(){self.pending.push(Cmd::OpenUrl("https://entra.microsoft.com/".into()));}
        });
        ui.add(
            egui::Label::new(
                RichText::new(
                    "在 Outlook 网页设置 → 邮件 → 转发和 IMAP 中允许 IMAP 访问，再返回这里登录。",
                )
                .size(13.0),
            )
            .wrap(),
        );
        if ui.link("打开 Outlook 网页版").clicked() {
            self.pending
                .push(Cmd::OpenUrl("https://outlook.live.com/mail/".into()));
        }
        ui.label(RichText::new("登录凭据保存在 Windows 凭据管理器中，可随账号删除。").size(13.0));
        if !self.mail.note.is_empty() {
            ui.add(
                egui::Label::new(
                    RichText::new(&self.mail.note)
                        .size(13.0)
                        .color(settings_color(
                            ui.ctx(),
                            Color32::from_gray(200),
                            Color32::from_gray(65),
                        )),
                )
                .wrap(),
            );
        }
    }

    pub(crate) fn begin_outlook_login(&mut self) {
        let email = self.mail.form.email.trim().to_string();
        let id = self.mail.form.outlook_client_id.trim().to_string();
        if !outlook_oauth::valid_email(&email) {
            self.mail.note = "请填写完整邮箱地址。".into();
            return;
        }
        if let Err(error) = outlook_oauth::save_client_id(&id) {
            self.mail.note = error;
            return;
        }
        self.mail.outlook_attempt = self.mail.outlook_attempt.wrapping_add(1);
        self.mail.outlook_busy = true;
        self.mail.note = "正在打开微软登录页面…".into();
        self.pending.push(Cmd::OutlookLogin(
            Account {
                email,
                provider: Provider::Outlook,
                custom: None,
                enabled: true,
            },
            id,
            self.mail.outlook_attempt,
        ));
    }

    pub(crate) fn outlook_login_result(
        &mut self,
        attempt: u64,
        account: Account,
        result: Result<outlook_oauth::Grant, String>,
    ) {
        // A cancelled or superseded authorization must not save credentials or add an account.
        if !self.mail.outlook_busy || self.mail.outlook_attempt != attempt {
            return;
        }
        self.mail.outlook_busy = false;
        match result.and_then(|grant| outlook_oauth::save_login(&account.email, &grant)) {
            Ok(()) => {
                self.pending.push(Cmd::MailRestart(account.email.clone()));
                self.mail.status.insert(
                    account.email.to_ascii_lowercase(),
                    crate::mail::Status::Ready,
                );
                self.mail.note = format!("{}：已连接，开始监控新邮件。", account.email);
                if let Some(old) = self
                    .mail
                    .accounts
                    .iter_mut()
                    .find(|a| a.email.eq_ignore_ascii_case(&account.email))
                {
                    *old = account;
                } else {
                    self.mail.accounts.push(account);
                }
                self.settings.mail_enabled = true;
                self.settings.save();
                self.mail_accounts_changed();
            }
            Err(error) => self.mail.note = error,
        }
    }
}
