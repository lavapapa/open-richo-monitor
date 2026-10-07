use std::{
    collections::BTreeSet,
    fmt, fs,
    io::Write,
    path::{Path, PathBuf},
    time::Duration,
};

use reqwest::{header, redirect::Policy, Client};
use serde::{Deserialize, Serialize};

pub const MAX_IMAGE_BYTES: u64 = 8 * 1024 * 1024;
pub const MAX_CACHE_BYTES: u64 = 128 * 1024 * 1024;

#[derive(Debug)]
pub enum ImageCacheError {
    InvalidProductId,
    InvalidUrl,
    HttpStatus { status: u16 },
    UnsupportedContentType,
    ImageTooLarge { limit: u64 },
    CacheFull { limit: u64 },
    Transport(reqwest::Error),
    Io(std::io::Error),
    Index(serde_json::Error),
}

impl fmt::Display for ImageCacheError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidProductId => formatter.write_str("图片缓存的商品 ID 必须是正整数"),
            Self::InvalidUrl => formatter.write_str("商品图片地址必须是有效的 HTTP 或 HTTPS 地址"),
            Self::HttpStatus { status } => write!(formatter, "商品图片下载失败：HTTP {status}"),
            Self::UnsupportedContentType => {
                formatter.write_str("商品图片格式不支持，需要 JPEG、PNG、WebP 或 GIF")
            }
            Self::ImageTooLarge { limit } => {
                write!(formatter, "商品图片超过单张上限（{limit} 字节）")
            }
            Self::CacheFull { limit } => write!(
                formatter,
                "图片缓存已满（上限 {limit} 字节），请清理已移除商品的缓存"
            ),
            Self::Transport(error) => write!(formatter, "商品图片连接失败或下载超时：{error}"),
            Self::Io(error) => write!(formatter, "图片缓存读写失败：{error}"),
            Self::Index(error) => write!(formatter, "图片缓存索引保存失败：{error}"),
        }
    }
}

impl std::error::Error for ImageCacheError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Transport(error) => Some(error),
            Self::Io(error) => Some(error),
            Self::Index(error) => Some(error),
            _ => None,
        }
    }
}

impl From<std::io::Error> for ImageCacheError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<reqwest::Error> for ImageCacheError {
    fn from(error: reqwest::Error) -> Self {
        Self::Transport(error.without_url())
    }
}

impl From<serde_json::Error> for ImageCacheError {
    fn from(error: serde_json::Error) -> Self {
        Self::Index(error)
    }
}

#[derive(Deserialize, Serialize)]
struct CacheIndex {
    url: String,
    file_name: String,
}

/// 由主 worker 串行下载、替换和清理；快照通过 cached_path 读取本地文件路径。
pub struct ImageCache {
    root: PathBuf,
    client: Client,
    nonce: u64,
    max_image_bytes: u64,
    max_cache_bytes: u64,
}

struct PendingFiles(Vec<PathBuf>);
impl Drop for PendingFiles {
    fn drop(&mut self) {
        for path in &self.0 {
            let _ = fs::remove_file(path);
        }
    }
}

impl ImageCache {
    pub fn new(
        root: PathBuf,
        use_system_proxy: bool,
        connect_timeout: Duration,
        total_timeout: Duration,
    ) -> Result<Self, ImageCacheError> {
        Self::with_limits(
            root,
            use_system_proxy,
            connect_timeout,
            total_timeout,
            MAX_IMAGE_BYTES,
            MAX_CACHE_BYTES,
        )
    }

