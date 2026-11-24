use crate::{
    config_sync::{Bundle, MAX_BYTES},
    model,
};
use base64::{engine::general_purpose::STANDARD, Engine};
use serde::{Deserialize, Serialize};
use std::{fs, io::Read, time::Duration};
use url::Url;

const FILE_NAME: &str = "Leidao-config.json";

#[derive(Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Connection {
    pub server: String,
    pub directory: String,
    pub username: String,
}

impl Connection {
    pub(crate) fn load() -> Option<Self> {
        fs::read(model::config_dir().join("config-webdav.json"))
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Self>(&bytes).ok())
            .and_then(|connection| connection.normalized().ok())
    }

    pub(crate) fn normalized(&self) -> Result<Self, String> {
        let value = self.server.trim();
        if value.contains('\\') || value.chars().any(char::is_control) {
            return Err("WebDAV 服务器地址无效".into());
        }
        let url = Url::parse(value).map_err(|_| "请输入完整的 HTTPS 服务器地址")?;
        if url.scheme() != "https"
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err("服务器地址须使用 HTTPS，账号和应用密码请单独填写".into());
        }
        let directory = self.directory.trim().replace('\\', "/");
        let directory = directory.trim_matches('/');
        if directory.len() > 1024
            || directory.split('/').count() > 12
            || (!directory.is_empty()
                && directory.split('/').any(|name| {
                    name.trim().is_empty()
                        || name == "."
                        || name == ".."
                        || name.chars().any(char::is_control)
                }))
        {
            return Err("存储路径须填写目录名称，各级之间用 / 或反斜杠分隔".into());
        }
        let username = self.username.trim();
        if username.is_empty()
            || username.len() > 256
            || username.contains(':')
            || username.chars().any(char::is_control)
        {
            return Err("请输入有效的 WebDAV 账号".into());
        }
        Ok(Self {
            server: url.as_str().trim_end_matches('/').into(),
            directory: directory.into(),
            username: username.into(),
        })
    }

    #[cfg(windows)]
    fn credential(&self) -> Result<keyring::Entry, String> {
        let identity = format!("{}\n{}", self.server, self.username);
        let hash = ring::digest::digest(&ring::digest::SHA256, identity.as_bytes());
        let key: String = hash
            .as_ref()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        keyring::Entry::new("Daozai.ConfigWebDAV", &key)
            .map_err(|_| "无法打开 Windows 凭据管理器".into())
    }

    pub(crate) fn password(&self) -> Result<String, String> {
        #[cfg(windows)]
        return self
            .credential()?
            .get_password()
            .map_err(|_| "请填写应用密码并测试连接".into());
        #[cfg(not(windows))]
        Err("此平台暂不支持 WebDAV 凭据保存".into())
    }

    pub(crate) fn save(&self, password: &str) -> Result<(), String> {
        let root = model::config_dir();
        fs::create_dir_all(&root).map_err(|_| "无法创建本机配置目录")?;
        #[cfg(windows)]
        self.credential()?
            .set_password(password)
            .map_err(|_| "无法保存 WebDAV 应用密码")?;
        #[cfg(not(windows))]
        let _ = password;
        let bytes = serde_json::to_vec_pretty(self).map_err(|_| "无法生成 WebDAV 连接配置")?;
        model::atomic_write(&root.join("config-webdav.json"), &bytes)
            .map_err(|_| "无法保存 WebDAV 连接配置".into())
    }
}

struct RemoteFile {
    bytes: Vec<u8>,
    tag: Option<String>,
    modified: Option<String>,
}

pub(crate) struct Client {
    agent: ureq::Agent,
    authorization: String,
    directories: Vec<Url>,
    file: Url,
}

impl Client {
    pub(crate) fn new(connection: &Connection, password: &str) -> Result<Self, String> {
        let url = Url::parse(&connection.server).map_err(|_| "服务器地址无效")?;
        if password.is_empty() || password.len() > 1024 {
            return Err("请输入 WebDAV 应用密码".into());
        }
        Self::with_agent(
            url,
            connection,
            password,
            ureq::AgentBuilder::new()
                .timeout(Duration::from_secs(15))
                .redirects(0)
                .build(),
        )
    }

    fn with_agent(
        mut root: Url,
        connection: &Connection,
        password: &str,
        agent: ureq::Agent,
    ) -> Result<Self, String> {
        root.path_segments_mut()
            .map_err(|_| "服务器地址无效")?
            .pop_if_empty();
        let mut directories = vec![root.clone()];
        for name in connection
            .directory
            .split('/')
            .filter(|name| !name.is_empty())
        {
            root.path_segments_mut()
                .map_err(|_| "目录地址无效")?
                .push(name);
            directories.push(root.clone());
        }
        root.path_segments_mut()
            .map_err(|_| "配置文件地址无效")?
            .push(FILE_NAME);
        Ok(Self {
            agent,
            authorization: format!(
                "Basic {}",
                STANDARD.encode(format!("{}:{password}", connection.username))
            ),
            directories,
            file: root,
        })
    }

