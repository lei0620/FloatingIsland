//! Personal Outlook IMAP OAuth: system browser, PKCE, loopback callback and
//! Windows Credential Manager. Public client IDs are the only disk configuration.
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use ring::{
    digest,
    rand::{SecureRandom, SystemRandom},
};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    sync::{
        atomic::{AtomicBool, Ordering},
        Mutex, OnceLock,
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

const AUTHORIZE: &str = "https://login.microsoftonline.com/consumers/oauth2/v2.0/authorize";
const TOKEN: &str = "https://login.microsoftonline.com/consumers/oauth2/v2.0/token";
pub const SCOPES: &str = "offline_access https://outlook.office.com/IMAP.AccessAsUser.All";
const VAULT: &str = "Leidao.Mail.OutlookOAuth";

#[derive(Clone, PartialEq, Eq)]
pub struct Grant {
    client_id: String,
    access_token: String,
    refresh_token: String,
    expires_at: u64,
}
impl std::fmt::Debug for Grant {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("OutlookGrant([redacted])")
    }
}
impl Grant {
    pub(crate) fn access_token(&self) -> &str {
        &self.access_token
    }
}
struct Cached {
    token: String,
    expires_at: u64,
}
fn cache() -> &'static Mutex<HashMap<String, Cached>> {
    static CACHE: OnceLock<Mutex<HashMap<String, Cached>>> = OnceLock::new();
    CACHE.get_or_init(Default::default)
}
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

pub fn valid_email(email: &str) -> bool {
    email.len() <= 254
        && !email.chars().any(|c| c.is_whitespace() || c.is_control())
        && email.split_once('@').is_some_and(|(left, right)| {
            !left.is_empty() && right.contains('.') && !right.contains('@')
        })
}
pub fn valid_client_id(id: &str) -> bool {
    id.len() == 36
        && id.bytes().enumerate().all(|(i, c)| {
            if [8, 13, 18, 23].contains(&i) {
                c == b'-'
            } else {
                c.is_ascii_hexdigit()
            }
        })
        && id.bytes().any(|c| c.is_ascii_hexdigit() && c != b'0')
}
fn config_path() -> std::path::PathBuf {
    crate::model::config_dir().join("mail-outlook.json")
}
pub fn load_client_id() -> String {
    std::fs::read(config_path())
        .ok()
        .and_then(|v| serde_json::from_slice::<Value>(&v).ok())
        .and_then(|v| v["client_id"].as_str().map(str::to_owned))
        .filter(|v| valid_client_id(v))
        .unwrap_or_default()
}
pub fn save_client_id(id: &str) -> Result<(), String> {
    if !valid_client_id(id) {
        return Err("请填写应用程序（客户端）ID，格式为 36 位 GUID。".into());
    }
    let path = config_path();
    std::fs::create_dir_all(path.parent().unwrap())
        .map_err(|_| "无法创建邮箱配置目录".to_string())?;
    crate::model::atomic_write(
        &path,
        json!({"version":1,"client_id":id}).to_string().as_bytes(),
    )
    .map_err(|_| "无法保存客户端 ID".into())
}

fn random_url_token() -> Result<String, String> {
    let mut bytes = [0u8; 32];
    SystemRandom::new()
        .fill(&mut bytes)
        .map_err(|_| "无法生成登录校验数据".to_string())?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}
fn challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(digest::digest(&digest::SHA256, verifier.as_bytes()))
}
fn authorization_url(
    email: &str,
    client_id: &str,
    redirect: &str,
    state: &str,
    verifier: &str,
) -> String {
    let mut url = url::Url::parse(AUTHORIZE).unwrap();
    url.query_pairs_mut().extend_pairs([
        ("client_id", client_id),
        ("response_type", "code"),
        ("response_mode", "query"),
        ("redirect_uri", redirect),
        ("scope", SCOPES),
        ("state", state),
        ("code_challenge", &challenge(verifier)),
        ("code_challenge_method", "S256"),
        ("login_hint", email),
        ("prompt", "select_account"),
    ]);
    url.into()
}