    pub fn with_limits(
        root: PathBuf,
        use_system_proxy: bool,
        connect_timeout: Duration,
        total_timeout: Duration,
        max_image_bytes: u64,
        max_cache_bytes: u64,
    ) -> Result<Self, ImageCacheError> {
        let mut builder = Client::builder()
            .connect_timeout(connect_timeout)
            .timeout(total_timeout)
            .retry(reqwest::retry::never())
            .redirect(Policy::none());
        if !use_system_proxy {
            builder = builder.no_proxy();
        }
        let client = builder.build()?;
        fs::create_dir_all(&root)?;
        let mut nonce = 0;
        for entry in fs::read_dir(&root)? {
            let name = entry?.file_name();
            if let Some(value) = name
                .to_str()
                .and_then(|name| name.split_once('-'))
                .and_then(|(_, tail)| tail.split('.').next())
                .and_then(|value| value.parse::<u64>().ok())
            {
                nonce = nonce.max(value);
            }
        }
        Ok(Self {
            root,
            client,
            nonce,
            max_image_bytes,
            max_cache_bytes,
        })
    }

    pub async fn cache(&mut self, product_id: u64, url: &str) -> Result<PathBuf, ImageCacheError> {
        if product_id == 0 {
            return Err(ImageCacheError::InvalidProductId);
        }
        let parsed_url = reqwest::Url::parse(url).map_err(|_| ImageCacheError::InvalidUrl)?;
        if !matches!(parsed_url.scheme(), "http" | "https") || parsed_url.host_str().is_none() {
            return Err(ImageCacheError::InvalidUrl);
        }
        if let Some(path) = self.cached_path(product_id, url) {
            return Ok(path);
        }

        let mut response = self.client.get(parsed_url).send().await?;
        if !response.status().is_success() {
            return Err(ImageCacheError::HttpStatus {
                status: response.status().as_u16(),
            });
        }
        let content_type = response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.split(';').next())
            .map(|value| value.trim().to_ascii_lowercase())
            .unwrap_or_default();
        let extension = match content_type.as_str() {
            "image/jpeg" => "jpg",
            "image/png" => "png",
            "image/webp" => "webp",
            "image/gif" => "gif",
            _ => return Err(ImageCacheError::UnsupportedContentType),
        };
        let used_bytes = self.disk_bytes()?;
        if let Some(size) = response.content_length() {
            self.check_size(used_bytes, size)?;
        }
        self.nonce += 1;
        let file_name = format!("{product_id}-{}.{}", self.nonce, extension);
        let final_path = self.root.join(&file_name);
        let temporary_path = self.root.join(format!("{file_name}.tmp"));
        let temporary_index = self
            .root
            .join(format!("{product_id}-{}.json.tmp", self.nonce));
        let mut pending = PendingFiles(vec![
            temporary_path.clone(),
            final_path.clone(),
            temporary_index.clone(),
        ]);
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary_path)?;
        let mut image_bytes = 0;
        while let Some(chunk) = response.chunk().await? {
            image_bytes += chunk.len() as u64;
            self.check_size(used_bytes, image_bytes)?;
            file.write_all(&chunk)?;
        }
        drop(file);
        fs::rename(&temporary_path, &final_path)?;
        let old_index = self.read_index(product_id);
        let new_index = CacheIndex {
            url: url.to_owned(),
            file_name,
        };
        fs::write(&temporary_index, serde_json::to_vec(&new_index)?)?;
        fs::rename(&temporary_index, self.index_path(product_id))?;
        pending.0.clear();
        if let Some(old) = old_index {
            let old_path = self.root.join(old.file_name);
            if old_path != final_path && old_path.exists() {
                fs::remove_file(old_path)?;
            }
        }
        Ok(final_path)
    }

    fn check_size(&self, used_bytes: u64, image_bytes: u64) -> Result<(), ImageCacheError> {
        if image_bytes > self.max_image_bytes {
            return Err(ImageCacheError::ImageTooLarge {
                limit: self.max_image_bytes,
            });
        }
        if image_bytes > self.max_cache_bytes.saturating_sub(used_bytes) {
            return Err(ImageCacheError::CacheFull {
                limit: self.max_cache_bytes,
            });
        }
        Ok(())
    }

    pub fn cached_path(&self, product_id: u64, url: &str) -> Option<PathBuf> {
        let (cached_url, path) = Self::cached_image(&self.root, product_id)?;
        (cached_url == url).then_some(path)
    }

    /// 首次快照和缺少图片网址的资料也能直接恢复持久缓存。
    pub fn cached_image(root: &Path, product_id: u64) -> Option<(String, PathBuf)> {
        let index: CacheIndex =
            serde_json::from_slice(&fs::read(root.join(format!("{product_id}.json"))).ok()?)
                .ok()?;
        let path = root.join(index.file_name);
        path.is_file().then_some((index.url, path))
    }

    pub fn remove(&self, product_id: u64) -> Result<(), ImageCacheError> {
        let prefix = format!("{product_id}-");
        let index = format!("{product_id}.json");
        for entry in fs::read_dir(&self.root)? {
            let entry = entry?;
            let name = entry.file_name();
            if entry.file_type()?.is_file()
                && name.to_str().is_some_and(|name| {
                    name.starts_with(&prefix) || name == index || name == format!("{index}.tmp")
                })
            {
                fs::remove_file(entry.path())?;
            }
        }
        Ok(())
    }

    pub fn prune(&self, valid_product_ids: &[u64]) -> Result<(), ImageCacheError> {
        let mut retained = BTreeSet::new();
        for &id in valid_product_ids {
            if let Some(index) = self.read_index(id) {
                let image = self.root.join(index.file_name);
                if image.is_file() {
                    retained.insert(image);
                    retained.insert(self.index_path(id));
                }
            }
        }
        for entry in fs::read_dir(&self.root)? {
            let entry = entry?;
            if entry.file_type()?.is_file() && !retained.contains(&entry.path()) {
                fs::remove_file(entry.path())?;
            }
        }
        Ok(())
    }

    /// 统计图片与未完成下载占用的字节，JSON 索引不计入图片预算。
    pub fn disk_bytes(&self) -> Result<u64, ImageCacheError> {
        let mut bytes = 0;
        for entry in fs::read_dir(&self.root)? {
            let entry = entry?;
            let name = entry.file_name();
            if entry.file_type()?.is_file()
                && name.to_str().is_some_and(|name| {
                    let name = name.strip_suffix(".tmp").unwrap_or(name);
                    matches!(
                        name.rsplit('.').next(),
                        Some("jpg" | "png" | "webp" | "gif")
                    )
                })
            {
                bytes += entry.metadata()?.len();
            }
        }
        Ok(bytes)
    }

    fn index_path(&self, product_id: u64) -> PathBuf {
        self.root.join(format!("{product_id}.json"))
    }

    fn read_index(&self, product_id: u64) -> Option<CacheIndex> {
        serde_json::from_slice(&fs::read(self.index_path(product_id)).ok()?).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs,
        sync::{
            atomic::{AtomicU64, AtomicUsize, Ordering},
            Arc,
        },
    };
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
        task::JoinHandle,
    };

    static TEST_NONCE: AtomicU64 = AtomicU64::new(0);

    struct TestRoot(PathBuf);
    impl TestRoot {
        fn new() -> Self {
            Self(std::env::temp_dir().join(format!(
                "ricoh-image-cache-test-{}-{}",
                std::process::id(),
                TEST_NONCE.fetch_add(1, Ordering::Relaxed)
            )))
        }
        fn cache(&self, single: u64, total: u64) -> ImageCache {
            ImageCache::with_limits(
                self.0.clone(),
                false,
                Duration::from_secs(1),
                Duration::from_secs(2),
                single,
                total,
            )
            .unwrap()
        }
    }
    impl Drop for TestRoot {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    struct Server {
        base: String,
        calls: Arc<AtomicUsize>,
        task: JoinHandle<()>,
    }
    impl Server {
        async fn new() -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let base = format!("http://{}", listener.local_addr().unwrap());
            let calls = Arc::new(AtomicUsize::new(0));
            let count = calls.clone();
            let task = tokio::spawn(async move {
                loop {
                    let (mut socket, _) = listener.accept().await.unwrap();
                    let mut request = vec![0; 4096];
                    let read = socket.read(&mut request).await.unwrap();
                    let request = String::from_utf8_lossy(&request[..read]);
                    count.fetch_add(1, Ordering::SeqCst);
                    let response = if request.contains("/404 ") {
                        "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                            .to_owned()
                    } else if request.contains("/text ") {
                        "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 5\r\nConnection: close\r\n\r\nhello".to_owned()
                    } else if request.contains("/chunked ") {
                        "HTTP/1.1 200 OK\r\nContent-Type: image/png\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n5\r\nfirst\r\n5\r\nother\r\n0\r\n\r\n".to_owned()
                    } else if request.contains("/broken ") {
                        "HTTP/1.1 200 OK\r\nContent-Type: image/png\r\nContent-Length: 12\r\nConnection: close\r\n\r\nshort".to_owned()
                    } else if request.contains("/slow ") {
                        "HTTP/1.1 200 OK\r\nContent-Type: image/png\r\nContent-Length: 10\r\nConnection: close\r\n\r\nfirst".to_owned()
                    } else {
                        "HTTP/1.1 200 OK\r\nContent-Type: image/png; charset=binary\r\nContent-Length: 5\r\nConnection: close\r\n\r\nimage".to_owned()
                    };
                    let _ = socket.write_all(response.as_bytes()).await;
                    if request.contains("/slow ") {
                        tokio::time::sleep(Duration::from_secs(5)).await;
                    }
                }
            });
            Self { base, calls, task }
        }
        fn url(&self, path: &str) -> String {
            format!("{}{path}", self.base)
        }
    }
    impl Drop for Server {
        fn drop(&mut self) {
            self.task.abort();
        }
    }

    #[tokio::test]
    async fn caches_local_image_and_hits_without_another_request_after_reopen() {
        let root = TestRoot::new();
        let server = Server::new().await;
        let url = server.url("/image");
        let mut cache = root.cache(8, 20);
        let first = cache.cache(65, &url).await.unwrap();
        assert_eq!(fs::read(&first).unwrap(), b"image");
        assert_eq!(first.extension().unwrap(), "png");
        assert_eq!(cache.cached_path(65, &url), Some(first.clone()));
        assert_eq!(cache.cache(65, &url).await.unwrap(), first);
        let mut reopened = root.cache(8, 20);
        assert_eq!(reopened.cache(65, &url).await.unwrap(), first);
        assert_eq!(server.calls.load(Ordering::SeqCst), 1);
        assert_eq!(reopened.disk_bytes().unwrap(), 5);
    }

    #[tokio::test]
    async fn replaces_changed_url_and_redownloads_deleted_file() {
        let root = TestRoot::new();
        let server = Server::new().await;
        let mut cache = root.cache(8, 20);
        let first = cache.cache(65, &server.url("/first")).await.unwrap();
        let url = server.url("/second");
        assert!(cache.cached_path(65, &url).is_none());
        let second = cache.cache(65, &url).await.unwrap();
        assert_ne!(first, second);
        assert!(!first.exists());
        fs::remove_file(&second).unwrap();
        assert!(cache.cached_path(65, &url).is_none());
        let third = cache.cache(65, &url).await.unwrap();
        assert_ne!(second, third);
        assert_eq!(server.calls.load(Ordering::SeqCst), 3);
        assert_eq!(cache.disk_bytes().unwrap(), 5);
    }

    #[tokio::test]
    async fn failed_url_change_preserves_old_image_and_cleans_partial_download() {
        let root = TestRoot::new();
        let server = Server::new().await;
        let mut cache = root.cache(8, 20);
        let old_url = server.url("/image");
        let old = cache.cache(65, &old_url).await.unwrap();
        for (path, expected) in [
            ("/404", "http"),
            ("/text", "type"),
            ("/chunked", "size"),
            ("/broken", "transport"),
        ] {
            let url = server.url(path);
            let error = cache.cache(65, &url).await.unwrap_err();
            match expected {
                "http" => assert!(matches!(error, ImageCacheError::HttpStatus { status: 404 })),
                "type" => assert!(matches!(error, ImageCacheError::UnsupportedContentType)),
                "size" => assert!(matches!(error, ImageCacheError::ImageTooLarge { limit: 8 })),
                _ => {}
            }
            assert_eq!(cache.cached_path(65, &old_url), Some(old.clone()));
            assert!(cache.cached_path(65, &url).is_none());
            assert_eq!(cache.disk_bytes().unwrap(), 5);
            assert_eq!(fs::read_dir(&root.0).unwrap().count(), 2);
        }
    }

    #[tokio::test]
    async fn enforces_content_length_and_streaming_total_budget_without_eviction() {
        let root = TestRoot::new();
        let server = Server::new().await;
        let url = server.url("/image");
        let mut cache = root.cache(4, 20);
        assert!(matches!(
            cache.cache(65, &url).await.unwrap_err(),
            ImageCacheError::ImageTooLarge { limit: 4 }
        ));
        assert_eq!(fs::read_dir(&root.0).unwrap().count(), 0);
        let mut cache = root.cache(20, 10);
        let old = cache.cache(65, &url).await.unwrap();
        assert!(matches!(
            cache.cache(130, &server.url("/chunked")).await.unwrap_err(),
            ImageCacheError::CacheFull { limit: 10 }
        ));
        assert_eq!(cache.cached_path(65, &url), Some(old));
        assert_eq!(cache.disk_bytes().unwrap(), 5);
        assert_eq!(fs::read_dir(&root.0).unwrap().count(), 2);
        cache.remove(65).unwrap();
        assert_eq!(cache.disk_bytes().unwrap(), 0);
        cache.cache(130, &server.url("/chunked")).await.unwrap();
        assert_eq!(cache.disk_bytes().unwrap(), 10);
    }

    #[tokio::test]
    async fn prunes_removed_products_orphans_and_temporary_files() {
        let root = TestRoot::new();
        let server = Server::new().await;
        let mut cache = root.cache(8, 20);
        let url = server.url("/image");
        let retained = cache.cache(65, &url).await.unwrap();
        let removed = cache.cache(130, &url).await.unwrap();
        fs::write(root.0.join("65-999.png"), b"orphan").unwrap();
        fs::write(root.0.join("65-1000.png.tmp"), b"partial").unwrap();
        fs::write(root.0.join("65.json.tmp"), b"partial").unwrap();
        cache.prune(&[65]).unwrap();
        assert!(retained.exists());
        assert!(!removed.exists());
        assert_eq!(fs::read_dir(&root.0).unwrap().count(), 2);
        assert_eq!(cache.disk_bytes().unwrap(), 5);
        cache.remove(65).unwrap();
        cache.remove(65).unwrap();
        assert!(cache.cached_path(65, &url).is_none());
        assert_eq!(fs::read_dir(&root.0).unwrap().count(), 0);
    }

    #[tokio::test]
    async fn rejects_non_http_urls_before_request() {
        let root = TestRoot::new();
        let mut cache = root.cache(8, 20);
        assert!(matches!(
            cache.cache(65, "file:///tmp/image.png").await.unwrap_err(),
            ImageCacheError::InvalidUrl
        ));
    }

    #[tokio::test]
    async fn cancelling_worker_removes_partial_image_and_keeps_committed_cache() {
        let root = TestRoot::new();
        let server = Server::new().await;
        let mut cache = root.cache(20, 40);
        let old_url = server.url("/image");
        let old_path = cache.cache(65, &old_url).await.unwrap();
        let new_url = server.url("/slow");
        let download_url = new_url.clone();
        let worker = tokio::spawn(async move { cache.cache(65, &download_url).await });
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if fs::read_dir(&root.0).unwrap().any(|entry| {
                    entry
                        .unwrap()
                        .path()
                        .extension()
                        .is_some_and(|extension| extension == "tmp")
                }) {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })
        .await
        .unwrap();
        worker.abort();
        assert!(worker.await.unwrap_err().is_cancelled());
        let reopened = root.cache(20, 40);
        assert_eq!(reopened.cached_path(65, &old_url), Some(old_path));
        assert!(reopened.cached_path(65, &new_url).is_none());
        assert_eq!(reopened.disk_bytes().unwrap(), 5);
        assert_eq!(fs::read_dir(&root.0).unwrap().count(), 2);
    }
}
