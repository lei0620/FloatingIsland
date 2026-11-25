//! URL-specific Windows proxy resolution for quota requests.
use std::{collections::HashMap, sync::Mutex, time::Duration};

pub(crate) trait HttpClient: Send + Sync {
    fn request(&self, method: &str, url: &str) -> ureq::Request;
    fn get(&self, url: &str) -> ureq::Request { self.request("GET", url) }
    fn post(&self, url: &str) -> ureq::Request { self.request("POST", url) }
}

impl HttpClient for ureq::Agent {
    fn request(&self, method: &str, url: &str) -> ureq::Request {
        ureq::Agent::request(self, method, url)
    }
}

pub(super) struct HttpAgent {
    default: ureq::Agent,
    automatic: bool,
    resolve: fn(&str) -> Option<String>,
    proxies: Mutex<HashMap<String, ureq::Agent>>,
    connect_timeout: Duration,
    streaming: bool,
}

fn build_agent(proxy: Option<&str>, connect_timeout: Duration, streaming: bool) -> ureq::Agent {
    let mut builder = ureq::AgentBuilder::new().try_proxy_from_env(false)
        .timeout_connect(connect_timeout).redirects(0);
    let timeout = Duration::from_secs(15);
    builder = if streaming { builder.timeout_read(timeout).timeout_write(timeout) } else { builder.timeout(timeout) };
    if let Some(proxy) = proxy.and_then(|value| ureq::Proxy::new(value).ok()) {
        builder = builder.proxy(proxy);
    }
    builder.build()
}

impl HttpAgent {
    pub(super) fn new() -> Self {
        Self::with_connect_timeout(Duration::from_secs(30))
    }

    pub(super) fn with_connect_timeout(connect_timeout: Duration) -> Self {
        Self::from_environment(connect_timeout, false)
    }

    pub(super) fn streaming() -> Self {
        Self::from_environment(Duration::from_secs(15), true)
    }

    fn from_environment(connect_timeout: Duration, streaming: bool) -> Self {
        let proxy = ["HTTPS_PROXY", "https_proxy", "ALL_PROXY", "all_proxy", "HTTP_PROXY", "http_proxy"]
            .iter().find_map(|key| std::env::var(key).ok().filter(|value| !value.is_empty()));
        Self::with_proxy_and_timeout(proxy, connect_timeout, streaming)
    }

    fn with_environment_proxy(proxy: Option<String>) -> Self {
        Self::with_proxy_and_timeout(proxy, Duration::from_secs(30), false)
    }

    fn with_proxy_and_timeout(proxy: Option<String>, connect_timeout: Duration, streaming: bool) -> Self {
        Self { default: build_agent(proxy.as_deref(), connect_timeout, streaming), automatic: proxy.is_none(),
            resolve: system_proxy, proxies: Mutex::new(HashMap::new()), connect_timeout, streaming }
    }
}

impl HttpClient for HttpAgent {
    fn request(&self, method: &str, url: &str) -> ureq::Request {
        if self.automatic {
            if let Some(proxy) = (self.resolve)(url) {
                let mut agents = self.proxies.lock().unwrap_or_else(|error| error.into_inner());
                return agents.entry(proxy.clone()).or_insert_with(|| build_agent(Some(&proxy), self.connect_timeout, self.streaming)).request(method, url);
            }
        }
        self.default.request(method, url)
    }
}

#[cfg(not(windows))]
pub(crate) fn system_proxy(_: &str) -> Option<String> { None }

#[cfg(windows)]
pub(crate) fn system_proxy(url: &str) -> Option<String> {
    windows_proxy(url).unwrap_or_else(|_| manual_proxy())
}

#[cfg(windows)]
fn windows_proxy(url: &str) -> windows::core::Result<Option<String>> {
    use windows::{core::{w, HSTRING, PCWSTR, PWSTR}, Win32::{Foundation::{GlobalFree, HGLOBAL}, Networking::WinHttp::*}};
    fn take_string(value: PWSTR) -> Option<String> {
        if value.is_null() { return None; }
        unsafe {
            let text = value.to_string().ok();
            let _ = GlobalFree(HGLOBAL(value.0.cast()));
            text
        }
    }
    struct Session(*mut std::ffi::c_void);
    impl Drop for Session {
        fn drop(&mut self) { unsafe { let _ = WinHttpCloseHandle(self.0); } }
    }
    let mut config = WINHTTP_CURRENT_USER_IE_PROXY_CONFIG::default();
    unsafe { WinHttpGetIEProxyConfigForCurrentUser(&mut config)?; }
    let pac = take_string(config.lpszAutoConfigUrl).filter(|value| !value.is_empty());
    let _ = take_string(config.lpszProxy);
    let _ = take_string(config.lpszProxyBypass);
    if pac.is_none() && !config.fAutoDetect.as_bool() { return Ok(manual_proxy()); }
    let session = Session(unsafe { WinHttpOpen(w!("Leidao quota"), WINHTTP_ACCESS_TYPE_NO_PROXY, PCWSTR::null(), PCWSTR::null(), 0) });
    if session.0.is_null() { return Err(windows::core::Error::from_win32()); }
    unsafe { WinHttpSetTimeouts(session.0, 5000, 5000, 5000, 5000)?; }
    let pac_url = HSTRING::from(pac.as_deref().unwrap_or_default());
    let request_url = HSTRING::from(url);
    let mut options = WINHTTP_AUTOPROXY_OPTIONS::default();
    if pac.is_some() {
        options.dwFlags = WINHTTP_AUTOPROXY_CONFIG_URL;
        options.lpszAutoConfigUrl = PCWSTR(pac_url.as_ptr());
    } else {
        options.dwFlags = WINHTTP_AUTOPROXY_AUTO_DETECT;
        options.dwAutoDetectFlags = WINHTTP_AUTO_DETECT_TYPE_DHCP | WINHTTP_AUTO_DETECT_TYPE_DNS_A;
    }
    let mut info = WINHTTP_PROXY_INFO::default();
    let resolved = unsafe { WinHttpGetProxyForUrl(session.0, PCWSTR(request_url.as_ptr()), &mut options, &mut info) };
    let proxy = take_string(info.lpszProxy);
    let _ = take_string(info.lpszProxyBypass);
    resolved?;
    if info.dwAccessType == WINHTTP_ACCESS_TYPE_NO_PROXY { return Ok(None); }
    Ok(proxy.and_then(|value| value.split(';').next().map(str::trim).filter(|value| !value.is_empty())
        .map(|value| if value.contains("://") { value.into() } else { format!("http://{value}") })))
}

