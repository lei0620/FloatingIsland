#![cfg_attr(windows, windows_subsystem = "windows")]
//! Windows 外壳：无边框透明窗口 + 托盘 + 系统数据，界面全部交给 `daozai::app::App`。

use eframe::egui::{self, Color32, Pos2};
use daozai::app::{self, App, Cmd, Env, Options};
use daozai::codex_tasks::{self, ThreadLinkError};
use daozai::smtc;
#[cfg(windows)]
use daozai::messenger::{self,OpenError,TaskbarEvent};
use daozai::poll_clock::PollClock;
use std::time::{Duration, Instant};

mod diagnostics;
mod graphics;

struct Shell {
    app: App,
    t0: Instant,
    #[cfg(windows)]
    frame_at: Instant,
    /// 兼容渲染上一次请求读回画面的时刻（无输入的持续动画按约 30 帧/秒读回）。
    #[cfg(windows)]
    readback_at: Option<Instant>,
    media: smtc::Shared,
    seek_rx: Option<(&'static str, std::sync::mpsc::Receiver<bool>)>,
    codex_open_rx: Option<std::sync::mpsc::Receiver<Result<String, ThreadLinkError>>>,
    media_control:Option<smtc::ControlWorker>,
    music_volume_rx:Option<(String,std::sync::mpsc::Receiver<bool>)>,
    _resource_monitor:Option<daozai::resources::Monitor>,
    resource_at:PollClock,
    guard_notified:bool,
    media_enabled: smtc::Poller,
    /// 刚点过播放控制或音量时，短时间内按最快频率采样，让按钮状态及时回显。
    media_fast_until: Option<Instant>,
    placed: bool,
    placement_at: PollClock,
    placement_scale: f32,
    placement_gap: u32,
    /// Last display, offset and dock used to position the island.
    placement_key: (String, i32, daozai::model::Dock, bool, (i32,i32)),
    #[cfg(windows)]
    oled_shift: (i32,i32),
    win_x: f32,
    sys: (Option<u8>, bool, PollClock),
    #[cfg(windows)]
    system_state: daozai::system::Snapshot,
    #[cfg(windows)]
    system_ready: bool,
    #[cfg(windows)]
    system_rx: std::sync::mpsc::Receiver<daozai::system::Observation>,
    fullscreen: Option<daozai::display_layout::Bounds>,
    #[cfg(windows)]
    incoming: Option<daozai::system::Notifications>,
    #[cfg(windows)]
    messengers:messenger::Runtime,
    #[cfg(windows)]
    unread_at:PollClock,
    #[cfg(windows)]
    opening_messenger:Option<(app::Unread,Instant)>,
    /// 后台线程正在唤起消息客户端。
    #[cfg(windows)]
    tray_opening:Option<(app::Unread,std::sync::mpsc::Receiver<Result<(),OpenError>>)>,
    #[cfg(windows)]
    hidden: bool,
    /// 毛玻璃皮肤的背景亮度采样（仅毛玻璃皮肤运行）。
    #[cfg(windows)]
    backdrop: Option<daozai::backdrop::Sampler>,
    #[cfg(windows)]
    native: Option<daozai::native::Window>,
    #[cfg(windows)]
    tray: Option<daozai::tray::Tray>,
    #[cfg(windows)]
    autostart: bool,
    /// v0.5.4：蓝牙音频设备（后台线程推送）与音量回声的静默窗口。
    #[cfg(windows)]
    bt_rx: Option<daozai::bt::Service>,
    bt: Vec<daozai::bt::Device>,
    bt_ready: bool,
    volume_quiet: PollClock,
    /// v0.5.5：邮件收信服务（工作线程），以及上次同步给它的账号/开关快照。
    /// 邮箱模块开启时才存在；关闭后收信与整理线程随之退出。
    mail: Option<daozai::mail::Service>,
    mail_sig: Option<(bool, u32, Vec<daozai::mail::Account>)>,
    /// v0.5.6：日程（CalDAV）服务与上次下发的配置。
    /// 日程模块开启时才存在。
    cal: Option<daozai::calendar::Service>,
    cal_sig: Option<(Vec<daozai::calendar::Config>, bool)>,
    quota: daozai::quota::Service,
    quota_sig: Option<(bool,Vec<daozai::quota::Account>)>,
    quota_at: PollClock,
    integrations: daozai::integrations::Service,
    clipboard: daozai::clipboard_history::Service,
    diagnostic_clock:PollClock,
}

const MODULE_OFF:&str="该功能已在「页面管理」中关闭";

impl Shell {
    /// 按模块开关启停后台服务：关闭的模块不保留任何线程，重新开启时从头对齐配置。
    fn reconcile_modules(&mut self){
        let mail_on=self.app.background_on(app::MAIL_PAGE);
        if mail_on!=self.mail.is_some() {self.mail=mail_on.then(daozai::mail::Service::start);self.mail_sig=None;}
        let cal_on=self.app.background_on(app::CALENDAR_PAGE);
        if cal_on!=self.cal.is_some() {self.cal=cal_on.then(daozai::calendar::Service::start);self.cal_sig=None;}
    }

    #[cfg(windows)]
    fn confirm_messenger_open(&mut self,unread:app::Unread)->bool{
        let foreground=self.messengers.foreground(&[unread.source]);
        if !self.app.confirm_messenger_open(unread,foreground){
            let seen=foreground.map(|(source,pid)|format!("{} 账号 {pid}",source.name())).unwrap_or_else(messenger::windows::describe_foreground);
            messenger::windows::trace(format!("唤起 {} 账号 {}：未到前台（前台是 {seen}）",unread.source.name(),unread.pid));
            return false;
        }
        messenger::windows::trace(format!("唤起 {} 账号 {}：已在前台",unread.source.name(),unread.pid));
        self.messengers.viewed_client(unread.source,unread.pid);
        true
    }

    #[cfg(windows)]
    fn clear_exited_messengers(&mut self,ctx:&egui::Context){
        for unread in self.messengers.exited(&self.app.pending_messengers){
            messenger::windows::trace(format!("清除已退出的{}提醒 账号 {}",unread.source.name(),unread.pid));
            if self.tray_opening.as_ref().is_some_and(|(target,_)|*target==unread) || self.opening_messenger.is_some_and(|(target,_)|target==unread){
                self.app.messenger_unavailable(format!("{} 账号已退出",unread.source.name()));
            }
            self.app.discard_unread(unread);
            ctx.request_repaint();
        }
        self.unread_at=PollClock::now();
    }

    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        diagnostics::graphics(cc);
        let running_compat=cc.wgpu_render_state.is_some();
        diagnostics::startup("加载本地配置");
        let mut app = App::new(&cc.egui_ctx, Options::default());
        app.render_compat_running=running_compat;
        #[cfg(feature="record")] let quota_demo=std::env::var("DAOZAI_QUOTA_DEMO").is_ok();
        #[cfg(not(feature="record"))] let quota_demo=false;
        diagnostics::startup("初始化额度配置");
        if !quota_demo{app.quota_initialize();}
        diagnostics::startup("启动剪贴板记录");
        if std::env::args().any(|arg|arg=="--settings"){app.open_settings();}
        if std::env::args().any(|arg|arg=="--config-sync"){app.open_config_sync();}
        if std::env::args().any(|arg|arg=="--calendar"){app.open_calendar(None);}
        if std::env::args().any(|arg|arg=="--clipboard"){app.open_integrations(true);}
        if std::env::args().any(|arg|arg=="--messages"){app.open_integrations(false);}
        #[cfg(feature="record")]
        if std::env::var("DAOZAI_MAIL_DEMO").is_ok(){app.mail_demo();}
        #[cfg(feature="record")]
        if std::env::var("DAOZAI_CAL_DEMO").is_ok(){app.calendar_demo();}
        #[cfg(feature="record")]
        if let Ok(mode)=std::env::var("DAOZAI_QUOTA_DEMO"){app.quota_demo(&mode);}
        diagnostics::startup("初始化媒体和托盘");
        let media = smtc::shared();
        let media_enabled=smtc::spawn_poller(media.clone(),cc.egui_ctx.clone());
        media_enabled.store(app.settings.visible_pages[0],std::sync::atomic::Ordering::Relaxed);
        #[cfg(windows)]
        let autostart = daozai::tray::autostart_enabled();
        #[cfg(windows)]
        let tray = app.start_tray(daozai::tray::State {
            pet: app.settings.pet,
            minimal: app.settings.minimal,
            autostart,
        });
        diagnostics::startup("初始化通知监听");
        #[cfg(windows)]
        let request_notifications=std::env::args().any(|arg|arg=="--connect-notifications");
        #[cfg(windows)]
        if request_notifications {
            app.settings.read_notifications=true;
            app.settings.qq_only=false;
            app.settings.save();
        }
        #[cfg(windows)]
        let incoming=if app.settings.read_notifications {
            match daozai::system::connect(cc.egui_ctx.clone(),messenger::NotificationFilter::for_settings(&app.settings)) {
                Ok(rx)=>Some(rx),
                Err(e)=>{app.listener_status=format!("Windows 无法读取通知：{}",e.code());None}
            }
        } else {None};
        // 音频和网络查询由后台完成，避免首帧等待系统服务。
        diagnostics::startup("创建后台采样与窗口");
        #[cfg(windows)] let system_state=daozai::system::Snapshot::default();