enum Callback {
    Code(String),
    Denied,
    Ignore,
}
fn parse_callback(request: &str, state: &str) -> Callback {
    let mut words = request
        .lines()
        .next()
        .unwrap_or_default()
        .split_whitespace();
    if words.next() != Some("GET") {
        return Callback::Ignore;
    }
    let Some(target) = words.next() else {
        return Callback::Ignore;
    };
    if !target.starts_with("/?") || target.len() > 8192 {
        return Callback::Ignore;
    }
    let Ok(url) = url::Url::parse(&format!("http://localhost{target}")) else {
        return Callback::Ignore;
    };
    let pairs: Vec<_> = url.query_pairs().collect();
    let values = |key: &str| {
        pairs
            .iter()
            .filter(|(k, _)| k == key)
            .map(|(_, v)| v.as_ref())
            .collect::<Vec<_>>()
    };
    if values("state") != vec![state] {
        return Callback::Ignore;
    }
    if !values("error").is_empty() {
        return Callback::Denied;
    }
    let codes = values("code");
    if codes.len() != 1 || codes[0].is_empty() || codes[0].chars().any(char::is_control) {
        return Callback::Ignore;
    }
    Callback::Code(codes[0].to_string())
}
fn callback_response(stream: &mut TcpStream, accepted: bool) {
    let body = if accepted {
        "<!doctype html><meta charset=utf-8><title>浮岛微软登录</title><p>已收到登录结果，请返回浮岛。</p>"
    } else {
        "<!doctype html><meta charset=utf-8><title>浮岛微软登录</title><p>无效的登录回调，请返回微软登录页面继续。</p>"
    };
    let status = if accepted {
        "200 OK"
    } else {
        "400 Bad Request"
    };
    let response=format!("HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nCache-Control: no-store\r\nContent-Security-Policy: default-src 'none'\r\nConnection: close\r\n\r\n{body}",body.len());
    let _ = stream.write_all(response.as_bytes());
}
fn wait_callback(
    listener: TcpListener,
    state: &str,
    cancel: &AtomicBool,
    limit: Duration,
) -> Result<String, String> {
    listener
        .set_nonblocking(true)
        .map_err(|_| "无法监听本机登录回调".to_string())?;
    let start = Instant::now();
    while start.elapsed() < limit {
        if cancel.load(Ordering::Relaxed) {
            return Err("微软登录已取消".into());
        }
        match listener.accept() {
            Ok((mut stream, peer)) => {
                if !peer.ip().is_loopback() {
                    continue;
                }
                let _ = stream.set_read_timeout(Some(Duration::from_secs(1)));
                let _ = stream.set_write_timeout(Some(Duration::from_secs(1)));
                let mut bytes = Vec::new();
                let mut chunk = [0u8; 1024];
                while bytes.len() < 8192 {
                    match stream.read(&mut chunk) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => bytes.extend_from_slice(&chunk[..n]),
                    }
                    if bytes.windows(4).any(|s| s == b"\r\n\r\n") {
                        break;
                    }
                }
                let result = if bytes.len() <= 8192 {
                    parse_callback(&String::from_utf8_lossy(&bytes), state)
                } else {
                    Callback::Ignore
                };
                callback_response(&mut stream, !matches!(result, Callback::Ignore));
                match result {
                    Callback::Code(code) => return Ok(code),
                    Callback::Denied => return Err("未授予邮箱访问权限，登录已取消".into()),
                    Callback::Ignore => {}
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(50))
            }
            Err(_) => return Err("本机登录回调连接失败".into()),
        }
    }
    Err("微软登录等待超时，请重新点击登录。".into())
}
pub(crate) fn authorize(
    email: &str,
    id: &str,
    cancel: &AtomicBool,
    ready: impl FnOnce(String),
) -> Result<Grant, String> {
    if !valid_email(email) || !valid_client_id(id) {
        return Err("请先填写完整邮箱地址和有效的客户端 ID。".into());
    }
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .map_err(|_| "无法创建本机登录回调".to_string())?;
    let port = listener
        .local_addr()
        .map_err(|_| "无法获取本机登录端口".to_string())?
        .port();
    let redirect = format!("http://localhost:{port}");
    let state = random_url_token()?;
    let verifier = random_url_token()?;
    ready(authorization_url(email, id, &redirect, &state, &verifier));
    let code = wait_callback(listener, &state, cancel, Duration::from_secs(180))?;
    if cancel.load(Ordering::Relaxed) {
        return Err("微软登录已取消".into());
    }
    request_tokens(
        TOKEN,
        &[
            ("client_id", id),
            ("grant_type", "authorization_code"),
            ("code", &code),
            ("redirect_uri", &redirect),
            ("code_verifier", &verifier),
            ("scope", SCOPES),
        ],
        id,
        None,
    )
}

