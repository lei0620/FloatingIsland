//! WeChat / WeCom signals. Taskbar attention is a hint, never a chat message count.
use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Source {
    WeChat,
    WeCom,
}
impl Source {
    pub fn label(self) -> &'static str {
        match self {
            Self::WeChat => "微信",
            Self::WeCom => "企业微信",
        }
    }
    pub fn executable(name: &str) -> Option<Self> {
        match name.to_ascii_lowercase().as_str() {
            "weixin.exe" | "wechat.exe" => Some(Self::WeChat),
            "wxwork.exe" => Some(Self::WeCom),
            _ => None,
        }
    }
    pub fn notification(app: &str, id: &str) -> Option<Self> {
        let value = format!("{app} {id}").to_lowercase();
        if value.contains("企业微信") || value.contains("wxwork") || value.contains("wecom") {
            Some(Self::WeCom)
        } else if value.contains("微信") || value.contains("wechat") || value.contains("weixin") {
            Some(Self::WeChat)
        } else {
            None
        }
    }
}

#[derive(Clone, Debug)]
pub enum Signal {
    Attention(Source),
    Activated(Source),
    Status(String),
}

#[derive(Default)]
pub struct Deduper {
    recent: HashMap<(Source, String), Instant>,
    attention: HashMap<Source, Instant>,
    content: HashMap<Source, Instant>,
}
impl Deduper {
    pub fn content(&mut self, source: Source, body: &str, now: Instant) -> bool {
        self.recent
            .retain(|_, at| now.saturating_duration_since(*at) < Duration::from_secs(5));
        let key = (source, body.to_owned());
        if self.recent.contains_key(&key) {
            return false;
        }
        self.recent.insert(key, now);
        self.content.insert(source, now);
        true
    }
    pub fn attention(&mut self, source: Source, now: Instant) -> bool {
        if self
            .content
            .get(&source)
            .is_some_and(|at| now.saturating_duration_since(*at) < Duration::from_secs(5))
        {
            return false;
        }
        if self.attention.contains_key(&source) {
            return false;
        }
        self.attention.insert(source, now);
        true
    }
    pub fn activated(&mut self, source: Source) {
        self.attention.remove(&source);
    }
}