#[cfg(windows)]
pub(super) fn manual_proxy() -> Option<String> {
    use windows::{core::w, Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_DWORD, RRF_RT_REG_SZ}};
    let key = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Internet Settings");
    let mut enabled = 0u32;
    let mut size = 4u32;
    unsafe {
        if RegGetValueW(HKEY_CURRENT_USER, key, w!("ProxyEnable"), RRF_RT_REG_DWORD, None,
            Some((&mut enabled as *mut u32).cast()), Some(&mut size)).is_err() || enabled == 0 { return None; }
    }
    let mut buffer = [0u16; 1024];
    size = (buffer.len() * 2) as u32;
    unsafe {
        if RegGetValueW(HKEY_CURRENT_USER, key, w!("ProxyServer"), RRF_RT_REG_SZ, None,
            Some(buffer.as_mut_ptr().cast()), Some(&mut size)).is_err() { return None; }
    }
    let text = String::from_utf16_lossy(&buffer[..buffer.iter().position(|value| *value == 0).unwrap_or(buffer.len())]);
    let server = if text.contains('=') {
        text.split(';').find_map(|part| part.trim().strip_prefix("https="))
            .or_else(|| text.split(';').find_map(|part| part.trim().strip_prefix("http=")))?
    } else { text.trim() };
    if server.is_empty() { None } else if server.contains("://") { Some(server.into()) }
    else { Some(format!("http://{server}")) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{io::{Read, Write}, net::TcpListener};

    fn server() -> (u16, std::thread::JoinHandle<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let worker = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
            let mut request = Vec::new();
            while !request.ends_with(b"\r\n\r\n") {
                let mut byte = [0];
                stream.read_exact(&mut byte).unwrap();
                request.push(byte[0]);
            }
            stream.write_all(b"HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n").unwrap();
            String::from_utf8(request).unwrap()
        });
        (port, worker)
    }

    fn synthetic_pac(url: &str) -> Option<String> {
        let url = url::Url::parse(url).unwrap();
        (url.host_str() == Some("proxy-test.invalid"))
            .then(|| format!("http://127.0.0.1:{}", url.port().unwrap()))
    }

    #[test]
    fn pac_routes_each_url_and_respects_direct_with_the_same_client() {
        let mut client = HttpAgent::with_environment_proxy(None);
        client.resolve = synthetic_pac;
        let (port, worker) = server();
        let url = format!("http://proxy-test.invalid:{port}/quota");
        assert_eq!(client.get(&url).call().unwrap().status(), 204);
        assert!(worker.join().unwrap().starts_with(&format!("GET {url} HTTP/1.1")));
        let (port, worker) = server();
        assert_eq!(client.get(&format!("http://127.0.0.1:{port}/direct")).call().unwrap().status(), 204);
        assert!(worker.join().unwrap().starts_with("GET /direct HTTP/1.1"));
    }

    #[test]
    fn explicit_proxy_precedes_system_pac() {
        let (port, worker) = server();
        let mut client = HttpAgent::with_environment_proxy(Some(format!("http://127.0.0.1:{port}")));
        client.resolve = |_| panic!("Explicit proxy must bypass automatic discovery");
        assert_eq!(client.get("http://explicit-test.invalid/quota").call().unwrap().status(), 204);
        assert!(worker.join().unwrap().starts_with("GET http://explicit-test.invalid/quota HTTP/1.1"));
    }

    #[cfg(windows)]
    #[test]
    #[ignore = "Requires the current computer's Windows PAC; no account credentials are used"]
    fn windows_pac_live_connectivity() {
        let url = "https://chatgpt.com/backend-api/wham/usage";
        assert!(windows_proxy(url).unwrap().is_some());
        assert!(windows_proxy("http://192.168.101.1/").unwrap().is_none());
        let client = HttpAgent::with_environment_proxy(None);
        let status = match client.get(url).call() {
            Ok(response) | Err(ureq::Error::Status(_, response)) => response.status(),
            Err(error) => panic!("PAC request failed: {error}"),
        };
        assert!(matches!(status, 200 | 401 | 403));
    }
}