        #[cfg(windows)] let system_rx=daozai::system::observe(cc.egui_ctx.clone(),system_state.clone());
        let integrations=daozai::integrations::Service::start(cc.egui_ctx.clone());
        let clipboard=daozai::clipboard_history::Service::start(cc.egui_ctx.clone(),app.integrations.config.clipboard,app.integrations.config.images);
        // 岛体大小只作用于主窗口，在首帧之前生效，避免启动时先按 100% 闪一下。
        #[cfg(windows)] eframe::set_root_viewport_scale(app.settings.island_scale());
        Self {
            integrations,
            clipboard,
            diagnostic_clock:PollClock::default(),
            app,
            t0: Instant::now(),
            #[cfg(windows)]
            frame_at: Instant::now(),
            #[cfg(windows)]
            readback_at: None,
            media,
            seek_rx: None,
            codex_open_rx: None,
            media_control:None,
            music_volume_rx:None,
            _resource_monitor:daozai::resources::Monitor::start(daozai::model::config_dir().join("diagnostics")),
            resource_at:PollClock::default(),
            guard_notified:false,
            media_enabled,
            media_fast_until: None,
            placed: false,
            placement_at: PollClock::default(),
            placement_scale: 0.0,
            placement_gap: daozai::display_layout::DEFAULT_TOP_GAP,
            placement_key: (String::new(), 0, daozai::model::Dock::Top, false, (0,0)),
            #[cfg(windows)] oled_shift: (0,0),
            win_x: 0.0,
            sys: (None, false, PollClock::default()),
            #[cfg(windows)]
            system_state,
            #[cfg(windows)] system_ready:false,
            #[cfg(windows)] system_rx,
            fullscreen:None,
            #[cfg(windows)] incoming,
            #[cfg(windows)] messengers:Default::default(),
            #[cfg(windows)] unread_at:PollClock::default(),
            #[cfg(windows)] opening_messenger:None,
            #[cfg(windows)] tray_opening:None,
            #[cfg(windows)] hidden:false,
            #[cfg(windows)] backdrop:None,
            #[cfg(windows)]
            native: daozai::native::Window::new(cc, tray.is_some()),
            #[cfg(windows)]
            tray,
            #[cfg(windows)]
            autostart,
            #[cfg(windows)]
            bt_rx: None,
            bt: Vec::new(),
            bt_ready: false,
            volume_quiet: PollClock::default(),
            mail: None,
            mail_sig: None,
            cal: None,
            cal_sig: None,
            quota: daozai::quota::Service::start(), quota_sig: None, quota_at: PollClock::default(),
        }
    }

    fn check_codex_thread(&mut self, ctx: &egui::Context, session: String) {
        let (tx, rx) = std::sync::mpsc::channel();
        let ctx = ctx.clone();
        let worker = std::thread::Builder::new().name("codex-thread-check".into()).spawn(move || {
            let result = codex_tasks::local_thread_url(&codex_tasks::codex_home(), &session);
            if tx.send(result).is_ok() {
                ctx.request_repaint();
            }
        });
        if worker.is_ok() {
            self.codex_open_rx = Some(rx);
        } else {
            self.codex_open_rx = None;
            self.app.say_below("暂时无法检查 Codex 会话，请重试");
        }
    }

    fn poll_codex_open(&mut self) {
        use std::sync::mpsc::TryRecvError;
        let Some(rx) = &self.codex_open_rx else { return; };
        match rx.try_recv() {
            Err(TryRecvError::Empty) => return,
            Ok(Ok(url)) => {
                if open_path(std::path::Path::new(&url)).is_err() {
                    self.app.say_below("无法打开会话，请检查 ChatGPT 桌面应用");
                }
            }
            Ok(Err(ThreadLinkError::InvalidId)) => self.app.say_below("此提醒缺少可用的 Codex 会话标识"),
            Ok(Err(ThreadLinkError::MissingRollout)) => self.app.say_below("本机未找到这条 Codex 会话记录"),
            Err(TryRecvError::Disconnected) => self.app.say_below("暂时无法检查 Codex 会话，请重试"),
        }
        self.codex_open_rx = None;
    }

