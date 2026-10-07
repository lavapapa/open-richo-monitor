//! 每个商品只有一个计时器，出口数量和请求数量不会放大累计时长。
use std::{
    collections::BTreeMap,
    time::{Duration, Instant},
};

#[derive(Default)]
pub(crate) struct MonitoringTime {
    active: BTreeMap<String, (Instant, Instant)>,
    pending: BTreeMap<String, u64>,
    versions: BTreeMap<String, (u64, u64)>,
}

impl MonitoringTime {
    pub fn reconcile(&mut self, versions: BTreeMap<String, (u64, u64)>) {
        let stale = self
            .versions
            .iter()
            .filter(|(key, version)| versions.get(*key) != Some(*version))
            .map(|(key, _)| key.clone())
            .collect::<Vec<_>>();
        for key in stale {
            self.forget(&key);
        }
        self.versions = versions;
    }
    pub fn update(&mut self, now: Instant, keys: &[String], until: Instant) {
        self.accrue(now);
        self.active = keys
            .iter()
            .map(|key| {
                let from = self
                    .active
                    .get(key)
                    .filter(|(_, deadline)| *deadline >= now)
                    .map(|(from, _)| *from)
                    .unwrap_or(now);
                (key.clone(), (from, until))
            })
            .collect();
    }

    fn accrue(&mut self, now: Instant) {
        for (key, (from, until)) in &mut self.active {
            let end = now.min(*until);
            let milliseconds = end
                .saturating_duration_since(*from)
                .as_millis()
                .min(i64::MAX as u128) as u64;
            let total = self.pending.entry(key.clone()).or_default();
            *total = total.saturating_add(milliseconds).min(i64::MAX as u64);
            // 保留不足一毫秒的余量，避免高频读取逐渐丢失时间。
            *from += Duration::from_millis(milliseconds);
        }
    }

    pub fn pending(&mut self, now: Instant) -> BTreeMap<String, u64> {
        self.accrue(now);
        self.pending.clone()
    }

    pub fn settled(&mut self) {
        self.pending.clear();
    }

    pub fn stop(&mut self, now: Instant) {
        self.update(now, &[], now);
    }

    pub fn forget(&mut self, key: &str) {
        self.active.remove(key);
        self.pending.remove(key);
        self.versions.remove(key);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timer_stops_at_schedule_boundary_and_never_counts_pause_or_duplicate_outlets() {
        let t = Instant::now();
        let mut timer = MonitoringTime::default();
        timer.update(t, &["65".into(), "65".into()], t + Duration::from_secs(3));
        assert_eq!(timer.pending(t + Duration::from_secs(9))["65"], 3000);
        timer.stop(t + Duration::from_secs(10));
        assert_eq!(timer.pending(t + Duration::from_secs(100))["65"], 3000);
        timer.settled();
        timer.update(
            t + Duration::from_secs(100),
            &["65".into()],
            t + Duration::from_secs(120),
        );
        assert_eq!(timer.pending(t + Duration::from_secs(105))["65"], 5000);
    }

    #[test]
    fn selection_changes_preserve_other_products_and_fractional_milliseconds() {
        let t = Instant::now();
        let mut timer = MonitoringTime::default();
        timer.update(t, &["65".into(), "130".into()], t + Duration::from_secs(10));
        for n in 1..=10 {
            timer.pending(t + Duration::from_micros(n * 100));
        }
        assert_eq!(timer.pending(t + Duration::from_millis(1))["65"], 1);
        timer.update(
            t + Duration::from_secs(2),
            &["130".into()],
            t + Duration::from_secs(10),
        );
        assert_eq!(timer.pending(t + Duration::from_secs(4))["65"], 2000);
        assert_eq!(timer.pending(t + Duration::from_secs(4))["130"], 4000);
        timer.forget("65");
        assert!(!timer.pending(t + Duration::from_secs(5)).contains_key("65"));
    }
}
