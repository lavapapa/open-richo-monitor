use std::{
    error::Error as _,
    fmt,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use reqwest::{header, redirect::Policy, Client, Proxy, Request};
use ring::{
    digest::{self, SHA256},
    rand::{SecureRandom, SystemRandom},
};

use crate::ricoh::{
    parse_product_list_response, parse_product_response, ProductDetail, RicohParseError,
    MAX_RESPONSE_BYTES,
};

pub const PRODUCT_DETAIL_URL: &str = "https://shop.ricn-mall.com/app-api/product/detail";
pub const PRODUCT_LIST_URL: &str = "https://shop.ricn-mall.com/app-api/product/products";
pub const MAX_PRODUCT_PAGE: u32 = 51;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(8);
const MAX_RETRY_AFTER_CHARS: usize = 128;
const SIGNING_SECRET: &str = "miniapp-product-sign";
const SIGNING_PATH: &str = "/product/detail";

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RetryAfter {
    Delay(Duration),
    At(SystemTime),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RicohApiError {
    InvalidProductId,
    InvalidConfiguration,
    Transport(String),
    Unauthorized,
    Forbidden,
    RateLimited { retry_after: Option<RetryAfter> },
    RedirectBlocked { status: u16 },
    HttpStatus { status: u16 },
    ResponseTooLarge,
    NonceGeneration,
    Parse(RicohParseError),
}

impl From<reqwest::Error> for RicohApiError {
    fn from(error: reqwest::Error) -> Self {
        Self::Transport(transport_source(error))
    }
}

fn transport_source(error: reqwest::Error) -> String {
    let error = error.without_url();
    let mut diagnostic = error.to_string();
    let mut cause = error.source();
    while let Some(inner) = cause {
        diagnostic.push_str(": ");
        diagnostic.push_str(&inner.to_string());
        cause = inner.source();
    }
    diagnostic
}

impl fmt::Display for RicohApiError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidProductId => formatter.write_str("Product ID 必须是正整数"),
            Self::InvalidConfiguration => formatter.write_str("理光接口配置无效"),
            Self::Transport(diagnostic) => {
                write!(formatter, "理光接口连接失败或请求超时：{diagnostic}")
            }
            Self::Unauthorized => formatter.write_str("理光接口要求身份验证"),
            Self::Forbidden => formatter.write_str("理光接口拒绝访问"),
            Self::RateLimited { .. } => formatter.write_str("理光接口暂时限制请求"),
            Self::RedirectBlocked { status } => {
                write!(formatter, "理光接口返回了被禁止的跳转（HTTP {status}）")
            }
            Self::HttpStatus { status } => write!(formatter, "理光接口返回 HTTP {status}"),
            Self::ResponseTooLarge => formatter.write_str("理光接口响应超过 1 MiB 上限"),
            Self::NonceGeneration => formatter.write_str("无法生成理光接口请求随机数"),
            Self::Parse(error) => write!(formatter, "理光接口响应无法识别：{error}"),
        }
    }
}

impl std::error::Error for RicohApiError {}

#[derive(Clone)]
pub struct RicohApi {
    endpoint: String,
    client: Client,
    network_mode: &'static str,
    connect_timeout: Duration,
    request_timeout: Duration,
    #[cfg(target_os = "windows")]
    system_proxy: crate::system_proxy::windows::HttpProxy,
}

impl RicohApi {
    pub fn new() -> Result<Self, RicohApiError> {
        Self::build(PRODUCT_DETAIL_URL, false)
    }

    /// 创建使用指定超时和网络代理模式的生产客户端。
    pub fn with_options(
        connect_timeout: Duration,
        request_timeout: Duration,
        use_system_proxy: bool,
    ) -> Result<Self, RicohApiError> {
        Self::build_with_timeouts(
            PRODUCT_DETAIL_URL,
            use_system_proxy,
            connect_timeout,
            request_timeout,
        )
    }

    pub fn with_proxy_options(
        connect_timeout: Duration,
        request_timeout: Duration,
        use_system_proxy: bool,
        proxy: Proxy,
    ) -> Result<Self, RicohApiError> {
        Self::build_with_timeouts_and_proxy(
            PRODUCT_DETAIL_URL,
            use_system_proxy,
            connect_timeout,
            request_timeout,
            Some(proxy),
        )
    }

    fn build(endpoint: &str, use_system_proxy: bool) -> Result<Self, RicohApiError> {
        Self::build_with_timeouts(endpoint, use_system_proxy, CONNECT_TIMEOUT, REQUEST_TIMEOUT)
    }

    fn build_with_timeouts(
        endpoint: &str,
        use_system_proxy: bool,
        connect_timeout: Duration,
        request_timeout: Duration,
    ) -> Result<Self, RicohApiError> {
        Self::build_with_timeouts_and_proxy(
            endpoint,
            use_system_proxy,
            connect_timeout,
            request_timeout,
            None,
        )
    }