    fn request(&self, method: &str, url: &Url) -> ureq::Request {
        self.agent
            .request(method, url.as_str())
            .set("Authorization", &self.authorization)
    }

    fn probe(&self, directory: &Url) -> Result<bool, String> {
        let response = response(self.request("PROPFIND", directory).set("Depth", "0").call())?;
        match response.status() {
            200 | 207 => Ok(true),
            404 => Ok(false),
            code => Err(status_error("检查目录", code)),
        }
    }

    pub(crate) fn ensure_directory(&self) -> Result<(), String> {
        let [root, folders @ ..] = self.directories.as_slice() else {
            return Err("缺少服务器地址".into());
        };
        if !self.probe(root)? {
            return Err("WebDAV 入口不存在，请检查服务器地址".into());
        }
        for folder in folders {
            if self.probe(folder)? {
                continue;
            }
            let result = response(self.request("MKCOL", folder).call())?;
            match result.status() {
                200 | 201 | 204 => {}
                405 | 409 if self.probe(folder)? => {}
                code => return Err(status_error("创建目录", code)),
            }
        }
        Ok(())
    }

    fn fetch(&self) -> Result<Option<RemoteFile>, String> {
        let mut url = self.file.clone();
        let mut attach_auth = true;
        let mut validators = (None, None);
        for redirect_count in 0..=4 {
            let request = if attach_auth {
                self.request("GET", &url)
            } else {
                self.agent.get(url.as_str())
            };
            let result = response(request.call())?;
            if redirect_count == 0 || attach_auth {
                validators = (
                    strong_tag(&result),
                    result.header("Last-Modified").map(str::to_owned),
                );
            }
            match result.status() {
                200 => {
                    let mut bytes = Vec::new();
                    result
                        .into_reader()
                        .take(MAX_BYTES + 1)
                        .read_to_end(&mut bytes)
                        .map_err(|_| "无法读取远端配置文件")?;
                    if bytes.len() as u64 > MAX_BYTES {
                        return Err("远端配置文件超过 256 KB".into());
                    }
                    return Ok(Some(RemoteFile {
                        bytes,
                        tag: validators.0,
                        modified: validators.1,
                    }));
                }
                404 if redirect_count == 0 => return Ok(None),
                301 | 302 | 303 | 307 | 308 => {
                    let next = result
                        .header("Location")
                        .and_then(|location| url.join(location).ok())
                        .filter(|next| {
                            next.scheme() == "https"
                                && next.host_str().is_some()
                                && next.username().is_empty()
                                && next.password().is_none()
                        })
                        .ok_or("下载转向了不受支持的地址")?;
                    attach_auth &= next.origin() == self.file.origin();
                    url = next;
                }
                code => return Err(status_error("下载配置", code)),
            }
        }
        Err("下载跳转次数过多".into())
    }

    pub(crate) fn download(&self) -> Result<Bundle, String> {
        let file = self
            .fetch()?
            .ok_or("远端还没有配置文件，请先从一台电脑上传")?;
        Bundle::decode(&file.bytes)
    }

    pub(crate) fn upload(&self, incoming: Bundle) -> Result<(), String> {
        self.ensure_directory()?;
        let remote = self.fetch()?;
        let body = if let Some(file) = &remote {
            let mut bundle = Bundle::decode(&file.bytes)
                .map_err(|_| "远端已有无法识别的配置文件，已保留原文件")?;
            bundle.merge(incoming);
            bundle.encode()?
        } else {
            incoming.encode()?
        };
        let request = self
            .request("PUT", &self.file)
            .set("Content-Type", "application/json; charset=utf-8");
        let request = match &remote {
            None => request.set("If-None-Match", "*"),
            Some(file) if file.tag.is_some() => {
                request.set("If-Match", file.tag.as_deref().unwrap())
            }
            Some(file) if file.modified.is_some() => {
                request.set("If-Unmodified-Since", file.modified.as_deref().unwrap())
            }
            Some(file) => {
                if self.fetch()?.as_ref().map(|current| &current.bytes) != Some(&file.bytes) {
                    return Err(status_error("上传配置", 412));
                }
                request.set("If-Match", "*")
            }
        };
        let result = response(request.send_bytes(&body))?;
        match result.status() {
            200 | 201 | 204 => Ok(()),
            code => Err(status_error("上传配置", code)),
        }
    }
}

fn strong_tag(response: &ureq::Response) -> Option<String> {
    response
        .header("ETag")
        .filter(|tag| tag.starts_with('"') && tag.ends_with('"'))
        .map(str::to_owned)
}

fn response(result: Result<ureq::Response, ureq::Error>) -> Result<ureq::Response, String> {
    match result {
        Ok(response) | Err(ureq::Error::Status(_, response)) => Ok(response),
        Err(_) => Err("无法连接 WebDAV，请检查网络、地址和 HTTPS 证书".into()),
    }
}

