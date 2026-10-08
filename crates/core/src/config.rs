use std::{fmt, time::Duration};

use serde::{Deserialize, Serialize};

pub const BEIJING_TIME_ZONE: &str = "Asia/Shanghai";
pub const WEEKDAYS: usize = 7;

pub const fn default_auto_start_monitoring() -> bool {
    true
}

pub const fn default_notification_use_system_proxy() -> bool {
    true
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MonitoringMode {
    #[default]
    ListedProducts,
    ProductDetail,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScheduleConfig {
    /// 星期掩码按周一至周日排列，索引 0 为周一。
    pub enabled_days: [bool; WEEKDAYS],
    pub start_minute: u16,
    pub end_minute: u16,
}

impl Default for ScheduleConfig {
    fn default() -> Self {
        Self {
            enabled_days: [true; WEEKDAYS],
            start_minute: 9 * 60,
            end_minute: 19 * 60,
        }
    }
}

impl ScheduleConfig {
    /// `weekday_monday_zero` 与 `minute_of_day` 必须是北京本地墙上时间。
    pub fn contains_local_time(&self, weekday_monday_zero: u8, minute_of_day: u16) -> bool {
        if weekday_monday_zero >= WEEKDAYS as u8
            || minute_of_day >= 24 * 60
            || self.start_minute >= 24 * 60
            || self.end_minute >= 24 * 60
        {
            return false;
        }

        let day = weekday_monday_zero as usize;
        if self.start_minute == self.end_minute {
            return self.enabled_days[day];
        }
        if self.start_minute < self.end_minute {
            self.enabled_days[day]
                && minute_of_day >= self.start_minute
                && minute_of_day < self.end_minute
        } else {
            let previous_day = (day + WEEKDAYS - 1) % WEEKDAYS;
            (self.enabled_days[day] && minute_of_day >= self.start_minute)
                || (self.enabled_days[previous_day] && minute_of_day < self.end_minute)
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RequestConfig {
    pub interval: Duration,
    pub jitter_percent: f64,
    pub failures_before_backoff: u32,
    pub failure_backoff: Duration,
    pub connect_timeout: Duration,
    pub total_timeout: Duration,
    pub max_concurrent_requests: u32,
    pub max_concurrent_per_line: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScanConfig {
    pub interval: Duration,
    pub max_concurrent_requests: u32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MonitorConfig {
    #[serde(default = "default_auto_start_monitoring")]
    pub auto_start_monitoring: bool,
    #[serde(default)]
    pub monitoring_mode: MonitoringMode,
    pub schedule: ScheduleConfig,
    pub requests: RequestConfig,
    pub scan: ScanConfig,
    pub failure_alert_after: Duration,
    #[serde(default)]
    pub use_system_proxy: bool,
    #[serde(default = "default_notification_use_system_proxy")]
    pub notification_use_system_proxy: bool,
    #[serde(default)]
    pub use_proxy_pool: bool,
}

impl Default for MonitorConfig {
    fn default() -> Self {
        Self {
            auto_start_monitoring: default_auto_start_monitoring(),
            monitoring_mode: MonitoringMode::ListedProducts,
            schedule: ScheduleConfig::default(),
            requests: RequestConfig {
                interval: Duration::from_millis(1_500),
                jitter_percent: 100.0 / 3.0,
                failures_before_backoff: 3,
                failure_backoff: Duration::from_secs(20),
                connect_timeout: Duration::from_secs(3),
                total_timeout: Duration::from_secs(8),
                max_concurrent_requests: 4,
                max_concurrent_per_line: 1,
            },
            scan: ScanConfig {
                interval: Duration::from_millis(500),
                max_concurrent_requests: 1,
            },
            failure_alert_after: Duration::from_secs(10 * 60),
            use_system_proxy: false,
            notification_use_system_proxy: default_notification_use_system_proxy(),
            use_proxy_pool: false,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConfigError {
    NoScheduleDays,
    InvalidScheduleTime,
    InvalidJitter,
    ZeroFailureThreshold,
    ZeroFailureBackoff,
    ZeroFailureAlertDelay,
    ZeroConnectTimeout,
    ZeroTotalTimeout,
    ConnectTimeoutExceedsTotal,
    ZeroGlobalConcurrency,
    LineConcurrencyMustBeOne,
    ZeroScanInterval,
    ScanConcurrencyMustBeOne,
}

impl fmt::Display for ConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::NoScheduleDays => "计划至少需要选择一天",
            Self::InvalidScheduleTime => "计划开始和结束时间无效",
            Self::InvalidJitter => "请求抖动必须在 0% 至 100% 之间",
            Self::ZeroFailureThreshold => "连续失败次数必须大于零",
            Self::ZeroFailureBackoff => "失败等待时间必须大于零",
            Self::ZeroFailureAlertDelay => "失败提醒时长必须大于零",
            Self::ZeroConnectTimeout => "连接超时必须大于零",
            Self::ZeroTotalTimeout => "总请求超时必须大于零",
            Self::ConnectTimeoutExceedsTotal => "连接超时不能超过总请求超时",
            Self::ZeroGlobalConcurrency => "全局并发数必须大于零",
            Self::LineConcurrencyMustBeOne => "每条商品线路的并发数必须为一",
            Self::ZeroScanInterval => "扫描间隔必须大于零",
            Self::ScanConcurrencyMustBeOne => "扫描并发数必须为一",
        };
        formatter.write_str(message)
    }
}

impl std::error::Error for ConfigError {}

impl MonitorConfig {
    /// 校验字段结构和值域。
    pub fn validate(&self) -> Result<(), ConfigError> {
        if !self.schedule.enabled_days.iter().any(|enabled| *enabled) {
            return Err(ConfigError::NoScheduleDays);
        }
        if self.schedule.start_minute >= 24 * 60 || self.schedule.end_minute >= 24 * 60 {
            return Err(ConfigError::InvalidScheduleTime);
        }
        if !self.requests.jitter_percent.is_finite()
            || !(0.0..=100.0).contains(&self.requests.jitter_percent)
        {
            return Err(ConfigError::InvalidJitter);
        }
        if self.requests.failures_before_backoff == 0 {
            return Err(ConfigError::ZeroFailureThreshold);
        }
        if self.requests.failure_backoff.is_zero() {
            return Err(ConfigError::ZeroFailureBackoff);
        }
        if self.failure_alert_after.is_zero() {
            return Err(ConfigError::ZeroFailureAlertDelay);
        }
        if self.requests.connect_timeout.is_zero() {
            return Err(ConfigError::ZeroConnectTimeout);
        }
        if self.requests.total_timeout.is_zero() {
            return Err(ConfigError::ZeroTotalTimeout);
        }
        if self.requests.connect_timeout > self.requests.total_timeout {
            return Err(ConfigError::ConnectTimeoutExceedsTotal);
        }
        if self.requests.max_concurrent_requests == 0 {
            return Err(ConfigError::ZeroGlobalConcurrency);
        }
        if self.requests.max_concurrent_per_line != 1 {
            return Err(ConfigError::LineConcurrencyMustBeOne);
        }
        if self.scan.interval.is_zero() {
            return Err(ConfigError::ZeroScanInterval);
        }
        if self.scan.max_concurrent_requests != 1 {
            return Err(ConfigError::ScanConcurrencyMustBeOne);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_config() -> MonitorConfig {
        MonitorConfig::default()
    }

    #[test]
    fn defaults_match_the_product_specification() {
        let config = MonitorConfig::default();
        assert_eq!(BEIJING_TIME_ZONE, "Asia/Shanghai");
        assert_eq!(config.schedule.enabled_days, [true; WEEKDAYS]);
        assert_eq!(config.schedule.start_minute, 9 * 60);
        assert_eq!(config.schedule.end_minute, 19 * 60);
        assert_eq!(config.requests.interval, Duration::from_millis(1_500));
        assert_eq!(config.requests.jitter_percent, 100.0 / 3.0);
        assert_eq!(config.requests.failures_before_backoff, 3);
        assert_eq!(config.requests.failure_backoff, Duration::from_secs(20));
        assert_eq!(config.failure_alert_after, Duration::from_secs(10 * 60));
        assert_eq!(config.requests.connect_timeout, Duration::from_secs(3));
        assert_eq!(config.requests.total_timeout, Duration::from_secs(8));
        assert_eq!(config.requests.max_concurrent_requests, 4);
        assert_eq!(config.requests.max_concurrent_per_line, 1);
        assert_eq!(config.scan.interval, Duration::from_millis(500));
        assert_eq!(config.scan.max_concurrent_requests, 1);
        assert!(!config.use_system_proxy);
    }

    #[test]
    fn schedule_window_includes_start_and_excludes_end() {
        let schedule = ScheduleConfig {
            enabled_days: [true, false, false, false, false, false, false],
            start_minute: 9 * 60,
            end_minute: 19 * 60,
        };
        assert!(!schedule.contains_local_time(0, 9 * 60 - 1));
        assert!(schedule.contains_local_time(0, 9 * 60));
        assert!(schedule.contains_local_time(0, 19 * 60 - 1));
        assert!(!schedule.contains_local_time(0, 19 * 60));
        assert!(!schedule.contains_local_time(7, 10 * 60));
        assert!(!schedule.contains_local_time(0, 24 * 60));
    }

    #[test]
    fn notifications_follow_system_proxy_independently_of_monitoring() {
        let value = serde_json::to_value(MonitorConfig::default()).unwrap();
        assert_eq!(value["notification_use_system_proxy"], true);
        assert_eq!(value["use_system_proxy"], false);
        let mut value = value;
        value
            .as_object_mut()
            .unwrap()
            .remove("notification_use_system_proxy");
        assert!(
            serde_json::from_value::<MonitorConfig>(value)
                .unwrap()
                .notification_use_system_proxy
        );
    }

    #[test]
    fn cross_midnight_window_is_attributed_to_its_start_weekday() {
        let schedule = ScheduleConfig {
            enabled_days: [true, false, false, false, false, false, false],
            start_minute: 22 * 60,
            end_minute: 2 * 60,
        };
        assert!(schedule.contains_local_time(0, 23 * 60));
        assert!(schedule.contains_local_time(1, 0));
        assert!(schedule.contains_local_time(1, 2 * 60 - 1));
        assert!(!schedule.contains_local_time(1, 2 * 60));
        assert!(!schedule.contains_local_time(6, 0));
        assert!(!schedule.contains_local_time(0, 21 * 60));
    }

    #[test]
    fn equal_schedule_bounds_mean_the_selected_day() {
        let schedule = ScheduleConfig {
            enabled_days: [true, false, false, false, false, false, false],
            start_minute: 0,
            end_minute: 0,
        };
        assert!(schedule.contains_local_time(0, 0));
        assert!(schedule.contains_local_time(0, 23 * 60 + 59));
        assert!(!schedule.contains_local_time(1, 12 * 60));
    }

    #[test]
    fn rejects_invalid_time_windows_and_parameter_values() {
        let mut config = valid_config();
        config.schedule.start_minute = config.schedule.end_minute;
        assert_eq!(config.validate(), Ok(()));

        let mut config = valid_config();
        config.schedule.start_minute = 24 * 60;
        assert_eq!(config.validate(), Err(ConfigError::InvalidScheduleTime));

        let mut config = valid_config();
        config.schedule.enabled_days = [false; WEEKDAYS];
        assert_eq!(config.validate(), Err(ConfigError::NoScheduleDays));

        let mut config = valid_config();
        config.requests.interval = Duration::ZERO;
        assert_eq!(config.validate(), Ok(()));

        let mut config = valid_config();
        config.requests.jitter_percent = 100.0;
        assert_eq!(config.validate(), Ok(()));

        config.requests.jitter_percent = 100.1;
        assert_eq!(config.validate(), Err(ConfigError::InvalidJitter));

        let mut config = valid_config();
        config.requests.jitter_percent = -0.1;
        assert_eq!(config.validate(), Err(ConfigError::InvalidJitter));

        let mut config = valid_config();
        config.requests.jitter_percent = f64::NAN;
        assert_eq!(config.validate(), Err(ConfigError::InvalidJitter));

        let mut config = valid_config();
        config.requests.failures_before_backoff = 0;
        assert_eq!(config.validate(), Err(ConfigError::ZeroFailureThreshold));

        let mut config = valid_config();
        config.requests.failure_backoff = Duration::ZERO;
        assert_eq!(config.validate(), Err(ConfigError::ZeroFailureBackoff));

        let mut config = valid_config();
        config.failure_alert_after = Duration::ZERO;
        assert_eq!(config.validate(), Err(ConfigError::ZeroFailureAlertDelay));

        let mut config = valid_config();
        config.requests.connect_timeout = Duration::ZERO;
        assert_eq!(config.validate(), Err(ConfigError::ZeroConnectTimeout));

        let mut config = valid_config();
        config.requests.total_timeout = Duration::ZERO;
        assert_eq!(config.validate(), Err(ConfigError::ZeroTotalTimeout));

        let mut config = valid_config();
        config.requests.connect_timeout = Duration::from_secs(9);
        assert_eq!(
            config.validate(),
            Err(ConfigError::ConnectTimeoutExceedsTotal)
        );

        let mut config = valid_config();
        config.requests.max_concurrent_requests = 0;
        assert_eq!(config.validate(), Err(ConfigError::ZeroGlobalConcurrency));

        let mut config = valid_config();
        config.requests.max_concurrent_per_line = 0;
        assert_eq!(
            config.validate(),
            Err(ConfigError::LineConcurrencyMustBeOne)
        );

        let mut config = valid_config();
        config.requests.max_concurrent_per_line = 2;
        assert_eq!(
            config.validate(),
            Err(ConfigError::LineConcurrencyMustBeOne)
        );

        let mut config = valid_config();
        config.scan.interval = Duration::ZERO;
        assert_eq!(config.validate(), Err(ConfigError::ZeroScanInterval));

        let mut config = valid_config();
        config.scan.max_concurrent_requests = 2;
        assert_eq!(
            config.validate(),
            Err(ConfigError::ScanConcurrencyMustBeOne)
        );
    }

    #[test]
    fn direct_is_default_and_system_proxy_requires_user_opt_in() {
        let mut config = MonitorConfig::default();
        assert!(!config.use_system_proxy);
        assert!(!crate::app::AppConfig::default().use_system_proxy);
        config.use_system_proxy = true;
        assert!(config.validate().is_ok());
    }

    #[test]
    fn valid_defaults_pass_structural_validation() {
        assert_eq!(MonitorConfig::default().validate(), Ok(()));
    }

    #[test]
    fn monitor_config_round_trips_with_standard_serde_duration_representation() {
        let config = MonitorConfig::default();
        let value = serde_json::to_value(&config).unwrap();

        assert_eq!(
            value["requests"]["interval"],
            serde_json::json!({"secs": 1, "nanos": 500_000_000})
        );
        assert_eq!(
            value["requests"]["connect_timeout"],
            serde_json::json!({"secs": 3, "nanos": 0})
        );
        assert_eq!(
            serde_json::from_value::<MonitorConfig>(value).unwrap(),
            config
        );
    }
}
