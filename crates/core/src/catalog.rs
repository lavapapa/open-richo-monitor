use crate::ricoh::ProductDetail;
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_SCAN_ID: AtomicU64 = AtomicU64::new(0);

pub const MAX_SCAN_IDS: u64 = 1_000;

pub const DEFAULT_PRODUCTS: &[(u64, &str)] = &[
    (9, "RICOH GR IIIx"),
    (18, "RICOH GR IIIx Urban Edition 都市版"),
    (19, "官翻品 GR III ING 套装版本"),
    (38, "RICOH GR III Diary Edition 日记版"),
    (45, "RICOH GRIIIx HDF 指环带套餐"),
    (46, "RICOH GR III HDF 套餐"),
    (47, "RICOH GR III 套装"),
    (48, "RICOH GR III Street Edition 街拍版"),
    (49, "RICOH GR III Street Edition 街拍版套装"),
    (50, "官翻品 RICOH GR III HDF"),
    (51, "RICOH GR IIIx 指环带套装"),
    (52, "RICOH GR IIIx Urban  都市版套装"),
    (65, "官翻品 GR IIIx"),
    (66, "官翻品 RICOH GR III"),
    (67, "官翻品 RICOH GR III 日记版"),
    (108, "GR SPACE VIP会员卡"),
    (114, "官翻品 GR IIIx HDF"),
    (122, "RICOH GR IV 电池套装"),
    (123, "RICOH GR IV HDF 电池充电器套装"),
    (124, "RICOH GR IV Monochrome"),
    (130, "官翻品 GR IV"),
    (245, "官翻品 GR IV HDF"),
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScanStatus {
    Running,
    Paused,
    Completed,
    Cancelled,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum ScanOutcome {
    Found(ProductDetail),
    Failed,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ScannedItem {
    pub requested_product_id: u64,
    pub outcome: ScanOutcome,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ScanError {
    NonPositiveStart,
    NonPositiveEnd,
    EndBeforeStart,
    TooManyIds { count: u64 },
    NoRequestInFlight,
    ProductIdMismatch { requested: u64, result: u64 },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ScanCheckpoint {
    scan_id: u64,
    start_id: u64,
    end_id: u64,
    next_id: Option<u64>,
    completed: Vec<ScannedItem>,
}

impl ScanCheckpoint {
    pub fn scan_id(&self) -> u64 {
        self.scan_id
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ProductIdScan {
    scan_id: u64,
    start_id: u64,
    end_id: u64,
    next_id: Option<u64>,
    in_flight_id: Option<u64>,
    status: ScanStatus,
    completed: Vec<ScannedItem>,
}

impl ProductIdScan {
    pub fn new(start_id: u64, end_id: u64) -> Result<Self, ScanError> {
        let count = range_count(start_id, end_id)?;
        if count > MAX_SCAN_IDS {
            return Err(ScanError::TooManyIds { count });
        }
        Ok(Self {
            scan_id: (std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos() as u64)
                .wrapping_add(NEXT_SCAN_ID.fetch_add(1, Ordering::Relaxed)),
            start_id,
            end_id,
            next_id: Some(start_id),
            in_flight_id: None,
            status: ScanStatus::Running,
            completed: Vec::new(),
        })
    }

    pub fn restore(checkpoint: ScanCheckpoint) -> Self {
        Self {
            scan_id: checkpoint.scan_id,
            start_id: checkpoint.start_id,
            end_id: checkpoint.end_id,
            next_id: checkpoint.next_id,
            in_flight_id: None,
            status: if checkpoint.next_id.is_some() {
                ScanStatus::Paused
            } else {
                ScanStatus::Completed
            },
            completed: checkpoint.completed,
        }
    }

    pub fn checkpoint(&self) -> Option<ScanCheckpoint> {
        if self.status == ScanStatus::Cancelled {
            return None;
        }
        Some(ScanCheckpoint {
            scan_id: self.scan_id,
            start_id: self.start_id,
            end_id: self.end_id,
            next_id: self.in_flight_id.or(self.next_id),
            completed: self.completed.clone(),
        })
    }

    pub fn next_id(&mut self) -> Option<u64> {
        if self.status != ScanStatus::Running || self.in_flight_id.is_some() {
            return None;
        }
        let id = self.next_id.take()?;
        self.in_flight_id = Some(id);
        Some(id)
    }

    pub fn record_found(&mut self, product: ProductDetail) -> Result<(), ScanError> {
        let requested_id = self.in_flight_id.ok_or(ScanError::NoRequestInFlight)?;
        if product.product_id != requested_id {
            return Err(ScanError::ProductIdMismatch {
                requested: requested_id,
                result: product.product_id,
            });
        }
        self.complete_current(ScanOutcome::Found(product))
    }

    pub fn record_failure(&mut self) -> Result<(), ScanError> {
        self.complete_current(ScanOutcome::Failed)
    }

    pub fn pause(&mut self) {
        if self.status == ScanStatus::Running {
            self.status = ScanStatus::Paused;
        }
    }

    pub fn continue_scan(&mut self) {
        if self.status == ScanStatus::Paused {
            self.status = ScanStatus::Running;
        }
    }

    pub fn cancel(&mut self) -> Option<u64> {
        if matches!(self.status, ScanStatus::Completed | ScanStatus::Cancelled) {
            return None;
        }
        self.status = ScanStatus::Cancelled;
        let current = self.in_flight_id.take();
        if current.is_some() {
            self.next_id = current;
        }
        current
    }

    pub fn status(&self) -> ScanStatus {
        self.status
    }

    pub fn current_id(&self) -> Option<u64> {
        self.in_flight_id
    }

    pub fn start_id(&self) -> u64 {
        self.start_id
    }

    pub fn scan_id(&self) -> u64 {
        self.scan_id
    }

    pub fn end_id(&self) -> u64 {
        self.end_id
    }

    pub fn in_flight_id(&self) -> Option<u64> {
        self.in_flight_id
    }

    pub fn completed(&self) -> &[ScannedItem] {
        &self.completed
    }

    fn complete_current(&mut self, outcome: ScanOutcome) -> Result<(), ScanError> {
        let id = self
            .in_flight_id
            .take()
            .ok_or(ScanError::NoRequestInFlight)?;
        self.completed.push(ScannedItem {
            requested_product_id: id,
            outcome,
        });
        if id == self.end_id {
            self.next_id = None;
            self.status = ScanStatus::Completed;
        } else {
            self.next_id = Some(id + 1);
        }
        Ok(())
    }
}

fn range_count(start_id: u64, end_id: u64) -> Result<u64, ScanError> {
    if start_id == 0 {
        return Err(ScanError::NonPositiveStart);
    }
    if end_id == 0 {
        return Err(ScanError::NonPositiveEnd);
    }
    if end_id < start_id {
        return Err(ScanError::EndBeforeStart);
    }
    Ok(end_id - start_id + 1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::availability::Availability;
    use serde_json::json;

    fn product(id: u64) -> ProductDetail {
        ProductDetail {
            product_id: id,
            name: format!("商品 {id}"),
            is_show: 0,
            stock: json!(0).as_number().unwrap().clone(),
            availability: Availability::OutOfStock,
            metadata: Default::default(),
        }
    }

    #[test]
    fn validates_positive_inclusive_ranges_of_at_most_one_thousand_ids() {
        assert_eq!(ProductIdScan::new(0, 2), Err(ScanError::NonPositiveStart));
        assert_eq!(ProductIdScan::new(3, 2), Err(ScanError::EndBeforeStart));
        assert!(ProductIdScan::new(1, 1_000).is_ok());
        assert_eq!(
            ProductIdScan::new(1, 1_001),
            Err(ScanError::TooManyIds { count: 1_001 })
        );
    }

    #[test]
    fn serializes_requests_and_advances_after_each_item_failure() {
        let mut scan = ProductIdScan::new(40, 42).unwrap();
        assert_eq!(scan.next_id(), Some(40));
        assert_eq!(scan.next_id(), None);
        assert_eq!(scan.in_flight_id(), Some(40));

        scan.record_failure().unwrap();
        assert_eq!(scan.next_id(), Some(41));
        scan.record_found(product(41)).unwrap();
        assert_eq!(scan.next_id(), Some(42));
        scan.record_failure().unwrap();
        assert_eq!(scan.status(), ScanStatus::Completed);
        assert_eq!(scan.next_id(), None);
        assert_eq!(scan.completed().len(), 3);
    }

    #[test]
    fn pause_checkpoint_restore_and_continue_resume_at_first_unfinished_id() {
        let mut scan = ProductIdScan::new(50, 52).unwrap();
        assert_eq!(scan.next_id(), Some(50));
        scan.record_found(product(50)).unwrap();
        assert_eq!(scan.next_id(), Some(51));
        scan.pause();
        assert_eq!(scan.next_id(), None);

        let mut restored = ProductIdScan::restore(scan.checkpoint().unwrap());
        assert_eq!(restored.status(), ScanStatus::Paused);
        assert_eq!(restored.completed().len(), 1);
        restored.continue_scan();
        assert_eq!(restored.next_id(), Some(51));
    }

    #[test]
    fn checkpoint_retries_an_unfinished_in_flight_id_without_repeating_completed_items() {
        let mut scan = ProductIdScan::new(60, 62).unwrap();
        assert_eq!(scan.next_id(), Some(60));
        scan.record_failure().unwrap();
        assert_eq!(scan.next_id(), Some(61));

        let mut restored = ProductIdScan::restore(scan.checkpoint().unwrap());
        assert_eq!(restored.completed().len(), 1);
        restored.continue_scan();
        assert_eq!(restored.next_id(), Some(61));
    }

    #[test]
    fn cancel_stops_new_work_and_returns_the_in_flight_id_to_abort() {
        let mut scan = ProductIdScan::new(70, 72).unwrap();
        assert_eq!(scan.next_id(), Some(70));
        scan.record_found(product(70)).unwrap();
        assert_eq!(scan.next_id(), Some(71));
        assert_eq!(scan.cancel(), Some(71));
        assert_eq!(scan.status(), ScanStatus::Cancelled);
        assert_eq!(scan.next_id(), None);
        assert_eq!(scan.cancel(), None);
        assert_eq!(scan.completed().len(), 1);
        assert!(scan.checkpoint().is_none());
    }

    #[test]
    fn pause_keeps_an_in_flight_result_but_stops_the_next_request() {
        let mut scan = ProductIdScan::new(80, 81).unwrap();
        assert_eq!(scan.next_id(), Some(80));
        scan.pause();
        assert_eq!(scan.next_id(), None);
        assert_eq!(scan.in_flight_id(), Some(80));
        scan.record_failure().unwrap();
        assert_eq!(scan.status(), ScanStatus::Paused);
        assert_eq!(scan.in_flight_id(), None);
        assert_eq!(scan.checkpoint().unwrap().next_id, Some(81));
    }

    #[test]
    fn completes_at_the_maximum_positive_id_without_overflow() {
        let mut scan = ProductIdScan::new(u64::MAX, u64::MAX).unwrap();
        assert_eq!(scan.next_id(), Some(u64::MAX));
        scan.record_failure().unwrap();
        assert_eq!(scan.status(), ScanStatus::Completed);
        assert_eq!(scan.next_id(), None);
    }
}
