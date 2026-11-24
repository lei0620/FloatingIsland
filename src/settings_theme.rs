//! 设置窗口独立外观，不改变岛体的绘制样式。
use eframe::egui::{self,Color32};

/// Settings use desktop reading sizes, independently of the compact island typography.
pub fn configure_typography(style:&mut egui::Style){
    for (role,size) in [(egui::TextStyle::Body,14.0),(egui::TextStyle::Button,14.0),
        (egui::TextStyle::Small,13.0),(egui::TextStyle::Heading,21.0)]{
        style.text_styles.insert(role,crate::theme::settings_font(size));
    }
    style.text_styles.insert(egui::TextStyle::Monospace,egui::FontId::monospace(13.0));
    style.spacing.interact_size.y=32.0;
    style.spacing.item_spacing=egui::vec2(8.0,8.0);
    style.spacing.button_padding=egui::vec2(12.0,7.0);
    let dark=style.visuals.dark_mode;
    let border=if dark{Color32::from_rgb(61,63,68)}else{Color32::from_rgb(225,228,233)};
    let text=if dark{Color32::from_rgb(236,238,242)}else{Color32::from_rgb(31,35,41)};
    style.visuals.override_text_color=Some(text);
    style.visuals.widgets.noninteractive.bg_stroke=egui::Stroke::new(1.0_f32,border);
    style.visuals.widgets.noninteractive.fg_stroke=egui::Stroke::new(1.0_f32,text);
    style.visuals.widgets.inactive.bg_fill=if dark{Color32::from_rgb(40,42,47)}else{Color32::WHITE};
    style.visuals.widgets.inactive.weak_bg_fill=style.visuals.widgets.inactive.bg_fill;
    style.visuals.widgets.inactive.bg_stroke=egui::Stroke::new(1.0_f32,border);
    style.visuals.widgets.inactive.fg_stroke=egui::Stroke::new(1.0_f32,text);
    style.visuals.widgets.hovered.bg_fill=if dark{Color32::from_rgb(51,54,60)}else{Color32::from_rgb(244,245,247)};
    style.visuals.widgets.hovered.weak_bg_fill=style.visuals.widgets.hovered.bg_fill;
    style.visuals.widgets.hovered.bg_stroke=egui::Stroke::new(1.0_f32,if dark{Color32::from_gray(105)}else{Color32::from_rgb(170,177,188)});
    style.visuals.widgets.active.bg_stroke=egui::Stroke::new(1.0_f32,Color32::from_rgb(92,135,206));
    style.visuals.extreme_bg_color=if dark{Color32::from_rgb(28,30,34)}else{Color32::from_rgb(250,251,252)};
    style.visuals.selection.bg_fill=if dark{Color32::from_rgb(45,74,113)}else{Color32::from_rgb(222,234,253)};
    for widget in [&mut style.visuals.widgets.inactive,&mut style.visuals.widgets.hovered,&mut style.visuals.widgets.active]{
        widget.corner_radius=egui::CornerRadius::same(6);
    }
}

#[derive(Clone,Copy,Debug,Default,PartialEq,Eq)]
pub enum SettingsTheme { #[default] System, Light, Dark }
impl SettingsTheme {
    pub fn key(self)->&'static str {match self {Self::System=>"system",Self::Light=>"light",Self::Dark=>"dark"}}
    pub fn parse(value:&str)->Self {match value {"light"=>Self::Light,"dark"=>Self::Dark,_=>Self::System}}
    pub fn dark(self,system_dark:bool)->bool {match self {Self::System=>system_dark,Self::Light=>false,Self::Dark=>true}}
}

pub fn system_dark(ctx:&egui::Context)->bool {
    let id=egui::Id::new("settings-system-theme");
    if let Some((at,dark))=ctx.data(|d|d.get_temp::<(std::time::Instant,bool)>(id)) {
        if at.elapsed()<std::time::Duration::from_secs(1){return dark;}
    }
    #[cfg(windows)]
    let dark={
        use windows::{core::w,Win32::System::Registry::*};
        let mut value=1u32;let mut size=4u32;
        unsafe { let _=RegGetValueW(HKEY_CURRENT_USER,w!("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize"),
            w!("AppsUseLightTheme"),RRF_RT_REG_DWORD,None,Some((&mut value as *mut u32).cast()),Some(&mut size)); }
        value==0
    };
    #[cfg(not(windows))]
    let dark=ctx.input(|i|i.raw.system_theme)==Some(egui::Theme::Dark);
    ctx.data_mut(|d|d.insert_temp(id,(std::time::Instant::now(),dark)));
    dark
}

pub fn color(ctx:&egui::Context,dark:Color32,light:Color32)->Color32 {
    if ctx.style().visuals.dark_mode {dark}else{light}
}