fn oauth_error(value: &Value, status: u16) -> String {
    let codes = value["error_codes"].as_array();
    if codes.is_some_and(|v| v.iter().any(|c| c.as_u64() == Some(50011))) {
        return "微软应用的重定向 URI 不匹配，请配置桌面应用 http://localhost。".into();
    }
    match value["error"].as_str().unwrap_or_default() {
        "invalid_client" | "unauthorized_client" => {
            "微软应用 ID 或账户类型配置不正确，请确认应用支持个人 Microsoft 账户。".into()
        }
        "invalid_grant" | "interaction_required" | "consent_required" => {
            "微软登录已过期或被撤销，请重新登录。".into()
        }
        "invalid_scope" => "应用未配置 IMAP.AccessAsUser.All 委托权限，请检查微软应用注册。".into(),
        "access_denied" => "未授予邮箱访问权限，请重新登录并确认授权。".into(),
        _ => format!("微软登录服务返回错误（HTTP {status}），请稍后重试。"),
    }
}
fn read_json(response: ureq::Response) -> Result<Value, String> {
    let mut bytes = Vec::new();
    response
        .into_reader()
        .take(65537)
        .read_to_end(&mut bytes)
        .map_err(|_| "无法读取微软登录响应".to_string())?;
    if bytes.len() > 65536 {
        return Err("微软登录响应过大".into());
    }
    serde_json::from_slice(&bytes).map_err(|_| "微软登录响应格式异常".into())
}
fn parse_grant(value: &Value, id: &str, previous: Option<&str>) -> Result<Grant, String> {
    let access = value["access_token"]
        .as_str()
        .filter(|s| !s.is_empty() && !s.chars().any(char::is_control))
        .ok_or("微软未返回有效的访问令牌")?;
    if !value["token_type"]
        .as_str()
        .is_some_and(|s| s.eq_ignore_ascii_case("Bearer"))
    {
        return Err("微软返回了不支持的令牌类型".into());
    }
    let refresh = value["refresh_token"]
        .as_str()
        .or(previous)
        .filter(|s| !s.is_empty())
        .ok_or("微软未返回离线登录权限，请重新授权。")?;
    let seconds = value["expires_in"]
        .as_u64()
        .filter(|v| *v > 0 && *v <= 86400)
        .ok_or("微软令牌有效期异常")?;
    Ok(Grant {
        client_id: id.into(),
        access_token: access.into(),
        refresh_token: refresh.into(),
        expires_at: now().saturating_add(seconds),
    })
}
fn request_tokens(
    endpoint: &str,
    form: &[(&str, &str)],
    id: &str,
    previous: Option<&str>,
) -> Result<Grant, String> {
    let agent = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(20))
        .redirects(0)
        .build();
    match agent.post(endpoint).send_form(form) {
        Ok(response) if response.status() == 200 => {
            parse_grant(&read_json(response)?, id, previous)
        }
        Ok(response) => Err(format!(
            "微软登录服务返回异常状态（HTTP {}）",
            response.status()
        )),
        Err(ureq::Error::Status(status, response)) => Err(oauth_error(
            &read_json(response).unwrap_or(Value::Null),
            status,
        )),
        Err(_) => Err("无法连接微软登录服务，请检查网络或代理。".into()),
    }
}

#[cfg(windows)]
fn vault_load(email: &str) -> Result<String, String> {
    keyring::Entry::new(VAULT, &email.to_ascii_lowercase())
        .map_err(|_| "凭据管理器不可用")?
        .get_password()
        .map_err(|_| "请先使用微软账户登录".into())
}
#[cfg(windows)]
fn vault_save(email: &str, value: &str) -> Result<(), String> {
    keyring::Entry::new(VAULT, &email.to_ascii_lowercase())
        .map_err(|_| "凭据管理器不可用")?
        .set_password(value)
        .map_err(|_| "保存微软登录凭据失败".into())
}
#[cfg(not(windows))]
fn vault_load(_: &str) -> Result<String, String> {
    Err("仅 Windows 支持凭据管理器".into())
}
#[cfg(not(windows))]
fn vault_save(_: &str, _: &str) -> Result<(), String> {
    Err("仅 Windows 支持凭据管理器".into())
}