#[cfg(windows)]
mod native {
    use super::*;
    use std::{
        cell::RefCell,
        path::PathBuf,
        sync::mpsc::{self, Receiver, Sender},
    };
    use windows::{
        core::{w, PWSTR},
        Win32::{
            Foundation::{CloseHandle, BOOL, HWND, LPARAM, LRESULT, WPARAM},
            System::{
                LibraryLoader::GetModuleHandleW,
                Threading::{
                    OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32,
                    PROCESS_QUERY_LIMITED_INFORMATION,
                },
            },
            UI::{Shell::ShellExecuteW, WindowsAndMessaging::*},
        },
    };
    thread_local! {static LISTENER:RefCell<Option<(u32,Sender<Signal>)>>=const{RefCell::new(None)};}
    fn source(hwnd: HWND) -> Option<Source> {
        unsafe {
            let mut pid = 0;
            GetWindowThreadProcessId(hwnd, Some(&mut pid));
            let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
            let mut path = [0u16; 2048];
            let mut length = path.len() as u32;
            let ok = QueryFullProcessImageNameW(
                process,
                PROCESS_NAME_WIN32,
                PWSTR(path.as_mut_ptr()),
                &mut length,
            )
            .is_ok();
            let _ = CloseHandle(process);
            if !ok {
                return None;
            }
            let path = PathBuf::from(String::from_utf16_lossy(&path[..length as usize]));
            Source::executable(path.file_name()?.to_str()?)
        }
    }
    unsafe extern "system" fn proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
        LISTENER.with(|ctx| {
            if let Some((message, tx)) = &*ctx.borrow() {
                if msg == *message {
                    let kind = wp.0 as u32;
                    let event_hwnd = HWND(lp.0 as *mut _);
                    if let Some(source) = source(event_hwnd) {
                        let signal = if kind == HSHELL_REDRAW | 0x8000 {
                            Some(Signal::Attention(source))
                        } else if kind == HSHELL_WINDOWACTIVATED
                            || kind == HSHELL_WINDOWACTIVATED | 0x8000
                        {
                            Some(Signal::Activated(source))
                        } else {
                            None
                        };
                        if let Some(signal) = signal {
                            let _ = tx.send(signal);
                        }
                    }
                }
            }
        });
        DefWindowProcW(hwnd, msg, wp, lp)
    }
    pub fn start() -> Receiver<Signal> {
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || unsafe {
            let result = (|| -> windows::core::Result<()> {
                let instance: windows::Win32::Foundation::HINSTANCE =
                    GetModuleHandleW(None)?.into();
                let class = w!("DaozaiMessengerSignals");
                let wc = WNDCLASSW {
                    lpfnWndProc: Some(proc),
                    hInstance: instance,
                    lpszClassName: class,
                    ..Default::default()
                };
                RegisterClassW(&wc);
                let hwnd = CreateWindowExW(
                    WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
                    class,
                    w!(""),
                    WS_POPUP,
                    0,
                    0,
                    0,
                    0,
                    None,
                    None,
                    Some(&instance),
                    None,
                )?;
                let message = RegisterWindowMessageW(w!("SHELLHOOK"));
                if !RegisterShellHookWindow(hwnd).as_bool() {
                    let _ = DestroyWindow(hwnd);
                    return Err(windows::core::Error::from_win32());
                }
                LISTENER.with(|ctx| *ctx.borrow_mut() = Some((message, tx.clone())));
                let _ = tx.send(Signal::Status("正在监听微信与企业微信任务栏提醒".into()));
                loop {
                    let mut msg = MSG::default();
                    while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                        let _ = TranslateMessage(&msg);
                        DispatchMessageW(&msg);
                    }
                    if tx.send(Signal::Status(String::new())).is_err() {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
                let _ = DeregisterShellHookWindow(hwnd);
                let _ = DestroyWindow(hwnd);
                LISTENER.with(|ctx| *ctx.borrow_mut() = None);
                Ok(())
            })();
            if let Err(e) = result {
                let _ = tx.send(Signal::Status(format!("任务栏监听不可用：{}", e.code())));
            }
        });
        rx
    }
    struct Find {
        source: Source,
        windows: Vec<(HWND, i64)>,
    }
    unsafe extern "system" fn collect(hwnd: HWND, data: LPARAM) -> BOOL {
        let find = &mut *(data.0 as *mut Find);
        if source(hwnd) == Some(find.source)
            && GetWindow(hwnd, GW_OWNER).unwrap_or_default().is_invalid()
        {
            let mut rect = windows::Win32::Foundation::RECT::default();
            if GetWindowRect(hwnd, &mut rect).is_ok() {
                let area = (rect.right - rect.left) as i64 * (rect.bottom - rect.top) as i64;
                if area > 80_000 {
                    find.windows.push((hwnd, area));
                }
            }
        }
        BOOL(1)
    }
    pub fn activate(target: Source) -> Result<(), String> {
        unsafe {
            let mut find = Find {
                source: target,
                windows: vec![],
            };
            let _ = EnumWindows(Some(collect), LPARAM(&mut find as *mut _ as isize));
            find.windows
                .sort_by_key(|(_, area)| std::cmp::Reverse(*area));
            let hwnd = find
                .windows
                .first()
                .map(|(h, _)| *h)
                .ok_or_else(|| format!("请先启动并登录{}", target.label()))?;
            if target == Source::WeChat {
                // Let WeChat restore its own render state; direct HWND restoration can produce a blank window.
                let result =
                    ShellExecuteW(None, w!("open"), w!("weixin://"), None, None, SW_SHOWNORMAL);
                if result.0 as isize <= 32 {
                    return Err("无法通过微信注册入口打开，请从托盘打开微信".into());
                }
            } else {
                let _ = ShowWindowAsync(hwnd, SW_RESTORE);
                let _ = SetForegroundWindow(hwnd);
            }
            Ok(())
        }
    }
}
#[cfg(windows)]
pub use native::{activate, start};

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn identifies_enterprise_before_personal() {
        assert_eq!(
            Source::notification("企业微信", "WeChatWork"),
            Some(Source::WeCom)
        );
        assert_eq!(Source::executable("WXWork.exe"), Some(Source::WeCom));
        assert_eq!(Source::executable("not-wechat.exe"), None);
    }
    #[test]
    fn repeated_attention_is_not_a_message_count() {
        let mut d = Deduper::default();
        let t = Instant::now();
        assert!(d.attention(Source::WeChat, t));
        assert!(!d.attention(Source::WeChat, t + Duration::from_secs(30)));
        d.activated(Source::WeChat);
        assert!(d.attention(Source::WeChat, t + Duration::from_secs(31)));
    }
    #[test]
    fn actual_content_supersedes_taskbar_hint_and_duplicates_expire() {
        let mut d = Deduper::default();
        let t = Instant::now();
        assert!(d.content(Source::WeCom, "同事 · 开会", t));
        assert!(!d.content(Source::WeCom, "同事 · 开会", t));
        assert!(!d.attention(Source::WeCom, t));
        assert!(d.content(Source::WeCom, "同事 · 开会", t + Duration::from_secs(6)));
    }
}