    #[cfg(windows)]
    fn poll_tray(&mut self, ctx: &egui::Context) {
        use daozai::tray::{self, Event};
        let events = self.tray.as_ref().map(|t| t.poll()).unwrap_or_default();
        for e in events {
            match e {
                Event::Quit => {
                    self.app.save_window_before_exit();
                    diagnostics::session_event("托盘请求退出");
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
                Event::OpenSettings=>self.app.open_settings(),
                Event::OpenCalendar=>self.app.open_calendar(None),
                Event::OpenMessages=>self.app.open_integrations(false),
                Event::OpenClipboard=>self.app.open_integrations(true),
                Event::ConnectNotifications=>self.connect_notifications(ctx,false),
                Event::TogglePet => {
                    self.app.set_pet(!self.app.settings.pet);
                    self.app.say(if self.app.settings.pet { "吉祥物已打开" } else { "吉祥物已关闭" });
                }
                Event::ToggleMinimal => {
                    self.app.set_minimal(!self.app.settings.minimal);
                    self.app.say(if self.app.settings.minimal { "极简刘海已打开" } else { "极简刘海已关闭" });
                }
                Event::ToggleAutostart => {
                    let want = !self.autostart;
                    match tray::set_autostart(want) {
                        Ok(()) => {
                            self.autostart = want;
                            self.app.say(if want { "已设为开机启动" } else { "已取消开机启动" });
                        }
                        Err(e) => self.app.say(e),
                    }
                }
            }
        }
        if let Some(t) = &self.tray {
            t.sync(tray::State { pet: self.app.settings.pet, minimal: self.app.settings.minimal, autostart: self.autostart });
        }
    }

    #[cfg(windows)]
    fn connect_notifications(&mut self, ctx: &egui::Context, qq_only: bool){
        self.app.settings.qq_only=qq_only;
        self.app.settings.read_notifications=true;self.app.settings.save();self.incoming=None;
        match daozai::system::connect(ctx.clone(),messenger::NotificationFilter::for_settings(&self.app.settings)){Ok(rx)=>{self.incoming=Some(rx);self.app.listener_status="正在请求 Windows 通知权限…".into();},Err(e)=>self.app.listener_status=format!("Windows 未允许读取通知：{}。需要相应的系统权限或应用身份。",e.code())}
    }

    fn env(&mut self, ctx: &egui::Context) -> Env {
        if let Some((source,rx))=&self.music_volume_rx {
            if let Ok(success)=rx.try_recv() {
                if !success && self.media.lock().is_ok_and(|media|media.source==*source) {
                    self.app.say("音乐应用音量暂不可调，请稍后重试");
                }
                self.music_volume_rx=None;
            }
        }
        let (clock, today, date) = local_now();
        // 电量变化很慢，每 5 秒读一次就够
        if self.sys.2.ready(Duration::from_secs(5)) {
            let (b, c) = read_power();
            if self.app.settings.show_battery && self.sys.0.is_some() && self.sys.1!=c && self.app.settings.system_hud {self.app.alert(if c {"电源已连接"}else{"已切换电池供电"},b.map(|v|format!("当前电量 {v}%")).unwrap_or_else(||"桌面电源状态已更新".into()),5,false);}
            if self.app.settings.show_battery && b.is_some_and(|v|v<=20) && !c && self.sys.0.is_some_and(|v|v>20) {self.app.alert("电量偏低",format!("剩余 {}% · 请连接电源",b.unwrap()),5,true);}
            self.sys = (b, c, PollClock::now());
        }
        let ppp = ctx.pixels_per_point();
        let origin = ctx.input(|i| i.viewport().outer_rect).map(|r| r.min)
            .unwrap_or(Pos2::new(self.win_x, self.placement_gap as f32));
        #[cfg(windows)]
        let origin=self.native.as_ref().and_then(|w|w.logical_origin(ppp)).unwrap_or(origin);
        let cursor = cursor_logical(ppp).map(|(x, y)| Pos2::new(x - origin.x, y - origin.y));
        // 所在屏幕换算到窗口逻辑坐标，供通知详情等浮层避开屏幕边缘。
        #[cfg(windows)]
        let screen=daozai::display_layout::selected(&self.app.displays,&self.app.settings.display_id).map(|d|{
            // 与岛体摆放用同一范围：避开顶栏后，拖动换边和浮层都以顶栏下沿为顶边。
            let b=daozai::display_layout::placement_bounds(d,self.app.settings.avoids_top_bar());
            egui::Rect::from_min_max(Pos2::new(b.left as f32/ppp-origin.x,b.top as f32/ppp-origin.y),Pos2::new(b.right as f32/ppp-origin.x,b.bottom as f32/ppp-origin.y))
        });
        #[cfg(not(windows))]
        let screen=None;
        #[cfg(windows)]
        let (backdrop,glass_blur)=(self.backdrop.as_ref().and_then(|b|b.color()),self.native.as_ref().is_some_and(|w|w.blur_active()));
        #[cfg(not(windows))]
        let (backdrop,glass_blur)=(None,false);
        Env {
            t: self.t0.elapsed().as_secs_f64(),
            clock,
            today,
            date,
            battery: self.sys.0,
            charging: self.sys.1,
            network: Some(self.system_state.network),
            cursor,
            idle_seconds: {
                #[cfg(windows)] {daozai::system::idle_seconds()}
                #[cfg(not(windows))] {None}
            },
            screen_locked: {
                #[cfg(windows)] {daozai::system::screen_locked()}
                #[cfg(not(windows))] {false}
            },
            fullscreen_hidden: {
                #[cfg(windows)] {self.hidden}
                #[cfg(not(windows))] {false}
            },
            browser_at_top: {
                #[cfg(windows)] {if self.app.settings.browser_minimal {self.native.as_mut().and_then(|native|native.browser_at_top(&self.app.settings))}else{None}}
                #[cfg(not(windows))] {None}
            },
            media: self.media.lock().map(|m| m.clone()).unwrap_or_default(),
            volume: self.system_state.volume,
            bt: self.bt.clone(),
            bt_ready: self.bt_ready,
            screen,
            backdrop,
            glass_blur,
            opaque_surface: {
                #[cfg(windows)] {self.native.as_ref().is_some_and(|w|w.opaque_surface())}
                #[cfg(not(windows))] {false}
            },
        }
    }
}

impl eframe::App for Shell {
    fn raw_input_hook(&mut self,_ctx:&egui::Context,input:&mut egui::RawInput) {
        if input.viewport_id==egui::ViewportId::ROOT {self.app.filter_todo_input(input);}
    }

    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        [0.0; 4]
    }

    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let frame_start=Instant::now();
        if self.resource_at.ready(Duration::from_secs(30)){
            let (count,bytes)={let textures=ctx.tex_manager();let textures=textures.read();(textures.num_allocated(),textures.allocated().map(|(_,info)|info.bytes_used()).sum())};
            daozai::resources::TEXTURES.store(count,std::sync::atomic::Ordering::Relaxed);
            daozai::resources::TEXTURE_BYTES.store(bytes,std::sync::atomic::Ordering::Relaxed);
            daozai::resources::GALLEYS.store(ctx.fonts(|fonts|fonts.num_galleys_in_cache()),std::sync::atomic::Ordering::Relaxed);
            self.resource_at=PollClock::now();
        }
        let guarded=daozai::resources::GUARD.tripped();
        if guarded && !self.guard_notified {
            // 资源保护熔断：停止音乐会话轮询（含封面、网易云日志）与歌词查询，保住用户电脑，重启后恢复。
            self.guard_notified=true;self.app.opts.fetch_lyrics=false;
            self.app.alert("浮岛已暂停音乐功能","检测到资源占用异常，已暂停读取音乐信息和歌词。重启浮岛即可恢复；诊断记录在数据目录的 diagnostics 文件夹。",0,false);
        }
        self.media_enabled.store(self.app.settings.visible_pages[0] && !guarded,std::sync::atomic::Ordering::Relaxed);
        if self.media_fast_until.is_some_and(|until|Instant::now()>=until){self.media_fast_until=None;}
        self.media_enabled.set_interval(if self.media_fast_until.is_some(){smtc::POLL_FAST}else{self.app.media_poll_interval()});
        // 设置里改了岛体大小：下一帧起主窗口按新倍率栅格化，定位逻辑随像素比变化同步调整画布。
        #[cfg(windows)] eframe::set_root_viewport_scale(self.app.settings.island_scale());
        #[cfg(windows)]
        {
            // 用帧间隔限制刷新，绕过部分 Intel 驱动在垂直同步中的忙等待。
            let delay=Duration::from_secs_f64(1.0/60.0).saturating_sub(self.frame_at.elapsed());
            if !delay.is_zero(){std::thread::sleep(delay);}
            self.frame_at=Instant::now();
        }
        #[cfg(windows)]
        self.poll_tray(ctx);
        #[cfg(windows)]
        if daozai::native::take_island_close_request() {
            diagnostics::session_event("收到岛体关闭请求，继续在托盘运行");
        }
        #[cfg(windows)]
        let foreground_changed=daozai::native::take_foreground_change();
        #[cfg(windows)]
        if foreground_changed {
            // 切到别的程序：收起，并在光标离开前不再自动展开
            self.app.foreground_changed();
        }
        let top_gap = self.app.settings.island_top_gap;
        #[cfg(windows)]
        let offset=self.app.placement_offset();
        #[cfg(windows)]
        let shift=self.oled_shift;
        // 拖动时位置不取整，按万分之一比例比较，窗口逐帧平滑跟随。
        #[cfg(windows)]
        let placement_key=(self.app.settings.display_id.clone(),(offset*10000.0).round() as i32,self.app.settings.dock,self.app.settings.avoids_top_bar(),shift);
        #[cfg(windows)]
        let refresh_placement = !self.placed || self.placement_scale!=ctx.pixels_per_point() || self.placement_key!=placement_key
            || self.placement_gap!=top_gap || self.placement_at.ready(Duration::from_secs(1));
        #[cfg(windows)]
        if refresh_placement {
            self.placement_at=PollClock::now();
            self.placement_scale=ctx.pixels_per_point();
            self.placement_key=placement_key;
            self.placement_gap=top_gap;
            self.app.displays=daozai::display_layout::connected();
            let display=daozai::display_layout::selected(&self.app.displays,&self.app.settings.display_id);
            if display.is_some_and(|d|self.native.as_ref().is_some_and(|w|w.place_on_display(self.placement_scale,daozai::display_layout::placement_bounds(d,self.app.settings.avoids_top_bar()),offset,self.app.settings.dock,top_gap,shift))) {
                if !self.placed {diagnostics::startup("目标屏幕物理坐标定位完成，已请求显示");}
                self.placed=true;
                ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
            } else if self.t0.elapsed()>=Duration::from_secs(2) {
                ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
            }
        }
        #[cfg(not(windows))]
        if !self.placed || self.placement_gap != top_gap {
            if let Some(mon)=ctx.input(|i|i.viewport().monitor_size) {
                self.placed=true;self.win_x=((mon.x-app::WIN_W)/2.0).round();
                self.placement_gap = top_gap;
                ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(egui::pos2(self.win_x, top_gap as f32)));
                ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
            }
        }
        #[cfg(windows)]
        {
            while let Ok(observation)=self.system_rx.try_recv(){
                let new=observation.system;
                // 切换输出设备时，系统会恢复该设备的音量；等待它稳定后再提示调节。
                if new.audio_endpoint!=self.system_state.audio_endpoint {self.volume_quiet=PollClock::now();}
                if self.system_ready && self.app.settings.system_hud {
                    for (title,body) in new.changes(&self.system_state) {
                        // 拖动音量条刚改过音量时，系统回声不再单独弹一条。
                        if title=="系统音量" && !self.volume_quiet.ready(Duration::from_millis(900)) { continue; }
                        self.app.alert(title,body,5,false);
                    }
                }
                self.system_state=new;
                self.system_ready=true;
                self.fullscreen=observation.fullscreen;
            }
            self.hidden=daozai::display_layout::hide_for_fullscreen(self.app.settings.hide_fullscreen,self.fullscreen,daozai::display_layout::selected(&self.app.displays,&self.app.settings.display_id).map(|d|d.bounds));
            // 毛玻璃：在岛体左右两侧和画布底边（都在窗口区域之外）取样，估计岛背后的明暗。
            if self.app.settings.skin.is_glass() && !self.hidden {
                let sampler=self.backdrop.get_or_insert_with(daozai::backdrop::Sampler::start);
                let ppp=ctx.pixels_per_point();
                if let Some(origin)=self.native.as_ref().and_then(|w|w.logical_origin(ppp)) {
                    let points=self.app.backdrop_points();
                    sampler.set_points(points.iter().map(|&(x,y)|(((origin.x+x)*ppp).round() as i32,((origin.y+y)*ppp).round() as i32)).collect());
                }
            } else {
                self.backdrop=None;
            }
            if self.app.settings.bluetooth_notifications {
                if self.bt_rx.is_none() {self.bt_rx=Some(daozai::bt::spawn());}
            } else {
                self.bt_rx=None;
                self.bt.clear();
                self.bt_ready=false;
            }
            if let Some(service)=&self.bt_rx {
                while let Ok(found)=service.rx.try_recv(){ self.bt=found; self.bt_ready=true; }
            }
            let previous_pending=self.app.pending_messengers.clone();
            for event in self.messengers.poll_taskbar(ctx,&self.app.settings){match event{
                TaskbarEvent::Status(client,status)=>{
                    if client==app::Messenger::Wechat {
                        self.app.wechat_status=status.clone();
                        self.app.wechat_listener_active=status.starts_with("正在监听");
                    } else if client==app::Messenger::WeCom {
                        self.app.wecom_listener_active=status.starts_with("正在监听");
                    }
                    self.app.set_messenger_status(client,status);
                },
                TaskbarEvent::Message(unread)=>{
                    if self.messengers.track_unread(unread)==Err(OpenError::Exited){continue;}
                    if unread.source==app::Messenger::Wechat {
                        self.app.wechat_listener_active=true;
                        self.app.receive_wechat_reminder_from(unread.pid,unread.window);
                    } else {
                        self.app.receive_messenger_reminder(unread);
                    }
                }
            }}
            if let Some(rx)=&self.incoming {
                rx.taskbar_settings(&self.app.settings);
                while let Ok(event)=rx.try_recv(){match event {
                    daozai::system::Incoming::Revoked=>{self.app.settings.read_notifications=false;self.app.settings.save();},
                    daozai::system::Incoming::Status(s)=>self.app.listener_status=s,
                    daozai::system::Incoming::Message(a,id,b,target)=>{
                        if target.is_some_and(|target|self.messengers.track(target)==Err(OpenError::Exited)){continue;}
                        self.app.receive_notification_from(a,id,b,target.map(|target|target.unread));
                    }
                }}
            }
            if self.app.pending_messengers!=previous_pending || self.unread_at.ready(Duration::from_secs(1)){
                self.clear_exited_messengers(ctx);
            }
            if !self.app.pending_messengers.is_empty(){ctx.request_repaint_after(Duration::from_secs(1));}
            if foreground_changed || self.app.pending_messengers!=previous_pending {
                // 在微信 / QQ 里打开了某个账号的窗口：清掉这个账号的未读。
                let mut sources:Vec<app::Messenger>=self.app.pending_messengers.iter().map(|u|u.source).collect();sources.dedup();
                match self.messengers.foreground(&sources){
                    Some((source,pid))=>{
                        if self.app.pending_messengers.iter().any(|u|u.source==source){messenger::windows::trace(format!("岛上清除 {} 账号 {pid}（前台切换={foreground_changed}）",source.name()));}
                        self.messengers.viewed_client(source,pid);
                        self.app.viewed_client(source,pid);
                    }
                    None=>if foreground_changed && !sources.is_empty(){messenger::windows::trace(format!("前台切换但不是消息客户端主窗口：{}；岛上未读 {:?}",messenger::windows::describe_foreground(),self.app.pending_messengers.iter().map(|u|(u.source.name(),u.pid)).collect::<Vec<_>>()));},
                }
            }
            if self.tray_opening.as_ref().is_some_and(|(unread,_)|!self.app.pending_messengers.contains(unread)){
                self.tray_opening=None;
            }
            if let Some((unread,rx))=&self.tray_opening {
                let unread=*unread;
                match rx.try_recv(){
                    Ok(Ok(()))=>{
                        self.tray_opening=None;
                        if !self.confirm_messenger_open(unread){
                            self.opening_messenger=Some((unread,Instant::now()));
                            ctx.request_repaint_after(Duration::from_secs(4));
                        }
                    }
                    Ok(Err(error))=>{
                        self.tray_opening=None;
                        if error==OpenError::Exited{self.app.discard_unread(unread);}
                        else if unread.source.adapter().dismiss_unread{self.app.reveal_messenger_notice(unread);}
                        self.app.messenger_unavailable(error.message(unread.source));
                    }
                    Err(std::sync::mpsc::TryRecvError::Disconnected)=>{
                        messenger::windows::trace(format!("唤起 {} 账号 {}：唤起线程异常结束，未返回结果",unread.source.name(),unread.pid));
                        self.tray_opening=None;self.app.messenger_unavailable(format!("{} 暂时无法响应，请重试",unread.source.name()));
                    }
                    Err(std::sync::mpsc::TryRecvError::Empty)=>{}
                }
            }
            if let Some((unread,started))=self.opening_messenger {
                if !self.app.pending_messengers.contains(&unread){self.opening_messenger=None;}
                else if started.elapsed()>=Duration::from_secs(4){
                    self.opening_messenger=None;
                    if !self.confirm_messenger_open(unread){
                        self.app.messenger_unavailable(format!("{}尚未切到前台，可点图标重试",unread.source.name()));
                    }
                }
            }
        }
        if let Some((message,rx))=&self.seek_rx {
            if let Some(success)=match rx.try_recv(){Ok(success)=>Some(success),Err(std::sync::mpsc::TryRecvError::Disconnected)=>Some(false),Err(_)=>None}{
                let message=*message;
                self.seek_rx=None;
                if !success{self.app.say(message);}
            }
        }
        self.poll_codex_open();
        let env = self.env(ctx);
        self.reconcile_modules();
        // v0.5.5 邮件：开关 / 间隔 / 账号清单变了就重新对齐收信线程，再把事件交给界面。
        // 每帧只比较不复制，确有变化时才复制账号清单。
        if let Some(mail)=&self.mail {
            let (mail_enabled,mail_interval)=(self.app.mail_active(),self.app.settings.mail_interval);
            if !self.mail_sig.as_ref().is_some_and(|known|(known.0,known.1)==(mail_enabled,mail_interval) && known.2.as_slice()==self.app.mail_accounts()){
                let sig=(mail_enabled,mail_interval,self.app.mail_accounts().to_vec());
                let accounts:&[daozai::mail::Account]=if sig.0{ &sig.2 }else{ &[] };
                mail.sync(accounts,sig.1);
                self.mail_sig=Some(sig);
            }
            while let Some(ev)=mail.try_recv(){self.app.mail_event(ev);}
        }
        while let Some(ev)=self.integrations.try_recv(){self.app.integration_event(ev);}
        while let Some(snapshot)=self.clipboard.try_recv(){self.app.clipboard_snapshot(snapshot);}
        // 日程连接或自动同步开关变化时下发给工作线程。
        // 录制/截图演示用的是假账号，别让真服务去连（会立刻失败、把状态改成红色）。
        #[cfg(feature="record")]
        let cal_demo=std::env::var("DAOZAI_CAL_DEMO").is_ok();
        #[cfg(not(feature="record"))]
        let cal_demo=false;
        if let Some(service)=&self.cal {
            let auto_sync=self.app.settings.calendar_auto_sync;
            let cal_same=self.cal_sig.as_ref().is_some_and(|(known,known_sync)|*known_sync==auto_sync && if cal_demo{known.is_empty()}else{self.app.calendar_configs_match(known)});
            if !cal_same{
                let cal=if cal_demo{Vec::new()}else{self.app.calendar_configs()};
                service.configure(cal.clone(),auto_sync);
                self.cal_sig=Some((cal,auto_sync));
            }
            while let Some(ev)=service.try_recv(){self.app.calendar_event(ev);}
        }
        #[cfg(feature="record")] let quota_demo=std::env::var("DAOZAI_QUOTA_DEMO").is_ok();
        #[cfg(not(feature="record"))] let quota_demo=false;
        if !quota_demo {
            let (active,config)=(self.app.quota_active(),self.app.quota_config());
            if !self.quota_sig.as_ref().is_some_and(|known|quota_sig_matches(known,active,config)) || self.quota_at.ready(Duration::from_secs(180)){
                let sig=quota_sig(active,config);
                let accounts:Vec<_>=sig.1.iter().filter(|a|sig.0 && a.enabled).cloned().collect();
                if self.quota.refresh(accounts.clone()){self.app.quota_pending(&accounts);self.quota_sig=Some(sig);self.quota_at=PollClock::now();}
            }
            while let Some(ev)=self.quota.try_recv(self.app.quota_config()){self.app.quota_event(ev);}
        }
        #[cfg(windows)]
        if let Some(native) = &mut self.native {
            let settings = &self.app.settings;
            native.update_sampled_backdrop(ctx, settings.uses_leidao_glass(),
                settings.glass_blur && !self.hidden && self.placed);
        }
        let mut out = None;
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(Color32::TRANSPARENT))
            .show(ctx, |ui| {
                // 左右停靠时画布是竖向的 SIDE_W × SIDE_H。
                let size=if self.app.settings.dock.is_side() {egui::vec2(daozai::side_dock::SIDE_W,daozai::side_dock::SIDE_H)} else {egui::vec2(app::WIN_W,app::WIN_H)};
                let win = egui::Rect::from_min_size(ui.max_rect().min,size);
                out = Some(self.app.frame(ui, win, &env));
            });
        let Some(out) = out else { return };

        #[cfg(windows)]
        {
            // 先处理本帧输入和动画，再决定是否移动，避免悬停或按下的首帧仍使用旧状态。
            let hovering=env.cursor.is_some_and(|p|out.hit.contains(p)||out.notice_hit.is_some_and(|r|r.contains(p)));
            let shift=if !self.app.settings.oled_shift {(0,0)}
                else if !self.hidden && !hovering && self.app.oled_shift_idle(ctx) {
                    ctx.request_repaint_after(daozai::oled_shift::until_next_step(self.t0.elapsed()));
                    let target=daozai::oled_shift::shift(true,self.app.settings.dock,self.t0.elapsed());
                    daozai::oled_shift::toward(self.oled_shift,target)
                } else {self.oled_shift};
            if shift!=self.oled_shift {
                self.oled_shift=shift;
                let display=daozai::display_layout::selected(&self.app.displays,&self.app.settings.display_id);
                if display.is_some_and(|d|self.native.as_ref().is_some_and(|w|w.place_on_display(ctx.pixels_per_point(),daozai::display_layout::placement_bounds(d,self.app.settings.avoids_top_bar()),self.app.placement_offset(),self.app.settings.dock,top_gap,shift))) {
                    self.placement_key.4=shift;
                }
                ctx.request_repaint();
            }
        }

        #[cfg(windows)]
        if let Some(native) = &mut self.native {
            // 兼容渲染：读回的画面交给分层窗口，边缘保留抗锯齿。画面变化（或有其他输入）时才请求下一次读回，
            // 读回完成本身触发的那一帧若画面没变就停下，静止时不会一直空转。
            if native.wants_frames() {
                let (shot,other_input)=ctx.input(|i|(
                    i.events.iter().rev().find_map(|e|match e{egui::Event::Screenshot{image,..}=>Some(image.clone()),_=>None}),
                    i.events.iter().any(|e|!matches!(e,egui::Event::Screenshot{..})),
                ));
                let changed=shot.as_ref().is_some_and(|image|native.present_frame(image.size,image.as_raw()));
                if shot.is_none() || changed || other_input {
                    // 拖动、悬停等有输入时满帧跟手；歌词滚动这类自己在动的画面约 30 帧/秒读回，
                    // 未到间隔就预约一次重绘，到时补上读回，动画停下时的最后一帧不会漏掉。
                    let now=Instant::now();
                    match daozai::layered::readback_delay(self.readback_at,now,other_input) {
                        None=>{self.readback_at=Some(now);ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(Default::default()));}
                        Some(wait)=>ctx.request_repaint_after(wait),
                    }
                }
            }
            let r = |r: egui::Rect| (r.left(), r.top(), r.width(), r.height());
            // 保留主窗口的事件循环，避免全屏隐藏后设置窗口无法关闭或重新打开。
            let region=if self.hidden {(0.0,0.0,0.0,0.0)} else {r(out.region)};
            let areas = if self.hidden { Vec::new() } else { out.pointer_areas };
            let center = areas.first().map(|area| area.rect().center());
            native.update(region, areas, if self.hidden{None}else{out.file_drop_zone.map(r)},if self.hidden{None}else{out.glass.as_deref()},ctx.pixels_per_point(), out.editing, !self.hidden && out.expanded);
            let icon = if self.hidden { egui::CursorIcon::Default } else { ctx.output(|output| output.cursor_icon) };
            if native.set_drag_cursor(icon) {
                // winit 在 Windows 上会把 Grab 和 Grabbing 都映射为四向箭头。
                ctx.set_cursor_icon(egui::CursorIcon::Default);
            }
            if !self.hidden && self.app.settings.avoid_top_bar && self.app.settings.top_bar_on_top && !self.app.settings.dock.is_side() {
                if let Some(display)=daozai::display_layout::selected(&self.app.displays,&self.app.settings.display_id) {
                    native.keep_above_top_bar(display.bounds);
                }
            }
            if refresh_placement && center.is_some_and(|point| native.restore_if_covered(point, ctx.pixels_per_point())) {
                diagnostics::session_event("岛体被普通窗口盖住，已恢复置顶");
            }
        }
        for cmd in out.cmds {
            match cmd {
                Cmd::Clipboard(command)=>self.clipboard.send(command),
                Cmd::ActivateMessenger(source)=>{
                    #[cfg(windows)] if let Err(error)=daozai::messengers::activate(source){self.app.say(error);}
                },
                Cmd::OpenCodexThread(session) => {
                    self.app.acknowledge_codex_notice(&session);
                    self.check_codex_thread(ctx, session);
                },
                Cmd::InstallCodexHooks=>{
                    self.app.integrations.hook_status=match std::env::current_exe().map_err(|e|e.to_string()).and_then(|exe|daozai::codex_tasks::install_hooks(&daozai::codex_tasks::codex_home(),&exe,&daozai::codex_tasks::backup_dir())){
                        Ok(_)=>"接入已安装。请重启 Codex，并在 Hook 管理中审阅信任新增项。".into(),
                        Err(error)=>format!("接入失败：{error}"),
                    };
                },
                Cmd::Media(t) => {
                    self.media_fast_until=Some(Instant::now()+Duration::from_secs(3));self.media_enabled.set_interval(smtc::POLL_FAST);
                    let source=self.media.lock().map(|media|media.source.clone()).unwrap_or_default();
                    let message=if source.eq_ignore_ascii_case("cloudmusic.exe"){"网易云暂未适配进度跳转，请在客户端调整"}
                        else if source.eq_ignore_ascii_case("bodian_pc.exe"){"波点音乐暂未适配进度跳转，请在客户端调整"}
                        else if source.eq_ignore_ascii_case("kwmusic.exe"){"酷我音乐暂未适配进度跳转，请在客户端调整"}
                        else{"播放器未完成跳转，请打开客户端后重试"};
                    if self.media_control.is_none(){self.media_control=smtc::ControlWorker::start(ctx.clone());}
                    match self.media_control.as_ref().ok_or(()).and_then(|worker|worker.submit(t,source)){
                        Ok(Some(rx))=>self.seek_rx=Some((message,rx)),Ok(None)=>{},Err(())=>self.app.say("播放器正在响应，请稍后重试"),
                    }
                }
                Cmd::MusicVolume(command,source)=>{
                    self.media_fast_until=Some(Instant::now()+Duration::from_secs(3));self.media_enabled.set_interval(smtc::POLL_FAST);
                    if self.media_control.is_none(){self.media_control=smtc::ControlWorker::start(ctx.clone());}
                    match self.media_control.as_ref().ok_or(()).and_then(|worker|worker.submit(command,source.clone())){
                        Ok(Some(rx))=>self.music_volume_rx=Some((source,rx)),
                        _=>self.app.say("音乐应用音量暂不可调，请稍后重试"),
                    }
                }
                Cmd::ViewMessenger(unread)=>{
                    #[cfg(windows)] {
                        let was_pending=self.app.pending_messengers.contains(&unread);
                        self.clear_exited_messengers(ctx);
                        if !self.app.pending_messengers.contains(&unread){
                            if was_pending{
                                messenger::windows::trace(format!("唤起 {} 账号 {}：未开始，账号进程已退出",unread.source.name(),unread.pid));
                                self.app.messenger_unavailable(OpenError::Exited.message(unread.source));
                            }
                            continue;
                        }
                        if self.tray_opening.is_some(){
                            messenger::windows::trace(format!("唤起 {} 账号 {}：未开始，上一次唤起还没结束",unread.source.name(),unread.pid));
                            self.app.messenger_unavailable(OpenError::Busy.message(unread.source));ctx.request_repaint();continue;
                        }
                        match self.messengers.request(unread,&self.app.settings).start(ctx.clone()){
                            Ok(rx)=>self.tray_opening=Some((unread,rx)),
                            Err(error)=>{
                                messenger::windows::trace(format!("唤起 {} 账号 {}：未开始 {error:?}",unread.source.name(),unread.pid));
                                if error==OpenError::Unidentified{self.app.reveal_messenger_notice(unread);}
                                self.app.messenger_unavailable(error.message(unread.source));ctx.request_repaint();
                            }
                        }
                    }
                }
                Cmd::RestartApp=>{
                    // 先拉起新进程（它会等本进程退出），再像托盘「退出」一样摘图标后结束。
                    if diagnostics::spawn_replacement(){
                        self.app.save_window_before_exit();
                        #[cfg(windows)] if let Some(tray)=&self.tray {tray.exit_process();}
                        std::process::exit(0);
                    }else{self.app.say("无法重新启动浮岛，请从托盘退出后手动打开");}
                }
                Cmd::Open(p) => {if let Err(e)=open_path(&p){self.app.say(e);}},
                Cmd::OpenUrl(url)=>{
                    if let Some(url)=daozai::mail::web_url(&url) {
                        if open_path(std::path::Path::new(&url)).is_err(){self.app.say("无法打开默认浏览器，请检查 Windows 默认应用设置");}
                    }
                },
                Cmd::CalendarImport=>{
                    #[cfg(windows)] match daozai::system::pick_files_for("选择要导入的日历文件（.ics）",true){
                        Ok(files)=>if !files.is_empty(){
                            let read:Vec<(String,String)>=files.iter().map(|path|{
                                let name=path.file_name().map(|n|n.to_string_lossy().to_string()).unwrap_or_default();
                                let text=std::fs::read(path).ok().filter(|raw|raw.len()<=20*1024*1024).map(|raw|String::from_utf8_lossy(&raw).into_owned()).unwrap_or_default();
                                (name,text)
                            }).collect();
                            let note=self.app.import_ics(&read);self.app.say(note);
                        },
                        Err(e)=>self.app.say(format!("无法选择文件：{}",e.code())),
                    }
                },
                Cmd::AddFiles=>{
                    #[cfg(windows)] match daozai::system::pick_files(){Ok(files)=>if !files.is_empty(){self.app.on_drop(files);self.app.show_page(3);},Err(e)=>self.app.say(format!("无法选择文件：{}",e.code()))}
                },
                Cmd::ExportConfig => {
                    #[cfg(windows)]
                    match daozai::system::save_config_file() {
                        Ok(Some(path)) => self.app.export_config(&path),
                        Ok(None) => {}
                        Err(e) => self.app.config_sync_error(format!("无法打开保存窗口（{}）", e.code())),
                    }
                    #[cfg(not(windows))]
                    self.app.config_sync_error("此平台暂不支持配置文件对话框");
                },
                Cmd::ChooseConfig => {
                    #[cfg(windows)]
                    match daozai::system::pick_files_for("选择浮岛配置文件", false) {
                        Ok(files) => if let Some(path) = files.first() { self.app.preview_config(path); },
                        Err(e) => self.app.config_sync_error(format!("无法打开文件选择窗口（{}）", e.code())),
                    }
                    #[cfg(not(windows))]
                    self.app.config_sync_error("此平台暂不支持配置文件对话框");
                },
                Cmd::ApplyConfig => self.app.apply_config(),
                cmd @ (Cmd::WebDavConnect | Cmd::WebDavUpload | Cmd::WebDavDownload) => {
                    self.app.start_config_webdav(cmd, ctx);
                },
                Cmd::DragFile(p)=>{
                    #[cfg(windows)] if let Err(e)=daozai::system::drag_file(&p){self.app.say(format!("无法拖出文件：{}",e.code()));}
                    ctx.input_mut(|i| i.pointer=Default::default());
                },
                Cmd::DisconnectNotifications=>{#[cfg(windows)]{self.incoming=None;self.app.settings.read_notifications=false;self.app.settings.save();self.app.listener_status="已断开，不再读取 Windows 通知".into();}},
                Cmd::Reveal(p)=>{let _=daozai::child_process::spawn(std::process::Command::new("explorer.exe").arg(format!("/select,{}",p.display())));},
                Cmd::QuotaChooseFile=>{
                    #[cfg(windows)] match daozai::system::pick_files_for("选择 AI 账号的登录 JSON 文件",false){Ok(files)=>if let Some(path)=files.into_iter().next(){self.app.quota_set_source(path);},Err(_)=>self.app.say("无法选择登录文件")}
                },
                Cmd::QuotaRefresh=>{
                    if self.quota_at.ready(Duration::from_secs(15)) {
                        let (active,config)=(self.app.quota_active(),self.app.quota_config());let accounts:Vec<_>=config.accounts.iter().filter(|a|active && a.enabled).cloned().collect();
                        if self.quota.refresh(accounts.clone()){self.app.quota_pending(&accounts);self.quota_at=PollClock::now();}
                    }else{self.app.say("请稍后再刷新");}
                },
                Cmd::QuotaConnected=>{self.quota_sig=None;},
                Cmd::ToggleAutostart=>{
                    #[cfg(windows)] {let want=!self.autostart;match daozai::tray::set_autostart(want){Ok(())=>self.autostart=want,Err(e)=>self.app.say(e)}}
                },
                Cmd::SetVolume(v)=>{
                    #[cfg(windows)] daozai::system::set_volume(v);
                    self.volume_quiet=PollClock::now();
                },
                Cmd::ToggleMute=>{
                    #[cfg(windows)] daozai::system::toggle_mute();
                    self.volume_quiet=PollClock::now();
                },
                Cmd::Copy(text)=>{ctx.copy_text(text);self.app.say("已复制");},
                Cmd::Notify(title, body, sound) => {
                    #[cfg(windows)] {
                        if sound == app::NotificationSound::Health {
                            daozai::tray::play_health_sound();
                        }
                        if let Some(tray) = &self.tray {
                            tray.notify(&title, &body, sound == app::NotificationSound::System);
                        }
                    }
                },
                cmd @ (Cmd::RequestNotifications|Cmd::RequestQqNotifications)=>{
                    #[cfg(windows)] self.connect_notifications(ctx, matches!(cmd,Cmd::RequestQqNotifications));
                },
                // 模块关闭时设置项已置灰；万一仍有命令到达，只提示，不为它临时启动服务。
                Cmd::MailSync=>if let Some(mail)=&self.mail {
                    let enabled=self.app.mail_active();
                    let accounts=if enabled{self.app.mail_accounts().to_vec()}else{Vec::new()};
                    mail.sync(&accounts,self.app.settings.mail_interval);
                    self.mail_sig=Some((enabled,self.app.settings.mail_interval,self.app.mail_accounts().to_vec()));
                },
                Cmd::MailTest(account,password)=>if let Some(mail)=&self.mail {mail.test(account,password)} else {self.app.say(MODULE_OFF)},
                Cmd::MailRestart(email)=>if let Some(mail)=&self.mail {mail.restart(email)},
                Cmd::OutlookLogin(account,id,attempt)=>if let Some(mail)=&self.mail {mail.outlook_login(account,id,attempt)} else {self.app.say(MODULE_OFF)},
                Cmd::OutlookCancel=>if let Some(mail)=&self.mail {mail.outlook_cancel()},
                Cmd::MailManage(request)=>if let Some(mail)=&self.mail {mail.manage(request)} else {self.app.say(MODULE_OFF)},
                Cmd::CalendarTest(cfg,password)=>if let Some(cal)=&self.cal {cal.test(cfg,password)} else {self.app.say(MODULE_OFF)},
                Cmd::CalendarSync=>if let Some(cal)=&self.cal {cal.sync_now()},
                Cmd::CalendarMonth(month)=>if let Some(cal)=&self.cal {cal.view_month(month)},
                Cmd::CalendarForget=>{
                    if let Err(error)=daozai::calendar::forget_config(){self.app.say(&error);continue;}
                    if let Some(old)=self.app.forget_calendar(){daozai::calendar::secrets::delete(&old);}
                    if let Some(cal)=&self.cal {
                        let configs=self.app.calendar_configs();
                        cal.configure(configs.clone(),self.app.settings.calendar_auto_sync);
                        self.cal_sig=Some((configs,self.app.settings.calendar_auto_sync));
                    }
                },
                Cmd::Focus => ctx.send_viewport_cmd(egui::ViewportCommand::Focus),
            }
        }


        #[cfg(windows)] let autostart = self.autostart;
        #[cfg(not(windows))] let autostart = false;
        self.app.poll_config_webdav();
        self.app.settings_ui(ctx, autostart);
        self.app.mail_reader_ui(ctx);
        if let Some(path)=std::env::var_os("DAOZAI_DIAGNOSTICS_FILE") {
            if self.diagnostic_clock.ready(Duration::from_millis(500)){
                let _=self.app.write_integration_diagnostics(std::path::Path::new(&path));
                self.diagnostic_clock=PollClock::now();
            }
        }
        #[cfg(windows)] let repaint_after=if self.hidden{Duration::from_secs(1)}else{out.repaint_after};
        #[cfg(not(windows))] let repaint_after=out.repaint_after;
        schedule_repaint(ctx,repaint_after);
        // 只记慢帧：卡顿反馈时能直接看出是渲染慢还是输入被吞（页面/展开状态一并记下）。
        let elapsed=frame_start.elapsed();
        if elapsed>=Duration::from_millis(60) {
            let (page,open)=self.app.debug_state();
            daozai::resources::append_log(&daozai::model::config_dir().join("diagnostics"),"frames",
                &format!("帧 {:.0} ms｜页面 {}｜展开 {}",elapsed.as_secs_f64()*1000.0,page,open));
        }
    }
}