fn saved_record(grant: &Grant) -> String {
    json!({"version":1,"client_id":grant.client_id,"refresh_token":grant.refresh_token}).to_string()
}
pub(crate) fn save_login(email: &str, grant: &Grant) -> Result<(), String> {
    let mut cache = cache().lock().unwrap_or_else(|e| e.into_inner());
    vault_save(email, &saved_record(grant))?;
    cache.insert(
        email.to_ascii_lowercase(),
        Cached {
            token: grant.access_token.clone(),
            expires_at: grant.expires_at,
        },
    );
    Ok(())
}
pub(crate) fn access_token(email: &str) -> Result<String, String> {
    let key = email.to_ascii_lowercase();
    let cached = || {
        cache()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&key)
            .filter(|c| c.expires_at > now() + 60)
            .map(|c| c.token.clone())
    };
    if let Some(token) = cached() {
        return Ok(token);
    }
    // Serialize rotation on workers, while keeping the UI's cache/vault lock
    // free during network requests. Deletion and re-login cannot be undone by
    // an older refresh that finishes later.
    static REFRESH: Mutex<()> = Mutex::new(());
    let _refresh = REFRESH.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(token) = cached() {
        return Ok(token);
    }
    let saved_raw = vault_load(email)?;
    let saved: Value =
        serde_json::from_str(&saved_raw).map_err(|_| "微软凭据格式异常，请重新登录")?;
    let id = saved["client_id"]
        .as_str()
        .filter(|id| valid_client_id(id))
        .ok_or("微软应用 ID 缺失，请重新登录")?;
    let refresh = saved["refresh_token"]
        .as_str()
        .filter(|s| !s.is_empty())
        .ok_or("微软登录已失效，请重新登录")?;
    let grant = request_tokens(
        TOKEN,
        &[
            ("client_id", id),
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh),
            ("scope", SCOPES),
        ],
        id,
        Some(refresh),
    )?;
    let mut cache = cache().lock().unwrap_or_else(|e| e.into_inner());
    if vault_load(email)? != saved_raw {
        return Err("微软登录状态已更新，请重试。".into());
    }
    vault_save(email, &saved_record(&grant))?;
    cache.insert(
        key,
        Cached {
            token: grant.access_token.clone(),
            expires_at: grant.expires_at,
        },
    );
    Ok(grant.access_token)
}
pub(crate) fn invalidate(email: &str) {
    cache()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(&email.to_ascii_lowercase());
}
pub(crate) fn forget(email: &str) {
    let mut cache = cache().lock().unwrap_or_else(|e| e.into_inner());
    cache.remove(&email.to_ascii_lowercase());
    #[cfg(windows)]
    if let Ok(entry) = keyring::Entry::new(VAULT, &email.to_ascii_lowercase()) {
        let _ = entry.delete_credential();
    }
}
pub(crate) fn xoauth2(email: &str, token: &str) -> Result<String, String> {
    if !valid_email(email) || token.is_empty() || token.chars().any(char::is_control) {
        return Err("微软登录凭据无效，请重新登录".into());
    }
    Ok(format!("user={email}\x01auth=Bearer {token}\x01\x01"))
}

