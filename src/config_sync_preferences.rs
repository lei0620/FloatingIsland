use crate::app::App;
use crate::config_sync::Section;
use crate::integrations::{self, Config};
use crate::model::{NoticeKind, NoticePreferences, Settings};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};

const NOTICES: [NoticeKind; 7] = [
    NoticeKind::Wechat,
    NoticeKind::Wecom,
    NoticeKind::Windows,
    NoticeKind::Todo,
    NoticeKind::Meeting,
    NoticeKind::System,
    NoticeKind::Bluetooth,
];

// Preserve the existing v2 wire format so an older Leidao can still use the
// same bundle. The new weather page and resident slot stay local on import.
fn portable_value(key: &str, value: &str) -> String {
    match key {
        "visible_pages" => {
            let pages: String = value.chars().take(crate::app::WEATHER_PAGE).collect();
            if pages.contains('1') {
                pages
            } else {
                "10000000".into()
            }
        }
        "page_order" => without_slot(value, crate::app::WEATHER_PAGE),
        "resident_order" => without_slot(value, crate::bar_layout::WEATHER),
        _ => value.into(),
    }
}

fn without_slot(value: &str, slot: usize) -> String {
    let slot = slot.to_string();
    value
        .split(',')
        .filter(|entry| *entry != slot)
        .collect::<Vec<_>>()
        .join(",")
}

fn with_local_slot(value: &str, current: &[usize], slot: usize) -> String {
    if value.is_empty() {
        return String::new();
    }
    let mut items: Vec<_> = value.split(',').map(str::to_owned).collect();
    let position = current
        .iter()
        .position(|entry| *entry == slot)
        .unwrap_or(items.len());
    items.insert(position.min(items.len()), slot.to_string());
    items.join(",")
}

fn keys(section: Section) -> &'static [&'static str] {
    match section {
        Section::Appearance => &[
            "skin",
            "island_shape",
            "dock",
            "glass_blur",
            "island_size",
            "island_top_gap",
            "settings_theme",
            "pet",
            "minimal",
            "clock_12h",
            "clock_period",
            "show_battery",
            "browser_minimal",
            "avoid_browsers",
            "hide_fullscreen",
        ],
        Section::Pages => &["visible_pages", "page_order", "resident_order"],
        Section::Music => &["lyrics_enabled", "island_lyrics", "lyric_delay_ms"],
        Section::Notifications => &[
            "notifications",
            "notification_preview",
            "sound",
            "system_hud",
            "bluetooth_notifications",
            "wechat_notifications",
            "wechat_tray_open",
        ],
        Section::Calendar => &["mail_ics", "calendar_prompt", "calendar_auto_sync"],
        _ => &[],
    }
}