fn status_error(action: &str, code: u16) -> String {
    match code {
        401 => "WebDAV 账号或应用密码验证失败".into(),
        403 => format!("{action}被服务器拒绝，请检查 WebDAV 权限（HTTP 403）"),
        412 => "远端配置已变化，请下载预览后重试".into(),
        423 => "远端配置被锁定，请稍后重试".into(),
        429 => "WebDAV 请求过多，请稍后重试".into(),
        _ => format!("{action}失败（HTTP {code}）"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::{BufRead, BufReader, Write},
        net::TcpListener,
    };

    #[test]
    fn config_webdav_validates_connection_and_encodes_subfolders() {
        let connection = Connection {
            server: "https://dav.example.test/webdav/".into(),
            directory: "配置测试\\Leidao".into(),
            username: "test".into(),
        };
        let clean = connection.normalized().unwrap();
        assert_eq!(clean.directory, "配置测试/Leidao");
        let client = Client::new(&clean, "fixture").unwrap();
        assert_eq!(
            client.file.as_str(),
            "https://dav.example.test/webdav/%E9%85%8D%E7%BD%AE%E6%B5%8B%E8%AF%95/Leidao/Leidao-config.json"
        );
        for value in [
            "http://dav.example.test",
            "https://user:pass@dav.example.test",
            "https://dav.example.test/?token=x",
        ] {
            assert!(Connection {
                server: value.into(),
                ..connection.clone()
            }
            .normalized()
            .is_err());
        }
        assert!(Connection {
            directory: "parent/../escape".into(),
            ..connection
        }
        .normalized()
        .is_err());
    }

    #[test]
    fn config_webdav_creates_directories_merges_upload_and_respects_conflict() {
        const EXISTING: &str = r#"{"format":"leidao-config","version":1,"focus":{"minutes":35,"rest_minutes":6,"remind":true}}"#;
        const INCOMING: &str =
            r#"{"format":"leidao-config","version":1,"sedentary":{"enabled":true,"minutes":45}}"#;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = Url::parse(&format!("http://{}/dav", listener.local_addr().unwrap())).unwrap();
        let file = "/dav/sync/island/Leidao-config.json";
        let server = std::thread::spawn(move || {
            for (method, path, status, is_fixture) in [
                ("PROPFIND", "/dav", "207 Multi-Status", false),
                ("PROPFIND", "/dav/sync", "404 Not Found", false),
                ("MKCOL", "/dav/sync", "201 Created", false),
                ("PROPFIND", "/dav/sync/island", "404 Not Found", false),
                ("MKCOL", "/dav/sync/island", "201 Created", false),
                ("GET", file, "200 OK", true),
                ("PUT", file, "412 Precondition Failed", false),
                ("GET", file, "200 OK", true),
            ] {
                let (stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(3)))
                    .unwrap();
                let mut stream = BufReader::new(stream);
                let mut first = String::new();
                stream.read_line(&mut first).unwrap();
                assert_eq!(first.trim_end(), format!("{method} {path} HTTP/1.1"));
                let mut headers = String::new();
                loop {
                    let mut line = String::new();
                    let read = stream.read_line(&mut line).unwrap();
                    if read == 0 || line == "\r\n" {
                        break;
                    }
                    headers.push_str(&line);
                }
                let lowercase = headers.to_ascii_lowercase();
                assert!(lowercase.contains("authorization: basic "));
                if method == "PROPFIND" {
                    assert!(lowercase.contains("depth: 0"));
                }
                if method == "PUT" {
                    assert!(lowercase.contains("if-match: \"rev1\""));
                    let length = lowercase
                        .lines()
                        .find_map(|line| {
                            line.strip_prefix("content-length: ")
                                .and_then(|length| length.parse::<usize>().ok())
                        })
                        .unwrap();
                    let mut bytes = vec![0; length];
                    stream.read_exact(&mut bytes).unwrap();
                    let bundle = Bundle::decode(&bytes).unwrap();
                    assert!(bundle.contains(crate::config_sync::Section::Focus));
                    assert!(bundle.contains(crate::config_sync::Section::Sedentary));
                }
                let body = if is_fixture { EXISTING } else { "" };
                let etag = if is_fixture { "ETag: \"rev1\"\r\n" } else { "" };
                write!(stream.get_mut(), "HTTP/1.1 {status}\r\nContent-Length: {}\r\n{etag}Connection: close\r\n\r\n{body}", body.len()).unwrap();
            }
        });
        let connection = Connection {
            server: url.to_string(),
            directory: "sync/island".into(),
            username: "demo".into(),
        };
        let agent = ureq::AgentBuilder::new()
            .timeout(Duration::from_secs(3))
            .redirects(0)
            .build();
        let client = Client::with_agent(url, &connection, "fixture", agent).unwrap();
        let outcome = client.upload(Bundle::decode(INCOMING.as_bytes()).unwrap());
        assert!(outcome.unwrap_err().contains("远端配置已变化"));
        assert!(client
            .download()
            .unwrap()
            .contains(crate::config_sync::Section::Focus));
        server.join().unwrap();
    }
}