#[cfg(test)]
mod tests {
    use super::*;
    const ID: &str = "a1234567-1234-4321-aaaa-123456789abc";
    #[test]
    fn pkce_matches_rfc7636_and_scope_is_imap_only() {
        let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        assert_eq!(
            challenge(verifier),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
        let u = url::Url::parse(&authorization_url(
            "a@outlook.com",
            ID,
            "http://localhost:54321",
            "state",
            verifier,
        ))
        .unwrap();
        let q: HashMap<_, _> = u.query_pairs().collect();
        assert_eq!(u.host_str(), Some("login.microsoftonline.com"));
        assert!(u.path().starts_with("/consumers/"));
        assert_eq!(q["code_challenge_method"], "S256");
        assert!(!q["scope"].contains("SMTP") && !q.contains_key("client_secret"));
    }
    #[test]
    fn callback_rejects_missing_wrong_or_duplicate_state() {
        for s in [
            "GET /?code=secret HTTP/1.1",
            "GET /?code=secret&state=wrong HTTP/1.1",
            "POST /?code=secret&state=ok HTTP/1.1",
            "GET /?code=secret&state=ok&state=ok HTTP/1.1",
            "GET /else?code=secret&state=ok HTTP/1.1",
        ] {
            assert!(matches!(parse_callback(s, "ok"), Callback::Ignore));
        }
        assert!(
            matches!(parse_callback("GET /?code=good&state=ok HTTP/1.1","ok"),Callback::Code(c)if c=="good")
        );
        assert!(matches!(
            parse_callback("GET /?error=access_denied&state=ok HTTP/1.1", "ok"),
            Callback::Denied
        ));
    }
    #[test]
    fn callback_listener_handles_cancellation_and_real_http() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let worker = std::thread::spawn(move || {
            wait_callback(
                listener,
                "nonce",
                &AtomicBool::new(false),
                Duration::from_secs(3),
            )
        });
        let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
        stream
            .write_all(b"GET /?state=nonce&code=abc HTTP/1.1\r\nHost: localhost\r\n\r\n")
            .unwrap();
        let mut body = String::new();
        stream.read_to_string(&mut body).unwrap();
        assert!(body.contains("Cache-Control: no-store") && !body.contains("abc"));
        assert_eq!(worker.join().unwrap().unwrap(), "abc");
        assert!(wait_callback(
            TcpListener::bind("127.0.0.1:0").unwrap(),
            "x",
            &AtomicBool::new(true),
            Duration::from_secs(3)
        )
        .unwrap_err()
        .contains("取消"));
    }
    #[test]
    fn refresh_rotation_and_secrets_are_not_in_debug_or_public_config() {
        let value = json!({"access_token":"access-secret","refresh_token":"new-refresh","token_type":"Bearer","expires_in":3600});
        let grant = parse_grant(&value, ID, Some("old-refresh")).unwrap();
        assert_eq!(grant.refresh_token, "new-refresh");
        assert!(!format!("{grant:?}").contains("secret"));
        let record = saved_record(&grant);
        assert!(!record.contains("access-secret"));
        let mut no_rotation = value;
        no_rotation.as_object_mut().unwrap().remove("refresh_token");
        assert_eq!(
            parse_grant(&no_rotation, ID, Some("retained"))
                .unwrap()
                .refresh_token,
            "retained"
        );
        assert!(parse_grant(&no_rotation, ID, None).is_err());
        assert!(!oauth_error(
            &json!({"error":"invalid_grant","error_description":"access-secret"}),
            400
        )
        .contains("secret"));
    }
    #[test]
    fn xoauth2_format_and_input_validation() {
        assert_eq!(
            xoauth2("demo@outlook.com", "token").unwrap(),
            "user=demo@outlook.com\x01auth=Bearer token\x01\x01"
        );
        assert!(xoauth2("demo\n@outlook.com", "token").is_err());
        assert!(xoauth2("a@outlook.com", "token\x01").is_err());
        assert!(valid_client_id(ID));
        assert!(!valid_client_id("00000000-0000-0000-0000-000000000000"));
    }
    #[test]
    fn token_transport_uses_form_and_keeps_error_details_private() {
        for success in [true, false] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let address = listener.local_addr().unwrap();
            let server = std::thread::spawn(move || {
                use std::io::BufRead;
                let (mut socket, _) = listener.accept().unwrap();
                socket
                    .set_read_timeout(Some(Duration::from_secs(3)))
                    .unwrap();
                let mut reader = std::io::BufReader::new(socket.try_clone().unwrap());
                let mut first = String::new();
                reader.read_line(&mut first).unwrap();
                assert!(first.starts_with("POST /token "));
                let mut length = 0;
                loop {
                    let mut line = String::new();
                    reader.read_line(&mut line).unwrap();
                    if line == "\r\n" {
                        break;
                    }
                    if let Some((k, v)) = line.split_once(':') {
                        if k.eq_ignore_ascii_case("content-length") {
                            length = v.trim().parse::<usize>().unwrap();
                        }
                    }
                }
                let mut body = vec![0; length];
                reader.read_exact(&mut body).unwrap();
                let form = String::from_utf8(body).unwrap();
                assert!(
                    form.contains("refresh_token=old-refresh") && !form.contains("client_secret")
                );
                let (status, json) = if success {
                    (
                        "200 OK",
                        r#"{"access_token":"new-access","refresh_token":"rotated","token_type":"Bearer","expires_in":3600}"#,
                    )
                } else {
                    (
                        "400 Bad Request",
                        r#"{"error":"invalid_grant","error_description":"do-not-show-this-secret"}"#,
                    )
                };
                write!(socket,"HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{json}",json.len()).unwrap();
            });
            let result = request_tokens(
                &format!("http://{address}/token"),
                &[
                    ("client_id", ID),
                    ("grant_type", "refresh_token"),
                    ("refresh_token", "old-refresh"),
                ],
                ID,
                Some("old-refresh"),
            );
            if success {
                let grant = result.unwrap();
                assert_eq!(grant.access_token, "new-access");
                assert_eq!(grant.refresh_token, "rotated");
            } else {
                let error = result.unwrap_err();
                assert!(error.contains("重新登录") && !error.contains("secret"));
            }
            server.join().unwrap();
        }
    }
}
