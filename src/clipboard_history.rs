//! Bounded local text/image history. Native access is confined to a worker thread.
use serde_json::{json, Value};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::mpsc::{self, Receiver, Sender},
    time::Duration,
};

const MAX_ITEMS: usize = 100;
const MAX_TOTAL: u64 = 64 * 1024 * 1024;
const MAX_TEXT: usize = 100_000;
const MAX_IMAGE: usize = 16 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq)]
pub enum Content {
    Text(String),
    Image { width: u32, height: u32 },
}
#[derive(Clone, Debug)]
pub struct Entry {
    pub id: String,
    pub content: Content,
    pub favorite: bool,
    pub at: u64,
    pub bytes: u64,
}
impl Entry {
    pub fn summary(&self) -> String {
        match &self.content {
            Content::Text(t) => t.lines().next().unwrap_or("").chars().take(120).collect(),
            Content::Image { width, height } => format!("图片 · {width} × {height}"),
        }
    }
    pub fn matches(&self, query: &str) -> bool {
        let q = query.trim().to_lowercase();
        q.is_empty()
            || match &self.content {
                Content::Text(t) => t.to_lowercase().contains(&q),
                Content::Image { .. } => self.summary().to_lowercase().contains(&q),
            }
    }
}
#[derive(Clone, Default)]
pub struct Snapshot {
    pub entries: Vec<Entry>,
    pub error: String,
    pub enabled: bool,
    pub notice: String,
}
pub struct Store {
    dir: PathBuf,
    pub entries: Vec<Entry>,
}
fn digest(bytes: &[u8]) -> String {
    ring::digest::digest(&ring::digest::SHA256, bytes)
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}
fn valid_id(id: &str) -> bool {
    id.len() == 64 && id.bytes().all(|b| b.is_ascii_hexdigit())
}
impl Store {
    pub fn load(dir: PathBuf) -> Result<Self, String> {
        fs::create_dir_all(dir.join("images")).map_err(|e| e.to_string())?;
        let path = dir.join("history.json");
        let raw = match fs::read(&path) {
            Ok(v) => v,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self {
                    dir,
                    entries: vec![],
                })
            }
            Err(e) => return Err(e.to_string()),
        };
        if raw.len() > 20 * 1024 * 1024 {
            return Err("剪贴板记录文件超过大小限制".into());
        }
        let values: Value =
            serde_json::from_slice(&raw).map_err(|_| "剪贴板历史损坏，未覆盖原文件")?;
        let mut entries = Vec::new();
        for v in values
            .as_array()
            .ok_or("剪贴板历史格式错误")?
            .iter()
            .take(MAX_ITEMS)
        {
            let id = v["id"].as_str().unwrap_or_default();
            if !valid_id(id) || entries.iter().any(|e: &Entry| e.id == id) {
                continue;
            }
            let content = if let Some(t) = v["text"].as_str() {
                if t.len() > MAX_TEXT {
                    continue;
                }
                Content::Text(t.into())
            } else {
                let (w, h) = (
                    v["width"].as_u64().unwrap_or(0),
                    v["height"].as_u64().unwrap_or(0),
                );
                if w == 0
                    || h == 0
                    || w > 8192
                    || h > 8192
                    || w * h * 4 > MAX_IMAGE as u64
                    || !dir.join("images").join(format!("{id}.png")).is_file()
                {
                    continue;
                }
                Content::Image {
                    width: w as u32,
                    height: h as u32,
                }
            };
            entries.push(Entry {
                id: id.into(),
                content,
                favorite: v["favorite"].as_bool().unwrap_or(false),
                at: v["at"].as_u64().unwrap_or(0),
                bytes: v["bytes"]
                    .as_u64()
                    .unwrap_or(MAX_IMAGE as u64)
                    .min(MAX_IMAGE as u64),
            });
        }
        Ok(Self { dir, entries })
    }
    pub fn image_path(&self, id: &str) -> PathBuf {
        self.dir.join("images").join(format!("{id}.png"))
    }
    pub fn thumbnail_path(&self, id: &str) -> PathBuf {
        self.dir.join("images").join(format!("{id}-thumb.png"))
    }
    fn save(&self) -> Result<(), String> {
        let entries: Vec<_> = self
            .entries
            .iter()
            .map(|e| {
                let mut v = json!({"id":e.id,"favorite":e.favorite,"at":e.at,"bytes":e.bytes});
                match &e.content {
                    Content::Text(t) => v["text"] = json!(t),
                    Content::Image { width, height } => {
                        v["width"] = json!(width);
                        v["height"] = json!(height);
                    }
                }
                v
            })
            .collect();
        crate::model::atomic_write(
            &self.dir.join("history.json"),
            serde_json::to_string(&entries).unwrap().as_bytes(),
        )
        .map_err(|e| e.to_string())
    }
    fn insert(&mut self, mut entry: Entry) -> Result<(), String> {
        let before = self.entries.clone();
        let inserted_id = entry.id.clone();
        if let Some(index) = self.entries.iter().position(|e| e.id == entry.id) {
            entry.favorite = self.entries.remove(index).favorite;
        }
        self.entries.insert(0, entry);
        let mut removed = vec![];
        while self.entries.len() > MAX_ITEMS
            || self.entries.iter().map(|e| e.bytes).sum::<u64>() > MAX_TOTAL
        {
            if let Some(index) = self.entries.iter().rposition(|e| !e.favorite) {
                removed.push(self.entries.remove(index));
            } else {
                self.entries = before;
                return Err("收藏已达到容量上限，请先移除部分记录".into());
            }
        }
        if !self.entries.iter().any(|e| e.id == inserted_id) {
            self.entries = before;
            return Err("收藏已达到容量上限，本次未记录，请先移除部分收藏".into());
        }
        if let Err(e) = self.save() {
            self.entries = before;
            return Err(e);
        }
        for e in removed {
            self.remove_images(&e);
        }
        Ok(())
    }
    pub fn text(&mut self, text: String) -> Result<(), String> {
        if text.trim().is_empty() {
            return Ok(());
        }
        if text.len() > MAX_TEXT {
            return Err("文本超过 100 KB，本次未记录".into());
        }
        let id = digest(format!("text:{text}").as_bytes());
        let bytes = text.len() as u64;
        self.insert(Entry {
            id,
            content: Content::Text(text),
            favorite: false,
            at: crate::codex_tasks::now_ms(),
            bytes,
        })
    }
    pub fn image(&mut self, width: u32, height: u32, rgba: &[u8]) -> Result<(), String> {
        if width == 0
            || height == 0
            || width > 8192
            || height > 8192
            || rgba.len() > MAX_IMAGE
            || width as usize * height as usize * 4 != rgba.len()
        {
            return Err("图片过大或格式无效，本次未记录".into());
        }
        let mut key = Vec::with_capacity(rgba.len() + 8);
        key.extend(width.to_le_bytes());
        key.extend(height.to_le_bytes());
        key.extend(rgba);
        let id = digest(&key);
        let image =
            image::RgbaImage::from_raw(width, height, rgba.to_vec()).ok_or("图片格式无效")?;
        let save_png = |path: &Path, im: &image::RgbaImage| -> Result<(), String> {
            let mut bytes = std::io::Cursor::new(Vec::new());
            im.write_to(&mut bytes, image::ImageFormat::Png)
                .map_err(|e| e.to_string())?;
            crate::model::atomic_write(path, bytes.get_ref()).map_err(|e| e.to_string())
        };
        save_png(&self.image_path(&id), &image)?;
        save_png(
            &self.thumbnail_path(&id),
            &image::imageops::thumbnail(&image, 160, 96),
        )?;
        let existed = self.entries.iter().any(|e| e.id == id);
        let result = self.insert(Entry {
            id: id.clone(),
            content: Content::Image { width, height },
            favorite: false,
            at: crate::codex_tasks::now_ms(),
            bytes: rgba.len() as u64,
        });
        if result.is_err() && !existed {
            let _ = fs::remove_file(self.image_path(&id));
            let _ = fs::remove_file(self.thumbnail_path(&id));
        }
        result
    }
    fn remove_images(&self, e: &Entry) {
        if matches!(e.content, Content::Image { .. }) {
            let _ = fs::remove_file(self.image_path(&e.id));
            let _ = fs::remove_file(self.thumbnail_path(&e.id));
        }
    }
    pub fn delete(&mut self, id: &str) -> Result<(), String> {
        let before = self.entries.clone();
        let removed: Vec<_> = self
            .entries
            .iter()
            .filter(|e| e.id == id)
            .cloned()
            .collect();
        self.entries.retain(|e| e.id != id);
        if let Err(e) = self.save() {
            self.entries = before;
            return Err(e);
        }
        for e in removed {
            self.remove_images(&e);
        }
        Ok(())
    }
    pub fn favorite(&mut self, id: &str) -> Result<(), String> {
        let before = self.entries.clone();
        if let Some(e) = self.entries.iter_mut().find(|e| e.id == id) {
            e.favorite = !e.favorite;
        }
        if let Err(e) = self.save() {
            self.entries = before;
            return Err(e);
        }
        Ok(())
    }
    pub fn clear(&mut self) -> Result<(), String> {
        let before = std::mem::take(&mut self.entries);
        if let Err(e) = self.save() {
            self.entries = before;
            return Err(e);
        }
        for e in before {
            self.remove_images(&e);
        }
        Ok(())
    }
}

