use std::{error::Error, fmt, future::Future, pin::Pin};

use crate::{
    config::{ConfigError, MonitorConfig},
    ricoh::ProductDetail,
    ricoh_api::{RicohApi, RicohApiError},
    scheduler::{LineId, SchedulerClient},
};

pub struct RicohSchedulerClient {
    api: RicohApi,
}

impl RicohSchedulerClient {
    pub fn new(config: &MonitorConfig) -> Result<Self, RuntimeClientError> {
        config
            .validate()
            .map_err(RuntimeClientError::InvalidConfiguration)?;
        if config.use_proxy_pool {
            return Err(RuntimeClientError::ProxyPoolUnavailable);
        }
        let api = RicohApi::with_options(
            config.requests.connect_timeout,
            config.requests.total_timeout,
            config.use_system_proxy,
        )
        .map_err(RuntimeClientError::Api)?;
        Ok(Self { api })
    }

    pub fn with_proxy(
        config: &MonitorConfig,
        proxy: reqwest::Proxy,
    ) -> Result<Self, RuntimeClientError> {
        config
            .validate()
            .map_err(RuntimeClientError::InvalidConfiguration)?;
        let api = RicohApi::with_proxy_options(
            config.requests.connect_timeout,
            config.requests.total_timeout,
            config.use_system_proxy,
            proxy,
        )
        .map_err(RuntimeClientError::Api)?;
        Ok(Self { api })
    }
}

impl SchedulerClient for RicohSchedulerClient {
    fn product_page(
        &self,
        _outlet_id: String,
        page: u32,
        limit: u32,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<ProductDetail>, RicohApiError>> + Send + 'static>>
    {
        let api = self.api.clone();
        Box::pin(async move { api.product_page(page, limit).await })
    }

    fn product_detail(
        &self,
        line: LineId,
    ) -> Pin<Box<dyn Future<Output = Result<ProductDetail, RicohApiError>> + Send + 'static>> {
        let api = self.api.clone();
        Box::pin(async move { api.product_detail(line.product_id).await })
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum RuntimeClientError {
    InvalidConfiguration(ConfigError),
    Api(RicohApiError),
    ProxyPoolUnavailable,
}

impl fmt::Display for RuntimeClientError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidConfiguration(error) => write!(formatter, "监控配置无效：{error}"),
            Self::Api(error) => write!(formatter, "理光接口客户端初始化失败：{error}"),
            Self::ProxyPoolUnavailable => formatter.write_str("代理池网络尚未接入"),
        }
    }
}

impl Error for RuntimeClientError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_starts_direct_and_system_proxy_is_optional() {
        assert!(!MonitorConfig::default().use_system_proxy);
        assert!(RicohSchedulerClient::new(&MonitorConfig::default()).is_ok());
        let mut config = MonitorConfig::default();
        config.use_system_proxy = true;
        assert!(RicohSchedulerClient::new(&config).is_ok());
    }
}