// ---------------- 额度线程对齐 ----------------

/// 额度线程只关心总开关和账号身份；别名、置顶只影响显示，改了不重新查询。
/// `enabled` 是生效的开关：AI 额度模块开启且额度设置里打开。
fn quota_sig(enabled:bool,config:&daozai::quota::Config)->(bool,Vec<daozai::quota::Account>){
    (enabled,config.accounts.iter().cloned().map(|mut a|{a.alias.clear();a.pinned=false;a}).collect())
}

/// 与 `quota_sig(enabled,config)==*known` 等价，但每帧调用时不复制账号。
fn quota_sig_matches(known:&(bool,Vec<daozai::quota::Account>),enabled:bool,config:&daozai::quota::Config)->bool{
    known.0==enabled && known.1.len()==config.accounts.len() && known.1.iter().zip(&config.accounts).all(|(known,current)|{
        // 不用 `..`：账号新增字段时这里会编译失败，提醒决定新字段是否需要重新查询。
        let daozai::quota::Account{id,provider,alias:_,source,slot,enabled,pinned:_}=current;
        known.id==*id && known.provider==*provider && known.source==*source && known.slot==*slot && known.enabled==*enabled
    })
}

#[cfg(test)]
mod quota_sig_tests {
    use super::*;
    use daozai::quota::{Account,Config};