    fn build_with_timeouts_and_proxy(
        endpoint: &str,
        use_system_proxy: bool,
        connect_timeout: Duration,
        request_timeout: Duration,
        proxy: Option<Proxy>,
    ) -> Result<Self, RicohApiError> {
        let mut builder = Client::builder()
            .connect_timeout(connect_timeout)
            .timeout(request_timeout)
            .redirect(Policy::none())
            .retry(reqwest::retry::never())
            .user_agent(concat!(
                env!("CARGO_PKG_NAME"),
                "/",
                env!("CARGO_PKG_VERSION")
            ));
        let network_mode = if proxy.is_some() {
            "显式代理"
        } else if use_system_proxy {
            "系统代理（按操作系统或环境配置）"
        } else {
            "直连"
        };
        #[cfg(target_os = "windows")]
        let system_proxy =
            crate::system_proxy::windows::HttpProxy::new(use_system_proxy && proxy.is_none());
        if !use_system_proxy || cfg!(target_os = "windows") {
            builder = builder.no_proxy();
        }
        if let Some(proxy) = proxy {
            builder = builder.proxy(proxy);
        }
        let client = builder
            .build()
            .map_err(|_| RicohApiError::InvalidConfiguration)?;
        Ok(Self {
            endpoint: endpoint.to_owned(),
            client,
            network_mode,
            connect_timeout,
            request_timeout,
            #[cfg(target_os = "windows")]
            system_proxy,
        })
    }

    pub async fn product_detail(&self, product_id: u64) -> Result<ProductDetail, RicohApiError> {
        if product_id == 0 {
            return Err(RicohApiError::InvalidProductId);
        }

        let request = self.build_product_detail_request(product_id)?;
        let (_, body) = self.execute_bounded(request).await?;
        parse_product_response(product_id, &body).map_err(RicohApiError::Parse)
    }

    pub async fn product_page(
        &self,
        page: u32,
        limit: u32,
    ) -> Result<Vec<ProductDetail>, RicohApiError> {
        let request = self.build_product_page_request(page, limit)?;
        let (_, body) = self.execute_bounded(request).await?;
        parse_product_list_response(&body).map_err(RicohApiError::Parse)
    }

    async fn execute_bounded(&self, request: Request) -> Result<(u16, Vec<u8>), RicohApiError> {
        let started_at = Instant::now();
        #[cfg(target_os = "windows")]
        let client = self
            .system_proxy
            .client(
                request.url().as_str(),
                &self.client,
                |proxy| {
                    let proxy =
                        Proxy::all(proxy).map_err(|_| "Windows 系统代理地址无效".to_owned())?;
                    Self::build_with_timeouts_and_proxy(
                        &self.endpoint,
                        false,
                        self.connect_timeout,
                        self.request_timeout,
                        Some(proxy),
                    )
                    .map(|api| api.client)
                    .map_err(|_| "无法应用 Windows 系统代理".to_owned())
                },
                self.request_timeout,
            )
            .await
            .map_err(RicohApiError::Transport)?;
        #[cfg(target_os = "windows")]
        let request = {
            let mut request = request;
            *request.timeout_mut() =
                Some(self.request_timeout.saturating_sub(started_at.elapsed()));
            request
        };
        #[cfg(not(target_os = "windows"))]
        let client = &self.client;
        let mut response = client
            .execute(request)
            .await
            .map_err(|error| self.transport_error(error, "发送请求", started_at))?;
        let status = response.status();

        if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
            let retry_after = parse_retry_after(response.headers().get(header::RETRY_AFTER));
            return Err(RicohApiError::RateLimited { retry_after });
        }
        if status == reqwest::StatusCode::UNAUTHORIZED {
            return Err(RicohApiError::Unauthorized);
        }
        if status == reqwest::StatusCode::FORBIDDEN {
            return Err(RicohApiError::Forbidden);
        }
        if status.is_redirection() {
            return Err(RicohApiError::RedirectBlocked {
                status: status.as_u16(),
            });
        }
        if !status.is_success() {
            return Err(RicohApiError::HttpStatus {
                status: status.as_u16(),
            });
        }

