use crate::app::{App, Cmd};
use crate::extras::{notification_row, setting_row, settings_card, settings_hint, settings_label, settings_segmented};
use crate::integrations::{MAX_CODEX_NOTICE_SECONDS, MIN_CODEX_NOTICE_SECONDS};
use crate::messengers::Source;
use crate::model::{NoticeKind, NoticePreferences};
use eframe::egui;

fn seconds_range() -> std::ops::RangeInclusive<f64> {
    NoticePreferences::MIN_SECONDS..=NoticePreferences::MAX_SECONDS
}

fn kind_row(ui: &mut egui::Ui, title: &str, detail: &str, enabled: &mut bool, prefs: &mut NoticePreferences, kind: NoticeKind) {
    let mut seconds = prefs.seconds(kind);
    if notification_row(ui, title, detail, enabled, &mut seconds, seconds_range(), 0.5) {
        prefs.set_seconds(kind, seconds);
    }
}

impl App {
    pub(crate) fn notification_settings(&mut self, ui: &mut egui::Ui) {
        let before_integrations = self.integrations.config.clone();
        let before_apps = self.notification_apps.items.clone();
        let before_water = self.water.remind;

        settings_card(ui, |ui| {
            setting_row(ui, "提醒总开关", "控制浮岛上的通知", &mut self.settings.notifications);
            ui.separator();
            ui.add_enabled_ui(self.settings.notifications, |ui| {
                ui.label("提醒方式");
                let mut mode = if !self.settings.system_notify { 0 } else if self.settings.sound { 2 } else { 1 };
                if settings_segmented(ui, &mut mode,
                    &[(0, "仅岛上"), (1, "加系统通知"), (2, "系统通知＋声音")], 84.0)
                {
                    self.settings.system_notify = mode != 0;
                    self.settings.sound = mode == 2;
                }
            });
        });
        let master = self.settings.notifications;
        ui.add_space(5.0);
        settings_hint(ui, if master { "每项开关旁可调整岛上显示时长，下次提醒生效。" }
            else { "提醒总开关已关闭，开启后将按下方设置显示。" });

        settings_label(ui, "任务与消息");
        settings_card(ui, |ui| {
            let cfg = &mut self.integrations.config;
            notification_row(ui, "Codex 任务提醒", "任务完成、需要确认或中断；已读时提前收起",
                &mut cfg.codex, &mut cfg.codex_notice_seconds,
                MIN_CODEX_NOTICE_SECONDS..=MAX_CODEX_NOTICE_SECONDS, 1);
            ui.separator();
            let prefs = &mut self.settings.notice_preferences;
            kind_row(ui, "微信新消息提醒", "个人微信的任务栏与备用提醒",
                &mut self.settings.wechat_notifications, prefs, NoticeKind::Wechat);
            ui.separator();
            kind_row(ui, "企业微信提醒", "独立控制企业微信",
                &mut cfg.wecom, prefs, NoticeKind::Wecom);
            ui.separator();
            let qq = self.notification_apps.items.iter_mut().find(|(id, _, _)| id == "QQ");
            let mut enabled = qq.as_ref().is_none_or(|item| item.2);
            let mut seconds = prefs.app_seconds("QQ");
            if notification_row(ui, "QQ 提醒", "需要连接 Windows 通知",
                &mut enabled, &mut seconds, seconds_range(), 0.5) {
                prefs.set_app_seconds("QQ", seconds);
            }
            if let Some(item) = qq {
                item.2 = enabled;
            } else if !enabled {
                self.notification_apps.items.push(("QQ".into(), "QQ".into(), false));
            }
        });

        settings_label(ui, "办公消息");
        settings_card(ui, |ui| {
            ui.add_enabled_ui(self.integrations.config.wecom, |ui| {
                setting_row(ui, "企业微信多账号监听", "识别任务栏闪动的企业微信账号",
                    &mut self.settings.wecom_notifications);
            });
            ui.separator();
            setting_row(ui, "飞书消息提醒", "任务栏闪动时提醒，可打开对应窗口",
                &mut self.settings.feishu_notifications);
            ui.separator();
            setting_row(ui, "钉钉消息提醒", "任务栏闪动时提醒，可打开对应窗口",
                &mut self.settings.dingtalk_notifications);
        });

        settings_label(ui, "饮水、专注与待办");
        settings_card(ui, |ui| {
            let prefs = &mut self.settings.notice_preferences;
            kind_row(ui, "饮水提醒", "按饮水页设置的时段和间隔提醒",
                &mut self.water.remind, prefs, NoticeKind::Water);
            ui.separator();
            setting_row(ui, "久坐提醒", "在健康提醒页设置间隔；默认关闭，提醒显示时长沿用饮水项",
                &mut self.settings.sedentary_enabled);
            ui.separator();
            let mut focus = prefs.focus;
            kind_row(ui, "专注计时提醒", "专注或休息结束时提醒",
                &mut focus, prefs, NoticeKind::Focus);
            prefs.focus = focus;
            ui.separator();
            let mut todo = prefs.todo;
            kind_row(ui, "待办提醒", "设有提醒时间的待办到期时提醒",
                &mut todo, prefs, NoticeKind::Todo);
            prefs.todo = todo;
        });

        settings_label(ui, "邮箱与日程");
        settings_card(ui, |ui| {
            let prefs = &mut self.settings.notice_preferences;
            kind_row(ui, "新邮件提醒", "开启后台收信并在新邮件到达时提醒",
                &mut self.settings.mail_enabled, prefs, NoticeKind::Mail);
            ui.separator();
            notification_row(ui, "验证码高亮", "有效期更短时提前隐藏；手动查看后显示 15 秒",
                &mut self.settings.mail_code, &mut self.settings.mail_code_hide_seconds, 15..=300, 5);
            ui.separator();
            let mut meeting = prefs.meeting;
            kind_row(ui, "会议与日程提醒", "同时适用于日历和邮件邀请",
                &mut meeting, prefs, NoticeKind::Meeting);
            prefs.meeting = meeting;
        });

        settings_label(ui, "系统与设备");
        settings_card(ui, |ui| {
            let prefs = &mut self.settings.notice_preferences;
            kind_row(ui, "系统状态", "音量、充电、网络和大写锁定",
                &mut self.settings.system_hud, prefs, NoticeKind::System);
            ui.separator();
            kind_row(ui, "蓝牙通知", "设备连接和断开",
                &mut self.settings.bluetooth_notifications, prefs, NoticeKind::Bluetooth);
            ui.separator();
            let mut connected = self.settings.read_notifications;
            let status = if self.settings.qq_only && connected {
                "当前仅接收 QQ；下方可更改连接范围"
            } else {
                "应用未单独设置时使用此显示时长"
            };
            kind_row(ui, "Windows 通知", status, &mut connected, prefs, NoticeKind::Windows);
            if connected != self.settings.read_notifications {
                self.pending.push(if connected { Cmd::RequestNotifications } else { Cmd::DisconnectNotifications });
            }
            for (id, name, enabled) in &mut self.notification_apps.items {
                if id == "QQ" { continue; }
                ui.separator();
                if Source::notification(name, id).is_some() {
                    setting_row(ui, name, "Windows 来源，显示时长跟随对应微信提醒", enabled);
                } else {
                    let mut seconds = prefs.app_seconds(id);
                    ui.push_id(id.as_str(), |ui| {
                        if notification_row(ui, name, "", enabled, &mut seconds, seconds_range(), 0.5) {
                            prefs.set_app_seconds(id, seconds);
                        }
                    });
                }
            }
        });

        settings_label(ui, "接收方式");
        settings_card(ui, |ui| {
            if !self.listener_status.is_empty() { settings_hint(ui, &self.listener_status); }
            if !self.settings.read_notifications || !self.settings.qq_only {
                if ui.button("只连接 QQ").clicked() { self.pending.push(Cmd::RequestQqNotifications); }
            }
            if self.settings.read_notifications && self.settings.qq_only {
                if ui.button("连接所有应用通知").clicked() { self.pending.push(Cmd::RequestNotifications); }
            }
            setting_row(ui, "显示 Windows 通知正文", "关闭后在岛上隐藏普通应用通知正文",
                &mut self.settings.notification_preview);
            ui.separator();
            ui.add_enabled_ui(self.settings.wechat_notifications, |ui| {
                setting_row(ui, "微信备用提醒", "主监听不可用时接收备用提醒",
                    &mut self.integrations.config.wechat);
                setting_row(ui, "托盘一键唤起微信", "点击提醒时可恢复缩在托盘里的微信",
                    &mut self.settings.wechat_tray_open);
            });
            let status=self.messenger_status(crate::app::Messenger::Wechat);
            if !status.is_empty() { settings_hint(ui, status); }
        });

        if before_water != self.water.remind {
            self.water.save();
            self.last_reminder = self.t;
        }
        if before_apps != self.notification_apps.items { self.notification_apps.save(); }
        self.save_integration_settings(before_integrations);
    }

    pub(crate) fn notification_settings_link(&mut self, ui: &mut egui::Ui) {
        if ui.button("通知开关与显示时长").clicked() {
            self.settings_tab = 6;
        }
    }
}