    fn account(id:&str)->Account{
        Account{id:id.into(),provider:daozai::quota::Provider::Claude,alias:"工作".into(),source:"a.json".into(),slot:None,enabled:true,pinned:false}
    }

    #[test]
    fn matching_without_copy_agrees_with_the_copied_signature(){
        let config=Config{accounts:vec![account("a"),account("b")],..Default::default()};
        let known=quota_sig(true,&config);
        assert!(quota_sig_matches(&known,true,&config));
        let mut display_only=config.clone();display_only.accounts[0].alias="改名".into();display_only.accounts[1].pinned=true;
        assert!(quota_sig_matches(&known,true,&display_only),"改别名、置顶不应重新查询");
        assert_eq!(quota_sig(true,&display_only),known);
        let changes:[fn(&mut Config);6]=[
            |c|c.enabled=false,
            |c|c.accounts[1].enabled=false,
            |c|c.accounts[0].source="b.json".into(),
            |c|c.accounts[0].slot=Some("org".into()),
            |c|{c.accounts.pop();},
            |c|c.accounts.swap(0,1),
        ];
        for change in changes {
            let mut changed=config.clone();change(&mut changed);
            assert!(!quota_sig_matches(&known,changed.enabled,&changed));
            assert_ne!(quota_sig(changed.enabled,&changed),known,"两种判断必须一致");
        }
        assert!(!quota_sig_matches(&known,false,&config),"关闭 AI 额度模块后应停止查询");
    }
}