#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ExtraPreferences {
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    settings: BTreeMap<Section, BTreeMap<String, String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    notifications: Option<Notifications>,
    #[serde(skip_serializing_if = "Option::is_none")]
    clipboard: Option<Clipboard>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Notifications {
    codex: bool,
    codex_notice_seconds: u32,
    wechat_fallback: bool,
    wecom: bool,
    todo: bool,
    meeting: bool,
    durations: BTreeMap<NoticeKind, f64>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Clipboard {
    enabled: bool,
    images: bool,
}

impl ExtraPreferences {
    pub(crate) fn capture(app: &App, selected: &HashSet<Section>) -> Self {
        let pairs = app.settings.pairs();
        let prefs = &app.settings.notice_preferences;
        let local = &app.integrations.config;
        Self {
            settings: Section::ALL
                .into_iter()
                .filter(|section| selected.contains(section) && !keys(*section).is_empty())
                .map(|section| {
                    (
                        section,
                        pairs
                            .iter()
                            .filter(|(key, _)| keys(section).contains(&key.as_str()))
                            .map(|(key, value)| (key.clone(), portable_value(key, value)))
                            .collect(),
                    )
                })
                .collect(),
            notifications: selected
                .contains(&Section::Notifications)
                .then(|| Notifications {
                    codex: local.codex,
                    codex_notice_seconds: local.codex_notice_seconds,
                    wechat_fallback: local.wechat,
                    wecom: local.wecom,
                    todo: prefs.todo,
                    meeting: prefs.meeting,
                    durations: NOTICES
                        .into_iter()
                        .filter_map(|kind| prefs.override_seconds(kind).map(|s| (kind, s)))
                        .collect(),
                }),
            clipboard: selected.contains(&Section::Clipboard).then_some(Clipboard {
                enabled: local.clipboard,
                images: local.images,
            }),
        }
    }

    pub(crate) fn contains(&self, section: Section) -> bool {
        self.settings.contains_key(&section)
            || (section == Section::Notifications && self.notifications.is_some())
            || (section == Section::Clipboard && self.clipboard.is_some())
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.settings.is_empty() && self.notifications.is_none() && self.clipboard.is_none()
    }

    pub(crate) fn merge(&mut self, incoming: Self) {
        self.settings.extend(incoming.settings);
        if incoming.notifications.is_some() {
            self.notifications = incoming.notifications;
        }
        if incoming.clipboard.is_some() {
            self.clipboard = incoming.clipboard;
        }
    }

    pub(crate) fn validate(&self) -> Result<(), String> {
        for (section, pairs) in &self.settings {
            if pairs.is_empty()
                || pairs
                    .keys()
                    .any(|key| !keys(*section).contains(&key.as_str()))
            {
                return Err("配置文件包含无法同步的偏好".into());
            }
            let normalized: BTreeMap<_, _> = Settings::from_pairs(pairs.clone())
                .pairs()
                .into_iter()
                .collect();
            if pairs.iter().any(|(key, value)| {
                normalized
                    .get(key)
                    .is_none_or(|normalized| portable_value(key, normalized) != *value)
            }) {
                return Err(format!("「{}」包含无效的设置值", section.title()));
            }
        }
        if self.notifications.as_ref().is_some_and(|v| {
            !(integrations::MIN_CODEX_NOTICE_SECONDS..=integrations::MAX_CODEX_NOTICE_SECONDS)
                .contains(&v.codex_notice_seconds)
                || v.durations.iter().any(|(kind, seconds)| {
                    !NOTICES.contains(kind)
                        || !(NoticePreferences::MIN_SECONDS..=NoticePreferences::MAX_SECONDS)
                            .contains(seconds)
                })
        }) {
            return Err("配置文件中的通知偏好超出范围".into());
        }
        Ok(())
    }

    pub(crate) fn apply<'a>(
        &'a self,
        settings: &mut Settings,
        integrations: &mut Config,
        selected: &HashSet<Section>,
        changes: &mut Vec<(&'a str, String)>,
    ) -> bool {
        let pairs: Vec<_> = self
            .settings
            .iter()
            .filter(|(section, _)| selected.contains(section))
            .flat_map(|(_, pairs)| pairs)
            .map(|(key, value)| {
                let value = match key.as_str() {
                    "visible_pages" => format!(
                        "{value}{}",
                        u8::from(settings.visible_pages[crate::app::WEATHER_PAGE])
                    ),
                    "page_order" => {
                        with_local_slot(value, &settings.page_order, crate::app::WEATHER_PAGE)
                    }
                    "resident_order" => {
                        with_local_slot(value, &settings.resident_order, crate::bar_layout::WEATHER)
                    }
                    _ => value.clone(),
                };
                (key.as_str(), value)
            })
            .collect();
        let applied = !pairs.is_empty();
        if applied {
            let mut merged: BTreeMap<_, _> = settings.pairs().into_iter().collect();
            merged.extend(
                pairs
                    .iter()
                    .map(|(key, value)| (key.to_string(), value.clone())),
            );
            *settings = Settings::from_pairs(merged);
            changes.extend(pairs);
        }
        let notifications = self
            .notifications
            .as_ref()
            .filter(|_| selected.contains(&Section::Notifications));
        if let Some(v) = notifications {
            integrations.codex = v.codex;
            integrations.codex_notice_seconds = v.codex_notice_seconds;
            integrations.wechat = v.wechat_fallback;
            integrations.wecom = v.wecom;
            let prefs = &mut settings.notice_preferences;
            prefs.todo = v.todo;
            prefs.meeting = v.meeting;
            for kind in NOTICES {
                prefs.replace_seconds(kind, v.durations.get(&kind).copied());
            }
        }
        let clipboard = self
            .clipboard
            .as_ref()
            .filter(|_| selected.contains(&Section::Clipboard));
        if let Some(v) = clipboard {
            integrations.clipboard = v.enabled;
            integrations.images = v.images;
        }
        applied || notifications.is_some() || clipboard.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn v2_page_layout_preserves_the_destination_weather_slot() {
        use crate::{app::WEATHER_PAGE, bar_layout::WEATHER};

        let bundle: ExtraPreferences = serde_json::from_str(
            r#"{
            "settings":{"pages":{
                "visible_pages":"11111110",
                "page_order":"7,0,1,2,3,4,5,6",
                "resident_order":"1,9,0,2,6,7"
            }}
        }"#,
        )
        .unwrap();
        bundle.validate().unwrap();
        let upgraded = Settings::from_pairs([("visible_pages".into(), "11111111".into())]);
        assert!(!upgraded.visible_pages[WEATHER_PAGE]);
        let mut current = Settings::default();
        current.visible_pages[WEATHER_PAGE] = true;
        current.page_order.rotate_right(1);
        current.resident_order = crate::bar_layout::DEFAULT_ORDER.to_vec();
        let mut changes = Vec::new();
        assert!(bundle.apply(
            &mut current,
            &mut Config::default(),
            &[Section::Pages].into_iter().collect(),
            &mut changes
        ));
        assert!(current.visible_pages[WEATHER_PAGE]);
        assert!(!current.visible_pages[7]);
        assert_eq!(current.page_order[0], WEATHER_PAGE);
        assert_eq!(current.resident_order[3], WEATHER);
        assert_eq!(
            portable_value(
                "page_order",
                &changes
                    .iter()
                    .find(|(key, _)| *key == "page_order")
                    .unwrap()
                    .1
            ),
            "7,0,1,2,3,4,5,6"
        );
    }
}