        if response
            .content_length()
            .is_some_and(|length| length > MAX_RESPONSE_BYTES as u64)
        {
            return Err(RicohApiError::ResponseTooLarge);
        }
        let mut body = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|error| self.transport_error(error, "读取响应", started_at))?
        {
            if body.len() + chunk.len() > MAX_RESPONSE_BYTES {
                return Err(RicohApiError::ResponseTooLarge);
            }
            body.extend_from_slice(&chunk);
        }

        Ok((status.as_u16(), body))
    }

    fn transport_error(
        &self,
        error: reqwest::Error,
        phase: &str,
        started_at: Instant,
    ) -> RicohApiError {
        let phase = if error.is_connect() {
            "建立连接（DNS／代理隧道／TLS）"
        } else {
            phase
        };
        // 保留请求库的原始错误链，并附上本次请求的实际配置，界面直接呈现这些事实。
        let source = transport_source(error);
        RicohApiError::Transport(format!(
            "网络：{}；阶段：{}；耗时：{} ms；连接上限：{} ms；请求上限：{} ms\n{}",
            self.network_mode,
            phase,
            started_at.elapsed().as_millis(),
            self.connect_timeout.as_millis(),
            self.request_timeout.as_millis(),
            source,
        ))
    }

    fn build_product_detail_request(&self, product_id: u64) -> Result<Request, RicohApiError> {
        if product_id == 0 {
            return Err(RicohApiError::InvalidProductId);
        }
        self.build_signed_request(
            &self.endpoint,
            SIGNING_PATH,
            &format!("productId={product_id}&skuId="),
        )
    }

    fn build_product_page_request(&self, page: u32, limit: u32) -> Result<Request, RicohApiError> {
        if !(1..=MAX_PRODUCT_PAGE).contains(&page) || !(1..=20).contains(&limit) {
            return Err(RicohApiError::InvalidConfiguration);
        }
        let mut endpoint =
            reqwest::Url::parse(&self.endpoint).map_err(|_| RicohApiError::InvalidConfiguration)?;
        let path = endpoint
            .path()
            .strip_suffix("/detail")
            .map(|prefix| format!("{prefix}/products"))
            .ok_or(RicohApiError::InvalidConfiguration)?;
        endpoint.set_path(&path);
        self.build_signed_request(
            endpoint.as_str(),
            "/product/products",
            &format!("couponId=&keyword=&limit={limit}&news=&page={page}&sid=&type="),
        )
    }

    fn build_signed_request(
        &self,
        endpoint: &str,
        path: &str,
        query: &str,
    ) -> Result<Request, RicohApiError> {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| RicohApiError::InvalidConfiguration)?
            .as_millis()
            .to_string();
        let mut random_suffix = [0; 6];
        SystemRandom::new()
            .fill(&mut random_suffix)
            .map_err(|_| RicohApiError::NonceGeneration)?;
        let nonce = format!("{timestamp}{}", hex(&random_suffix));
        let signature = sign_request(path, query, &timestamp, &nonce);
        let url = format!("{endpoint}?{query}");
        self.client
            .get(url)
            .header(header::ACCEPT, "application/json")
            .header("X-Request-Timestamp", timestamp)
            .header("X-Request-Nonce", nonce)
            .header("X-Request-Sign", signature)
            .build()
            .map_err(|_| RicohApiError::InvalidConfiguration)
    }

    #[cfg(test)]
    fn for_test(endpoint: &str) -> Self {
        Self::build(endpoint, false).expect("本地测试接口配置应有效")
    }

    #[cfg(test)]
    fn for_test_with_timeouts(
        endpoint: &str,
        connect_timeout: Duration,
        request_timeout: Duration,
    ) -> Self {
        Self::build_with_timeouts(endpoint, false, connect_timeout, request_timeout)
            .expect("本地测试接口配置应有效")
    }
}

#[cfg(test)]
fn sign_product_detail(query: &str, timestamp: &str, nonce: &str) -> String {
    sign_request(SIGNING_PATH, query, timestamp, nonce)
}