// ---------------- 系统信息 ----------------

fn schedule_repaint(ctx:&egui::Context,interval:Duration){
    // egui 会预减一帧；本程序自行调度刷新，补回后才能保持实际的等待间隔。
    let predicted=ctx.input(|i|Duration::from_secs_f32(i.predicted_dt));
    ctx.request_repaint_after(interval.saturating_add(predicted));
}

#[cfg(test)]
mod repaint_tests {
    use super::*;

    fn scheduled_delay(interval:Duration,immediate:bool)->Duration{
        let ctx=egui::Context::default();
        let mut delay=Duration::ZERO;
        for frame in 0..4 {
            let output=ctx.run(egui::RawInput{time:Some(frame as f64),predicted_dt:1.0/60.0,..Default::default()},|ctx|{
                if immediate{ctx.request_repaint();}
                schedule_repaint(ctx,interval);
            });
            delay=output.viewport_output[&egui::ViewportId::ROOT].repaint_delay;
        }
        delay
    }

    #[test]
    fn background_repaint_keeps_the_requested_frame_interval(){
        for fps in [60.0,30.0,20.0,10.0,1.0] {
            let interval=Duration::from_secs_f64(1.0/fps);
            let actual=scheduled_delay(interval,false);
            assert!(actual.abs_diff(interval)<Duration::from_micros(1),"目标 {fps} 帧不应被提前唤醒：{actual:?}");
        }
    }