pub fn directory() -> PathBuf {
    crate::model::config_dir().join("clipboard")
}
#[derive(Clone, Debug, PartialEq)]
pub enum Command {
    Configure { enabled: bool, images: bool },
    Copy(String),
    Favorite(String),
    Delete(String),
    Clear,
}
pub struct Service {
    tx: Sender<Command>,
    rx: Receiver<Snapshot>,
}
impl Service {
    pub fn send(&self, command: Command) {
        let _ = self.tx.send(command);
    }
    pub fn try_recv(&self) -> Option<Snapshot> {
        self.rx.try_recv().ok()
    }
    pub fn start(ctx: eframe::egui::Context, enabled: bool, images: bool) -> Self {
        let (tx, commands) = mpsc::channel();
        let (results, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let mut store = match Store::load(directory()) {
                Ok(s) => s,
                Err(error) => {
                    let _ = results.send(Snapshot {
                        error,
                        ..Default::default()
                    });
                    ctx.request_repaint();
                    return;
                }
            };
            let (mut enabled, mut images) = (enabled, images);
            #[cfg(windows)]
            let mut clipboard = arboard::Clipboard::new().ok();
            #[cfg(windows)]
            let mut sequence =
                unsafe { windows::Win32::System::DataExchange::GetClipboardSequenceNumber() };
            let publish = |store: &Store, error: String, enabled: bool, notice: String| {
                let ok = results
                    .send(Snapshot {
                        entries: store.entries.clone(),
                        error,
                        enabled,
                        notice,
                    })
                    .is_ok();
                ctx.request_repaint();
                ok
            };
            if !publish(&store, String::new(), enabled, String::new()) {
                return;
            }
            loop {
                match commands.recv_timeout(Duration::from_millis(350)) {
                    Ok(command) => {
                        let copied = matches!(&command, Command::Copy(_));
                        let result = match command {
                            Command::Configure {
                                enabled: e,
                                images: i,
                            } => {
                                enabled = e;
                                images = i;
                                #[cfg(windows)]
                                {
                                    sequence = unsafe {
                                        windows::Win32::System::DataExchange::GetClipboardSequenceNumber()
                                    };
                                }
                                Ok(())
                            }
                            Command::Favorite(id) => store.favorite(&id),
                            Command::Delete(id) => store.delete(&id),
                            Command::Clear => store.clear(),
                            Command::Copy(id) => {
                                #[cfg(windows)]
                                {
                                    let result = copy(&store, &id, &mut clipboard);
                                    sequence = unsafe {
                                        windows::Win32::System::DataExchange::GetClipboardSequenceNumber()
                                    };
                                    result
                                }
                                #[cfg(not(windows))]
                                {
                                    let _ = id;
                                    Err("当前平台不支持剪贴板写入".into())
                                }
                            }
                        };
                        let notice = if copied && result.is_ok() {
                            "已复制，可切回目标应用粘贴".into()
                        } else {
                            String::new()
                        };
                        if !publish(&store, result.err().unwrap_or_default(), enabled, notice) {
                            return;
                        }
                    }
                    Err(mpsc::RecvTimeoutError::Disconnected) => return,
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                }
                #[cfg(windows)]
                if enabled {
                    let current = unsafe {
                        windows::Win32::System::DataExchange::GetClipboardSequenceNumber()
                    };
                    if current == sequence {
                        continue;
                    }
                    if excluded() {
                        sequence = current;
                        continue;
                    }
                    if clipboard.is_none() {
                        clipboard = arboard::Clipboard::new().ok();
                    }
                    if let Some(board) = clipboard.as_mut() {
                        let captured = if let Ok(text) = board.get_text() {
                            Some(store.text(text))
                        } else if images {
                            board.get_image().ok().map(|image| {
                                store.image(image.width as u32, image.height as u32, &image.bytes)
                            })
                        } else {
                            None
                        };
                        // A busy clipboard is retried on the next tick.
                        if let Some(result) = captured {
                            sequence = current;
                            if !publish(
                                &store,
                                result.err().unwrap_or_default(),
                                enabled,
                                String::new(),
                            ) {
                                return;
                            }
                        }
                    }
                }
            }
        });
        Self { tx, rx }
    }
}
#[cfg(windows)]
fn excluded() -> bool {
    unsafe {
        use windows::{core::w, Win32::System::DataExchange::*};
        let format = RegisterClipboardFormatW(w!("ExcludeClipboardContentFromMonitorProcessing"));
        IsClipboardFormatAvailable(format).is_ok()
    }
}
#[cfg(windows)]
fn copy(store: &Store, id: &str, clipboard: &mut Option<arboard::Clipboard>) -> Result<(), String> {
    let entry = store
        .entries
        .iter()
        .find(|e| e.id == id)
        .ok_or("记录已不存在")?;
    if clipboard.is_none() {
        *clipboard = Some(arboard::Clipboard::new().map_err(|e| e.to_string())?);
    }
    let board = clipboard.as_mut().unwrap();
    match &entry.content {
        Content::Text(t) => board.set_text(t.clone()).map_err(|e| e.to_string()),
        Content::Image { .. } => {
            let image = image::open(store.image_path(id))
                .map_err(|e| e.to_string())?
                .into_rgba8();
            board
                .set_image(arboard::ImageData {
                    width: image.width() as usize,
                    height: image.height() as usize,
                    bytes: std::borrow::Cow::Owned(image.into_raw()),
                })
                .map_err(|e| e.to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(windows)]
    #[test]
    #[ignore = "显式操作本机剪贴板，须由外层测试程序保存并恢复原内容"]
    fn native_service_captures_and_copies_text_and_image() {
        let root =
            std::env::temp_dir().join(format!("daozai-native-clipboard-{}", std::process::id()));
        std::env::set_var("DAOZAI_DATA_DIR", &root);
        let service = Service::start(eframe::egui::Context::default(), true, true);
        fn wait(service: &Service, predicate: impl Fn(&Snapshot) -> bool) -> Snapshot {
            let until = std::time::Instant::now() + Duration::from_secs(5);
            while std::time::Instant::now() < until {
                if let Some(snapshot) = service.try_recv() {
                    assert!(snapshot.error.is_empty(), "{}", snapshot.error);
                    if predicate(&snapshot) {
                        return snapshot;
                    }
                }
                std::thread::sleep(Duration::from_millis(25));
            }
            panic!("剪贴板后台服务未按时响应");
        }
        wait(&service, |s| s.enabled);
        let mut board = arboard::Clipboard::new().unwrap();
        board.set_text("岛仔原生剪贴板测试".to_string()).unwrap();
        let text = wait(&service, |s| {
            s.entries
                .iter()
                .any(|e| matches!(&e.content,Content::Text(t)if t=="岛仔原生剪贴板测试"))
        })
        .entries
        .into_iter()
        .find(|e| matches!(&e.content,Content::Text(t)if t=="岛仔原生剪贴板测试"))
        .unwrap();
        board.set_text("另一个合成样本".to_string()).unwrap();
        service.send(Command::Copy(text.id));
        wait(&service, |s| !s.notice.is_empty());
        assert_eq!(board.get_text().unwrap(), "岛仔原生剪贴板测试");
        let pixels = [40u8, 140, 220, 255].repeat(8 * 8);
        board
            .set_image(arboard::ImageData {
                width: 8,
                height: 8,
                bytes: std::borrow::Cow::Borrowed(&pixels),
            })
            .unwrap();
        let image = wait(&service, |s| {
            s.entries.iter().any(|e| {
                matches!(
                    e.content,
                    Content::Image {
                        width: 8,
                        height: 8
                    }
                )
            })
        })
        .entries
        .into_iter()
        .find(|e| {
            matches!(
                e.content,
                Content::Image {
                    width: 8,
                    height: 8
                }
            )
        })
        .unwrap();
        board.set_text("图片复制回测".to_string()).unwrap();
        service.send(Command::Copy(image.id));
        wait(&service, |s| !s.notice.is_empty());
        let actual = board.get_image().unwrap();
        assert_eq!((actual.width, actual.height), (8, 8));
        assert_eq!(actual.bytes.as_ref(), pixels.as_slice());
        service.send(Command::Configure {
            enabled: false,
            images: true,
        });
        wait(&service, |s| !s.enabled);
    }
    fn store() -> Store {
        let root = std::env::temp_dir().join(format!(
            "daozai-clipboard-test-{}-{}",
            std::process::id(),
            crate::codex_tasks::now_ms()
        ));
        Store::load(root).unwrap()
    }
    #[test]
    fn dedup_preserves_favorite_and_searches_full_text() {
        let mut s = store();
        s.text("first\nneedle".into()).unwrap();
        let id = s.entries[0].id.clone();
        s.favorite(&id).unwrap();
        s.text("second".into()).unwrap();
        s.text("first\nneedle".into()).unwrap();
        assert_eq!(s.entries.len(), 2);
        assert!(s.entries[0].favorite);
        assert!(s.entries[0].matches("NEEDLE"));
    }
    #[test]
    fn cap_evicts_oldest_unpinned_and_roundtrips() {
        let mut s = store();
        s.text("pinned".into()).unwrap();
        let id = s.entries[0].id.clone();
        s.favorite(&id).unwrap();
        for i in 0..105 {
            s.text(format!("item {i}")).unwrap();
        }
        assert_eq!(s.entries.len(), MAX_ITEMS);
        assert!(s.entries.iter().any(|e| e.id == id));
        assert_eq!(Store::load(s.dir.clone()).unwrap().entries.len(), MAX_ITEMS);
    }
    #[test]
    fn image_roundtrip_delete_and_clear_remove_owned_files_only() {
        let mut s = store();
        s.image(2, 1, &[255; 8]).unwrap();
        let id = s.entries[0].id.clone();
        assert!(s.image_path(&id).is_file());
        assert!(s.thumbnail_path(&id).is_file());
        let external = s.dir.join("leave.txt");
        fs::write(&external, "keep").unwrap();
        s.clear().unwrap();
        assert!(!s.image_path(&id).exists());
        assert!(external.exists());
        assert!(Store::load(s.dir.clone()).unwrap().entries.is_empty());
    }
    #[test]
    fn rejects_oversize_and_path_traversal() {
        let mut s = store();
        assert!(s.text("x".repeat(MAX_TEXT + 1)).is_err());
        assert!(s.image(8193, 1, &[]).is_err());
        fs::write(
            s.dir.join("history.json"),
            r#"[{"id":"../../foreign","width":1,"height":1}]"#,
        )
        .unwrap();
        assert!(Store::load(s.dir.clone()).unwrap().entries.is_empty());
    }
}