fn sign_request(path: &str, query: &str, timestamp: &str, nonce: &str) -> String {
    let canonical = format!("GET\n{path}\n{query}\n\n{timestamp}\n{nonce}");
    let digest = digest::digest(&SHA256, format!("{SIGNING_SECRET}\n{canonical}").as_bytes());
    hex(digest.as_ref())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn parse_retry_after(value: Option<&header::HeaderValue>) -> Option<RetryAfter> {
    let value = value?.to_str().ok()?.trim();
    if value.is_empty() || value.len() > MAX_RETRY_AFTER_CHARS {
        return None;
    }

    if value.bytes().all(|byte| byte.is_ascii_digit()) {
        return value
            .parse::<u64>()
            .ok()
            .map(|seconds| RetryAfter::Delay(Duration::from_secs(seconds)));
    }

    httpdate::parse_http_date(value).ok().map(RetryAfter::At)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::availability::Availability;
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::mpsc::{self, Receiver};
    use std::thread;

    const TEST_IO_TIMEOUT: Duration = Duration::from_secs(2);
    const TEST_WAIT_TIMEOUT: Duration = Duration::from_secs(4);

    #[tokio::test]
    #[ignore = "需明确授权：公开接口 6 次串行证据采样，间隔 2 秒，403/429 即停"]
    async fn live_list_product_evidence() {
        let directory = std::env::var_os("RICOH_LIST_EVIDENCE_DIR")
            .map(std::path::PathBuf::from)
            .expect("需要指定列表接口证据保存目录");
        std::fs::create_dir_all(&directory).unwrap();
        let mut log = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(directory.join("probe-direct-stdout.txt"))
            .unwrap();
        let api = RicohApi::with_options(CONNECT_TIMEOUT, REQUEST_TIMEOUT, false).unwrap();
        let samples = [
            (Some((1, 20)), None),
            (Some((2, 20)), None),
            (Some((1, 10)), None),
            (Some((2, 10)), None),
            (None, Some(108)),
            (None, Some(65)),
        ];
        for (index, (page, id)) in samples.into_iter().enumerate() {
            if index > 0 {
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
            let (label, request) = if let Some((page, limit)) = page {
                (
                    format!("probe-direct-list-limit{limit}-page{page}"),
                    api.build_product_page_request(page, limit).unwrap(),
                )
            } else {
                let id = id.unwrap();
                (
                    format!("probe-direct-detail-{id}"),
                    api.build_product_detail_request(id).unwrap(),
                )
            };
            let started = Instant::now();
            let (status, body) = match api.execute_bounded(request).await {
                Ok(response) => response,
                Err(error) => {
                    let line = format!(
                        "request={} {label}; elapsed_ms={}; stopped={error}",
                        index + 1,
                        started.elapsed().as_millis()
                    );
                    println!("{line}");
                    writeln!(log, "{line}").unwrap();
                    break;
                }
            };
            let products = if page.is_some() {
                parse_product_list_response(&body).unwrap()
            } else {
                vec![parse_product_response(id.unwrap(), &body).unwrap()]
            };
            let line = format!(
                "request={} {label}; HTTP={status}; elapsed_ms={}; bytes={}; products={:?}",
                index + 1,
                started.elapsed().as_millis(),
                body.len(),
                products
                    .iter()
                    .map(|p| (
                        p.product_id,
                        p.is_show,
                        p.stock.to_string(),
                        p.metadata.price.as_deref()
                    ))
                    .collect::<Vec<_>>()
            );
            println!("{line}");
            writeln!(log, "{line}").unwrap();
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(directory.join(format!("{label}.json")))
                .unwrap();
            file.write_all(&body).unwrap();
        }
    }

    #[test]
    fn list_request_has_exact_query_and_product_list_signature() {
        let api = RicohApi::for_test("https://shop.ricn-mall.com/app-api/product/detail");
        let request = api.build_product_page_request(51, 20).unwrap();
        let query = "couponId=&keyword=&limit=20&news=&page=51&sid=&type=";
        assert_eq!(request.url().path(), "/app-api/product/products");
        assert_eq!(request.url().query(), Some(query));
        assert_eq!(request.headers().len(), 4);
        assert_eq!(request.headers()[header::ACCEPT], "application/json");
        let timestamp = request.headers()["x-request-timestamp"].to_str().unwrap();
        let nonce = request.headers()["x-request-nonce"].to_str().unwrap();
        assert_eq!(
            request.headers()["x-request-sign"],
            sign_request("/product/products", query, timestamp, nonce)
        );
        assert_ne!(
            request.headers()["x-request-sign"],
            sign_product_detail(query, timestamp, nonce)
        );
        for (page, limit) in [(0, 10), (52, 10), (1, 0), (1, 21)] {
            assert!(api.build_product_page_request(page, limit).is_err());
        }
    }

    #[tokio::test]
    async fn list_local_empty_and_flat_product_responses_are_valid() {
        for body in [br#"{"code":0,"data":[]}"#.as_slice(), br#"{"code":0,"data":[{"id":108,"storeName":"VIP","isShow":1,"stock":0,"price":199.00}]}"#] {
            let (endpoint, received, worker) = serve_once(http_response(200, "Content-Type: application/json\r\n", body));
            let api = RicohApi::for_test(&format!("{endpoint}/app-api/product/detail"));
            let products = api.product_page(1,10).await.unwrap();
            if let Some(product) = products.first() { assert_eq!(product.metadata.price.as_deref(), Some("199.00")); }
            let request = String::from_utf8(receive(&received)).unwrap();
            assert!(request.starts_with("GET /app-api/product/products?couponId=&keyword=&limit=10&news=&page=1&sid=&type= HTTP/1.1"));
            assert!(!request.to_ascii_lowercase().contains("authorization:"));
            assert!(!request.to_ascii_lowercase().contains("cookie:"));
            worker.join().unwrap();
        }
    }

    #[tokio::test]
    async fn list_local_http_and_business_failures_are_not_empty_successes() {
        for (status, headers, body, expected) in [
            (403, "", b"denied".as_slice(), RicohApiError::Forbidden),
            (
                429,
                "Retry-After: 2\r\n",
                b"limited",
                RicohApiError::RateLimited {
                    retry_after: Some(RetryAfter::Delay(Duration::from_secs(2))),
                },
            ),
            (
                302,
                "Location: http://127.0.0.1/other\r\n",
                b"",
                RicohApiError::RedirectBlocked { status: 302 },
            ),
            (
                200,
                "",
                br#"{"code":7001,"data":null}"#,
                RicohApiError::Parse(RicohParseError::BusinessCode { code: 7001 }),
            ),
        ] {
            let (endpoint, received, worker) = serve_once(http_response(status, headers, body));
            let api = RicohApi::for_test(&format!("{endpoint}/app-api/product/detail"));
            assert_eq!(api.product_page(1, 10).await.unwrap_err(), expected);
            receive(&received);
            worker.join().unwrap();
        }
    }

    #[tokio::test]
    async fn list_local_oversized_response_is_rejected() {
        let (endpoint, received, worker) =
            serve_once(http_response(200, "", &vec![b' '; MAX_RESPONSE_BYTES + 1]));
        let api = RicohApi::for_test(&format!("{endpoint}/app-api/product/detail"));
        assert_eq!(
            api.product_page(1, 20).await.unwrap_err(),
            RicohApiError::ResponseTooLarge
        );
        receive(&received);
        worker.join().unwrap();
    }

    #[tokio::test]
    #[ignore = "手动读取公开商品详情，使用生产客户端签名，不操作用户配置"]
    async fn live_product_metadata_evidence() {
        let api = RicohApi::with_options(CONNECT_TIMEOUT, REQUEST_TIMEOUT, true).unwrap();
        let directory = std::env::var_os("RICOH_DETAIL_EVIDENCE_DIR")
            .map(std::path::PathBuf::from)
            .expect("需要指定接口证据保存目录");
        std::fs::create_dir_all(&directory).unwrap();
        for id in [65, 130, 108, 38] {
            let request = api.build_product_detail_request(id).unwrap();
            let response = api.client.execute(request).await.unwrap();
            let status = response.status();
            let body = response.bytes().await.unwrap();
            assert!(body.len() <= MAX_RESPONSE_BYTES);
            let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
            println!("Product {id}: HTTP {status}; code={}", json["code"]);
            assert_eq!(json["code"], 0);
            std::fs::write(
                directory.join(format!("product-{id}.json")),
                serde_json::to_vec_pretty(&json).unwrap(),
            )
            .unwrap();
            println!(
                "storeInfo keys: {:?}",
                json["data"]["storeInfo"]
                    .as_object()
                    .unwrap()
                    .keys()
                    .collect::<Vec<_>>()
            );
            println!(
                "data keys: {:?}",
                json["data"].as_object().unwrap().keys().collect::<Vec<_>>()
            );
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    }

    #[test]
    fn signature_matches_the_legacy_canonical_request() {
        assert_eq!(
            sign_product_detail(
                "productId=245&skuId=",
                "1724333333333",
                "1724333333333abcdef012345",
            ),
            "ef53b0842dd92b142e5b296389a0eeea7773f1563d434a1c95d20ff9146d161a"
        );
    }

    fn accept_with_timeout(listener: &TcpListener) -> std::io::Result<TcpStream> {
        listener.set_nonblocking(true)?;
        let deadline = std::time::Instant::now() + TEST_IO_TIMEOUT;
        loop {
            match listener.accept() {
                Ok((stream, _)) => {
                    stream.set_nonblocking(false)?;
                    stream.set_read_timeout(Some(TEST_IO_TIMEOUT))?;
                    stream.set_write_timeout(Some(TEST_IO_TIMEOUT))?;
                    return Ok(stream);
                }
                Err(error)
                    if error.kind() == std::io::ErrorKind::WouldBlock
                        && std::time::Instant::now() < deadline =>
                {
                    thread::sleep(Duration::from_millis(5));
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        "本地测试服务等待请求超时",
                    ));
                }
                Err(error) => return Err(error),
            }
        }
    }

    fn receive<T>(receiver: &Receiver<T>) -> T {
        receiver
            .recv_timeout(TEST_WAIT_TIMEOUT)
            .expect("本地测试服务未在期限内返回")
    }

    fn serve_once(response: Vec<u8>) -> (String, Receiver<Vec<u8>>, thread::JoinHandle<()>) {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let address = listener.local_addr().unwrap();
        let (sender, receiver) = mpsc::channel();
        let worker = thread::spawn(move || {
            let mut stream = accept_with_timeout(&listener).unwrap();
            let mut request = Vec::new();
            let mut buffer = [0_u8; 1024];
            loop {
                let count = stream.read(&mut buffer).unwrap();
                if count == 0 {
                    break;
                }
                request.extend_from_slice(&buffer[..count]);
                if request.windows(4).any(|window| window == b"\r\n\r\n") {
                    break;
                }
            }
            let _ = sender.send(request);
            let _ = stream.write_all(&response);
            let _ = stream.flush();
        });
        (format!("http://{address}"), receiver, worker)
    }

    fn serve_headers_until_body_release() -> (
        String,
        Receiver<Vec<u8>>,
        mpsc::Sender<()>,
        Receiver<()>,
        thread::JoinHandle<()>,
    ) {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let address = listener.local_addr().unwrap();
        let (request_sender, request_receiver) = mpsc::channel();
        let (release_sender, release_receiver) = mpsc::channel();
        let (body_sender, body_receiver) = mpsc::channel();
        let worker = thread::spawn(move || {
            let mut stream = accept_with_timeout(&listener).unwrap();
            let mut request = Vec::new();
            let mut buffer = [0_u8; 1024];
            loop {
                let count = stream.read(&mut buffer).unwrap();
                if count == 0 {
                    return;
                }
                request.extend_from_slice(&buffer[..count]);
                if request.windows(4).any(|window| window == b"\r\n\r\n") {
                    break;
                }
            }
            request_sender.send(request).unwrap();
            if stream
                .write_all(
                    b"HTTP/1.1 429 Too Many Requests\r\nRetry-After: 60\r\nContent-Length: 26\r\nConnection: close\r\n\r\n",
                )
                .is_err()
            {
                return;
            }
            if release_receiver.recv_timeout(TEST_WAIT_TIMEOUT).is_err() {
                return;
            }
            let _ = stream.write_all(b"private rate-limit body!!!");
            let _ = body_sender.send(());
        });
        (
            format!("http://{address}"),
            request_receiver,
            release_sender,
            body_receiver,
            worker,
        )
    }

    fn http_response(status: u16, headers: &str, body: &[u8]) -> Vec<u8> {
        let mut response = format!(
            "HTTP/1.1 {status} Test\r\n{headers}Content-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        )
        .into_bytes();
        response.extend_from_slice(body);
        response
    }

    #[test]
    fn request_has_fixed_path_query_and_signed_anonymous_headers() {
        let api = RicohApi::for_test("https://shop.ricn-mall.com/app-api/product/detail");
        let request = api.build_product_detail_request(245).unwrap();
        let url = request.url();

        assert_eq!(request.method(), reqwest::Method::GET);
        assert_eq!(url.path(), "/app-api/product/detail");
        assert_eq!(
            url.query_pairs().collect::<Vec<_>>(),
            vec![
                ("productId".into(), "245".into()),
                ("skuId".into(), "".into())
            ]
        );
        assert_eq!(
            request.headers().get(reqwest::header::ACCEPT).unwrap(),
            "application/json"
        );
        assert_eq!(request.headers().len(), 4);
        let timestamp = request
            .headers()
            .get("x-request-timestamp")
            .unwrap()
            .to_str()
            .unwrap();
        let nonce = request
            .headers()
            .get("x-request-nonce")
            .unwrap()
            .to_str()
            .unwrap();
        let signature = request
            .headers()
            .get("x-request-sign")
            .unwrap()
            .to_str()
            .unwrap();
        assert!(nonce.starts_with(timestamp));
        assert_eq!(nonce.len(), timestamp.len() + 12);
        assert_eq!(signature.len(), 64);
        for forbidden in ["cookie", "authorization", "referer"] {
            assert!(
                request.headers().get(forbidden).is_none(),
                "不应包含 {forbidden}"
            );
        }
    }

    #[tokio::test]
    async fn local_json_response_is_parsed_with_listing_and_stock_and_request_is_sent_once() {
        let body = r#"{"code":0,"data":{"storeInfo":{"storeName":"样例机身","id":245,"isShow":1,"stock":2}}}"#.as_bytes();
        let response = http_response(200, "Content-Type: application/json\r\n", body);
        let (endpoint, received, worker) = serve_once(response);
        let api = RicohApi::for_test(&format!("{endpoint}/app-api/product/detail"));

        let product = api.product_detail(245).await.unwrap();
        assert_eq!(product.product_id, 245);
        assert_eq!(product.is_show, 1);
        assert_eq!(product.stock.as_i64(), Some(2));
        assert_eq!(product.availability, Availability::InStock);

        let request = receive(&received);
        let request_text = String::from_utf8_lossy(&request);
        assert!(request_text.starts_with("GET /app-api/product/detail?"));
        let header_names: Vec<_> = request_text
            .lines()
            .skip(1)
            .take_while(|line| !line.is_empty())
            .filter_map(|line| {
                line.split_once(':')
                    .map(|(name, _)| name.to_ascii_lowercase())
            })
            .collect();
        for header_name in &header_names {
            assert!(
                [
                    "host",
                    "accept",
                    "user-agent",
                    "x-request-timestamp",
                    "x-request-nonce",
                    "x-request-sign",
                    "accept-encoding",
                    "connection"
                ]
                .contains(&header_name.as_str()),
                "意外请求头：{header_name}"
            );
        }
        assert!(request_text
            .lines()
            .any(|line| line.eq_ignore_ascii_case(&format!(
                "User-Agent: {}/{}",
                env!("CARGO_PKG_NAME"),
                env!("CARGO_PKG_VERSION")
            ))));
        for header in [
            "x-request-timestamp:",
            "x-request-nonce:",
            "x-request-sign:",
        ] {
            assert!(request_text
                .lines()
                .any(|line| line.to_ascii_lowercase().starts_with(header)));
        }
        assert!(!request_text.to_ascii_lowercase().contains("cookie:"));
        assert!(!request_text.to_ascii_lowercase().contains("authorization:"));
        worker.join().unwrap();
    }

    #[tokio::test]
    async fn redirect_is_reported_without_following_it() {
        let response = http_response(
            302,
            "Location: http://127.0.0.1/other-target\r\n",
            b"redirect body is not retained",
        );
        let (endpoint, received, worker) = serve_once(response);
        let api = RicohApi::for_test(&format!("{endpoint}/app-api/product/detail"));

        assert_eq!(
            api.product_detail(245).await.unwrap_err(),
            RicohApiError::RedirectBlocked { status: 302 }
        );
        receive(&received);
        worker.join().unwrap();
    }

    #[tokio::test]
    async fn rate_limit_parses_delta_seconds_without_reading_a_body() {
        let response = http_response(429, "Retry-After: 60\r\n", b"do not persist error body");
        let (endpoint, received, worker) = serve_once(response);
        let api = RicohApi::for_test(&format!("{endpoint}/app-api/product/detail"));

        assert_eq!(
            api.product_detail(245).await.unwrap_err(),
            RicohApiError::RateLimited {
                retry_after: Some(RetryAfter::Delay(Duration::from_secs(60)))
            }
        );
        receive(&received);
        worker.join().unwrap();
    }

    #[tokio::test]
    async fn rate_limit_returns_before_the_error_body_is_sent() {
        let (endpoint, received, release_body, body_sent, worker) =
            serve_headers_until_body_release();
        let api = RicohApi::for_test(&format!("{endpoint}/app-api/product/detail"));

        let result = tokio::time::timeout(TEST_WAIT_TIMEOUT, api.product_detail(245))
            .await
            .expect("429 应在释放正文前返回")
            .unwrap_err();
        assert_eq!(
            result,
            RicohApiError::RateLimited {
                retry_after: Some(RetryAfter::Delay(Duration::from_secs(60)))
            }
        );
        receive(&received);
        release_body.send(()).unwrap();
        receive(&body_sent);
        worker.join().unwrap();
    }

    #[test]
    fn retry_after_accepts_http_date_and_rejects_malformed_values() {
        let value = header::HeaderValue::from_static("Wed, 21 Oct 2015 07:28:00 GMT");
        assert_eq!(
            parse_retry_after(Some(&value)),
            Some(RetryAfter::At(
                httpdate::parse_http_date("Wed, 21 Oct 2015 07:28:00 GMT").unwrap()
            ))
        );
        for invalid in ["", "soon", "-1", "999999999999999999999999999999999999999"] {
            let value = header::HeaderValue::from_str(invalid).ok();
            assert_eq!(
                parse_retry_after(value.as_ref()),
                None,
                "应拒绝 {invalid:?}"
            );
        }
        let too_long =
            header::HeaderValue::from_bytes(&vec![b'1'; MAX_RETRY_AFTER_CHARS + 1]).unwrap();
        assert_eq!(parse_retry_after(Some(&too_long)), None);
    }

    #[tokio::test]
    async fn accepts_a_response_exactly_one_mibibyte() {
        let prefix = r#"{"code":0,"data":{"storeInfo":{"storeName":"样例机身","id":245,"isShow":1,"stock":2}}}"#;
        let mut body = prefix.as_bytes().to_vec();
        body.resize(MAX_RESPONSE_BYTES, b' ');
        let response = http_response(200, "Content-Type: application/json\r\n", &body);
        let (endpoint, received, worker) = serve_once(response);
        let api = RicohApi::for_test(&format!("{endpoint}/app-api/product/detail"));

        let product = api.product_detail(245).await.unwrap();
        assert_eq!(product.is_show, 1);
        assert_eq!(product.stock.as_i64(), Some(2));
        receive(&received);
        worker.join().unwrap();
    }

    #[tokio::test]
    async fn rejects_chunked_response_above_one_mibibyte() {
        let payload = vec![b'a'; MAX_RESPONSE_BYTES + 1];
        let mut response = format!(
            "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n{:X}\r\n",
            payload.len()
        )
        .into_bytes();
        response.extend_from_slice(&payload);
        response.extend_from_slice(b"\r\n0\r\n\r\n");
        let (endpoint, received, worker) = serve_once(response);
        let api = RicohApi::for_test(&format!("{endpoint}/app-api/product/detail"));

        assert_eq!(
            api.product_detail(245).await.unwrap_err(),
            RicohApiError::ResponseTooLarge
        );
        receive(&received);
        worker.join().unwrap();
    }

    #[tokio::test]
    async fn rejects_unknown_length_response_above_one_mibibyte() {
        let payload = vec![b'a'; MAX_RESPONSE_BYTES + 1];
        let mut response = b"HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n".to_vec();
        response.extend_from_slice(&payload);
        let (endpoint, received, worker) = serve_once(response);
        let api = RicohApi::for_test(&format!("{endpoint}/app-api/product/detail"));

        assert_eq!(
            api.product_detail(245).await.unwrap_err(),
            RicohApiError::ResponseTooLarge
        );
        receive(&received);
        worker.join().unwrap();
    }

    #[tokio::test]
    async fn wraps_http_response_shape_errors_as_parse_errors() {
        let response = http_response(
            200,
            "Content-Type: application/json\r\n",
            r#"{"code":0,"data":{"storeInfo":{"storeName":"样例机身","id":245,"isShow":1}}}"#
                .as_bytes(),
        );
        let (endpoint, received, worker) = serve_once(response);
        let api = RicohApi::for_test(&format!("{endpoint}/app-api/product/detail"));

        assert_eq!(
            api.product_detail(245).await.unwrap_err(),
            RicohApiError::Parse(RicohParseError::MissingField("data.storeInfo.stock"))
        );
        receive(&received);
        worker.join().unwrap();
    }

    #[tokio::test]
    async fn request_timeout_ends_a_stalled_local_request() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let (request_sender, request_receiver) = mpsc::channel();
        let worker = thread::spawn(move || {
            let mut stream = accept_with_timeout(&listener).unwrap();
            let mut request = Vec::new();
            let mut buffer = [0_u8; 1024];
            loop {
                let count = stream.read(&mut buffer).unwrap();
                if count == 0 {
                    return;
                }
                request.extend_from_slice(&buffer[..count]);
                if request.windows(4).any(|window| window == b"\r\n\r\n") {
                    break;
                }
            }
            request_sender.send(request).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_millis(250)))
                .unwrap();
            let mut byte = [0_u8; 1];
            let _ = stream.read(&mut byte);
        });
        let api = RicohApi::for_test_with_timeouts(
            &format!("{endpoint}/app-api/product/detail"),
            Duration::from_secs(1),
            Duration::from_millis(150),
        );

        let started_at = std::time::Instant::now();
        let error = api.product_detail(245).await.unwrap_err();
        let RicohApiError::Transport(diagnostic) = &error else {
            panic!("预期请求超时，实际为 {error}");
        };
        assert!(diagnostic.contains("timed out"));
        assert!(diagnostic.contains("网络：直连"));
        assert!(diagnostic.contains("阶段：发送请求"));
        assert!(diagnostic.contains("连接上限：1000 ms"));
        assert!(diagnostic.contains("请求上限：150 ms"));
        assert!(error.to_string().contains("timed out"));
        assert!(!error.to_string().contains(&endpoint));
        assert!(started_at.elapsed() < Duration::from_secs(1));
        receive(&request_receiver);
        worker.join().unwrap();
    }

    #[tokio::test]
    async fn stalled_proxy_tunnel_reports_connection_phase_and_explicit_route_without_fallback() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let proxy = Proxy::all(format!("http://{}", listener.local_addr().unwrap())).unwrap();
        let (sender, receiver) = mpsc::channel();
        let worker = thread::spawn(move || {
            let mut stream = accept_with_timeout(&listener).unwrap();
            let mut request = [0; 1024];
            let count = stream.read(&mut request).unwrap();
            sender.send(request[..count].to_vec()).unwrap();
            let _ = stream.read(&mut request);
        });
        let api = RicohApi::build_with_timeouts_and_proxy(
            "https://product.invalid/app-api/product/detail",
            false,
            Duration::from_millis(100),
            Duration::from_millis(500),
            Some(proxy),
        )
        .unwrap();
        let error = api.product_detail(65).await.unwrap_err().to_string();
        assert!(error.contains("网络：显式代理"), "{error}");
        assert!(error.contains("阶段：建立连接"), "{error}");
        assert!(error.contains("timed out"), "{error}");
        assert!(String::from_utf8(receive(&receiver))
            .unwrap()
            .starts_with("CONNECT product.invalid:443"));
        worker.join().unwrap();
    }

    #[tokio::test]
    async fn interrupted_response_body_keeps_transport_source_chain() {
        let (endpoint, received, worker) = serve_once(
            b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\nConnection: close\r\n\r\npartial".to_vec(),
        );
        let api = RicohApi::for_test(&format!("{endpoint}/app-api/product/detail"));

        let error = api.product_detail(245).await.unwrap_err();
        let RicohApiError::Transport(diagnostic) = &error else {
            panic!("预期正文读取失败，实际为 {error}");
        };
        assert!(diagnostic.contains("error decoding response body"));
        assert!(diagnostic.contains("end of file before message length reached"));
        assert!(diagnostic.contains("阶段：读取响应"));
        assert!(error.to_string().contains("理光接口连接失败或请求超时"));
        assert!(!error.to_string().contains(&endpoint));
        receive(&received);
        worker.join().unwrap();
    }

    #[tokio::test]
    async fn cancelling_while_waiting_for_headers_releases_the_request_task() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let (request_sender, request_receiver) = mpsc::channel();
        let (close_sender, close_receiver) = mpsc::channel();
        let worker = thread::spawn(move || {
            let mut stream = accept_with_timeout(&listener).unwrap();
            let mut request = Vec::new();
            let mut buffer = [0_u8; 1024];
            loop {
                let count = stream.read(&mut buffer).unwrap();
                if count == 0 {
                    return;
                }
                request.extend_from_slice(&buffer[..count]);
                if request.windows(4).any(|window| window == b"\r\n\r\n") {
                    break;
                }
            }
            request_sender.send(request).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut byte = [0_u8; 1];
            let closed = match stream.read(&mut byte) {
                Ok(0) => true,
                Ok(_) => false,
                Err(error) => error.kind() != std::io::ErrorKind::TimedOut,
            };
            let _ = close_sender.send(closed);
        });
        let api = RicohApi::for_test(&format!("{endpoint}/app-api/product/detail"));
        let request_task = tokio::spawn(async move { api.product_detail(245).await });
        tokio::task::spawn_blocking(move || receive(&request_receiver))
            .await
            .unwrap();

        request_task.abort();
        assert!(request_task.await.unwrap_err().is_cancelled());
        assert!(close_receiver.recv_timeout(TEST_WAIT_TIMEOUT).unwrap());
        worker.join().unwrap();
    }

    #[tokio::test]
    async fn transport_error_is_not_retried() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        listener.set_nonblocking(true).unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let accepted_requests = std::sync::Arc::new(AtomicUsize::new(0));
        let accepted_by_server = accepted_requests.clone();
        let (count_sender, count_receiver) = mpsc::channel();
        let worker = thread::spawn(move || {
            let deadline = std::time::Instant::now() + Duration::from_millis(750);
            while std::time::Instant::now() < deadline {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        accepted_by_server.fetch_add(1, Ordering::SeqCst);
                        let _ = stream.set_read_timeout(Some(Duration::from_millis(100)));
                        let mut request = Vec::new();
                        let mut buffer = [0_u8; 1024];
                        loop {
                            match stream.read(&mut buffer) {
                                Ok(0) | Err(_) => break,
                                Ok(count) => {
                                    request.extend_from_slice(&buffer[..count]);
                                    if request.windows(4).any(|window| window == b"\r\n\r\n") {
                                        break;
                                    }
                                }
                            }
                        }
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                    }
                    Err(_) => break,
                }
            }
            let _ = count_sender.send(accepted_by_server.load(Ordering::SeqCst));
        });
        let api = RicohApi::for_test(&format!("{endpoint}/app-api/product/detail"));

        assert!(matches!(
            api.product_detail(245).await.unwrap_err(),
            RicohApiError::Transport(_)
        ));
        assert_eq!(count_receiver.recv_timeout(TEST_WAIT_TIMEOUT).unwrap(), 1);
        worker.join().unwrap();
    }

    #[tokio::test]
    async fn rejects_declared_response_above_one_mibibyte() {
        let header = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            MAX_RESPONSE_BYTES + 1
        );
        let (endpoint, received, worker) = serve_once(header.into_bytes());
        let api = RicohApi::for_test(&format!("{endpoint}/app-api/product/detail"));

        assert_eq!(
            api.product_detail(245).await.unwrap_err(),
            RicohApiError::ResponseTooLarge
        );
        receive(&received);
        worker.join().unwrap();
    }

    #[tokio::test]
    async fn distinguishes_authorization_and_server_errors() {
        for (status, expected) in [
            (401, RicohApiError::Unauthorized),
            (403, RicohApiError::Forbidden),
            (500, RicohApiError::HttpStatus { status: 500 }),
        ] {
            let response = http_response(status, "", b"private provider response");
            let (endpoint, received, worker) = serve_once(response);
            let api = RicohApi::for_test(&format!("{endpoint}/app-api/product/detail"));
            assert_eq!(api.product_detail(245).await.unwrap_err(), expected);
            receive(&received);
            worker.join().unwrap();
        }
    }
}