    #[test]
    fn interaction_and_service_events_still_repaint_immediately(){
        assert_eq!(scheduled_delay(Duration::from_secs(1),true),Duration::ZERO);
    }
}

#[cfg(windows)]
fn local_now() -> (String, String, String) {
    use windows::Win32::System::SystemInformation::GetLocalTime;
    let st = unsafe { GetLocalTime() };
    let wd = ["周日", "周一", "周二", "周三", "周四", "周五", "周六"];
    (
        format!("{:02}:{:02}:{:02}", st.wHour, st.wMinute, st.wSecond),
        format!("{:04}-{:02}-{:02}", st.wYear, st.wMonth, st.wDay),
        format!("{}月{}日 {}", st.wMonth, st.wDay, wd[(st.wDayOfWeek as usize).min(6)]),
    )
}

#[cfg(not(windows))]
fn local_now() -> (String, String, String) {
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let (h, m, s) = ((secs / 3600) % 24, (secs / 60) % 60, secs % 60);
    (format!("{h:02}:{m:02}:{s:02}"), "1970-01-01".into(), "1月1日 周四".into())
}

#[cfg(windows)]
fn read_power() -> (Option<u8>, bool) {
    use windows::Win32::System::Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS};
    let mut st = SYSTEM_POWER_STATUS::default();
    if unsafe { GetSystemPowerStatus(&mut st) }.is_ok() {
        let pct = if st.BatteryLifePercent == 255 { None } else { Some(st.BatteryLifePercent) };
        (pct, st.ACLineStatus == 1)
    } else {
        (None, false)
    }
}

#[cfg(not(windows))]
fn read_power() -> (Option<u8>, bool) {
    (None, true)
}

#[cfg(windows)]
fn cursor_logical(ppp: f32) -> Option<(f32, f32)> {
    use windows::Win32::Foundation::POINT;
    use windows::Win32::UI::WindowsAndMessaging::GetCursorPos;
    let mut p = POINT::default();
    unsafe { GetCursorPos(&mut p) }.ok()?;
    Some((p.x as f32 / ppp, p.y as f32 / ppp))
}

#[cfg(not(windows))]
fn cursor_logical(_ppp: f32) -> Option<(f32, f32)> {
    None
}

#[cfg(windows)]
fn open_path(p: &std::path::Path)->Result<(),String> {
    use std::os::windows::ffi::OsStrExt;
    use windows::core::PCWSTR;
    use windows::Win32::UI::Shell::{ShellExecuteExW,SHOpenWithDialog,SHELLEXECUTEINFOW,OPENASINFO,SEE_MASK_FLAG_NO_UI,OAIF_EXEC};
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
    let mut wide: Vec<u16> = p.as_os_str().encode_wide().collect();
    wide.push(0);
    let mut info=SHELLEXECUTEINFOW{cbSize:std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,fMask:SEE_MASK_FLAG_NO_UI,lpVerb:windows::core::w!("open"),lpFile:PCWSTR(wide.as_ptr()),nShow:SW_SHOWNORMAL.0,..Default::default()};
    open_path_result(p,unsafe{ShellExecuteExW(&mut info)},||unsafe{
        SHOpenWithDialog(None,&OPENASINFO{pcszFile:PCWSTR(wide.as_ptr()),oaifInFlags:OAIF_EXEC,..Default::default()})
    })
}

#[cfg(windows)]
fn open_path_result(p:&std::path::Path,result:windows::core::Result<()>,choose:impl FnOnce()->windows::core::Result<()>)->Result<(),String>{
    use windows::Win32::Foundation::{ERROR_CANCELLED,ERROR_NO_ASSOCIATION};
    let result=match result{
        Err(error) if error.code()==windows::core::HRESULT::from_win32(ERROR_NO_ASSOCIATION.0)&&p.is_file()=>choose(),
        other=>other,
    };
    match result{
        Err(error) if error.code()==windows::core::HRESULT::from_win32(ERROR_CANCELLED.0)=>Ok(()),
        other=>other.map_err(|_|"无法打开，请检查文件位置和默认应用".into()),
    }
}

#[cfg(not(windows))]
fn open_path(_p: &std::path::Path)->Result<(),String> {Ok(())}

#[cfg(all(test,windows))]
mod open_path_tests {
    use super::*;
    use windows::Win32::Foundation::{ERROR_ACCESS_DENIED,ERROR_CANCELLED,ERROR_FILE_NOT_FOUND,ERROR_NO_ASSOCIATION};
    fn error(code:windows::Win32::Foundation::WIN32_ERROR)->windows::core::Error{windows::core::Error::from_hresult(windows::core::HRESULT::from_win32(code.0))}

    #[test]
    fn unassociated_files_offer_open_with_and_cancellation_is_normal(){
        let path=std::env::temp_dir().join(format!("岛仔 无关联-{}.daozai-open-test",std::process::id()));
        std::fs::write(&path,b"fixture").unwrap();
        for cancel in [false,true]{
            let mut chosen=false;
            let result=open_path_result(&path,Err(error(ERROR_NO_ASSOCIATION)),||{chosen=true;if cancel{Err(error(ERROR_CANCELLED))}else{Ok(())}});
            std::fs::remove_file(&path).unwrap();
            assert!(chosen,"文件没有默认关联时应提供系统打开方式");
            assert_eq!(result,Ok(()),"用户取消选择不应显示打开失败");
            if !cancel{std::fs::write(&path,b"fixture").unwrap();}
        }
    }

    #[test]
    fn default_apps_urls_and_other_errors_never_open_the_file_picker(){
        let file=std::env::temp_dir().join(format!("daozai-open-errors-{}",std::process::id()));
        std::fs::write(&file,b"fixture").unwrap();
        for result in [Ok(()),Err(error(ERROR_ACCESS_DENIED)),Err(error(ERROR_FILE_NOT_FOUND))]{
            let success=result.is_ok();
            let actual=open_path_result(&file,result,||panic!("已有默认程序或其他打开错误不应弹出选择器"));
            assert_eq!(actual.is_ok(),success);
        }
        assert!(open_path_result(&file,Err(error(ERROR_NO_ASSOCIATION)),||Err(error(ERROR_ACCESS_DENIED))).is_err(),"选择器失败应保留错误提示");
        std::fs::remove_file(file).unwrap();
        for (path,result,success) in [
            ("https://example.com",Ok(()),true),
            ("https://example.com",Err(error(ERROR_NO_ASSOCIATION)),false),
            ("missing.daozai-open-test",Err(error(ERROR_NO_ASSOCIATION)),false),
            ("missing.daozai-open-test",Err(error(ERROR_FILE_NOT_FOUND)),false),
            ("blocked.daozai-open-test",Err(error(ERROR_ACCESS_DENIED)),false),
        ]{
            let actual=open_path_result(std::path::Path::new(path),result,||panic!("非文件关联问题不能弹出打开方式"));
            assert_eq!(actual.is_ok(),success);
        }
    }
}

fn main() -> std::process::ExitCode {
    // The local launcher remembers its separate profile for direct launches and autostart.
    if std::env::var_os("DAOZAI_DATA_DIR").is_none() {
        if let Some(profile)=std::env::current_exe().ok().and_then(|exe|exe.parent().map(|p|p.join("daozai-data-dir.txt"))) {
            if let Ok(value)=std::fs::read_to_string(profile) {
                let directory=std::path::PathBuf::from(value.trim());
                if directory.is_absolute()&&directory.is_dir(){std::env::set_var("DAOZAI_DATA_DIR",directory);}
            }
        }
    }
    if let Some(index)=std::env::args().position(|arg|arg=="--codex-hook") {
        let dir=std::env::args().nth(index+1).map(std::path::PathBuf::from).unwrap_or_else(daozai::codex_tasks::event_dir);
        let result=daozai::codex_tasks::write_hook(std::io::stdin().lock(),&dir);
        // Stop and PermissionRequest accept an empty JSON object; never influence agent decisions.
        println!("{{}}");
        return if result.is_ok(){std::process::ExitCode::SUCCESS}else{std::process::ExitCode::FAILURE};
    }
    if std::env::args().any(|arg|arg=="--install-codex-hooks") {
        let result=std::env::current_exe().map_err(|e|e.to_string()).and_then(|exe|daozai::codex_tasks::install_hooks(&daozai::codex_tasks::codex_home(),&exe,&daozai::codex_tasks::backup_dir()));
        match result {Ok(path)=>{println!("{}",serde_json::json!({"installed":path,"review_required":true}));return std::process::ExitCode::SUCCESS;},Err(error)=>{eprintln!("{error}");return std::process::ExitCode::FAILURE;}}
    }
    diagnostics::install();
    // 收集诊断：在单实例检查、创建窗口和初始化显卡之前处理，岛仔启动即崩溃、卡死时也能用。
    #[cfg(windows)]
    if daozai::diagnostics_tool::requested(std::env::args()) {
        return match daozai::diagnostics_tool::run(&daozai::model::config_dir()) {
            Ok(_) => std::process::ExitCode::SUCCESS,
            Err(error) => {diagnostics::report("诊断收集未完成。", &error); std::process::ExitCode::FAILURE}
        };
    }
    match run() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            diagnostics::report("浮岛启动或运行失败。", &error.to_string());
            std::process::ExitCode::FAILURE
        }
    }
}

fn run() -> eframe::Result<()> {
    // 自动重启或「立即重启」拉起的新进程：先等旧进程退出，再做单实例检查。
    diagnostics::wait_for_previous_instance();
    // 单实例（与原版同名互斥体）
    #[cfg(windows)]
    if !daozai::tray::single_instance() {
        if std::env::args().any(|arg|arg=="--settings"){let _=daozai::tray::activate_existing(false);}
        if std::env::args().any(|arg|arg=="--calendar"){let _=daozai::tray::activate_existing(true);}
        if std::env::args().any(|arg|arg=="--messages"){daozai::tray::open_messages();}
        if std::env::args().any(|arg|arg=="--clipboard"){daozai::tray::open_clipboard();}
        if std::env::args().any(|arg|arg=="--connect-notifications"){daozai::tray::connect_notifications();}
        if !std::env::args().any(|arg|matches!(arg.as_str(),"--settings"|"--calendar"|"--messages"|"--clipboard"|"--connect-notifications")) && !daozai::tray::activate_existing(false) {diagnostics::report("浮岛已在运行，但窗口暂时无法响应。", "请稍后重试。");}

        return Ok(());
    }
    diagnostics::start_session();
    #[cfg(windows)]
    std::thread::spawn(daozai::record_install_size);
    diagnostics::startup("初始化图形窗口");
    let viewport = egui::ViewportBuilder::default()
        .with_title(daozai::APP_NAME).with_icon(daozai::theme::app_icon())
        .with_inner_size([app::CANVAS_W, app::WIN_H])
        .with_decorations(false)
        .with_transparent(true)
        .with_active(false)
        .with_resizable(false)
        .with_taskbar(false)
        .with_always_on_top()
        .with_visible(false);
    let opts = eframe::NativeOptions { viewport, vsync: !cfg!(windows), ..Default::default() };
    // Keep eframe's persistence identity across the display name change.
    let result = graphics::run(opts, |opts| eframe::run_native("Leidao", opts, Box::new(|cc| Ok(Box::new(Shell::new(cc))))));
    diagnostics::session_event(if result.is_ok() { "事件循环正常退出" } else { "事件循环返回错误" });
    result
}
