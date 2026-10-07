use super::Availability;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ObservationResult {
    pub availability: Availability,
    pub is_show: u8,
    pub stock: Option<f64>,
    pub observed_at_ms: i64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ObservationState {
    pub availability: Availability,
    pub is_show: u8,
    pub stock: Option<f64>,
    pub request_sequence: u64,
    pub observed_at_ms: i64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ResponseFailure {
    Failed,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AvailabilityResponse {
    pub sequence: u64,
    pub generation: u64,
    pub result: Result<ObservationResult, ResponseFailure>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RuntimeGate {
    pub generation: u64,
    pub enabled: bool,
    pub paused: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AvailabilityTransition {
    FirstObservedInStock,
    OutOfStockToInStock,
    StockIncreased,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IgnoreReason {
    Inactive,
    StaleGeneration,
    StaleSequence,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ApplyOutcome {
    Applied {
        transition: Option<AvailabilityTransition>,
    },
    Failed,
    Ignored(IgnoreReason),
}

#[derive(Clone, Debug, PartialEq)]
pub struct ObservationReducer {
    state: Option<ObservationState>,
}

impl ObservationReducer {
    pub fn new(state: Option<ObservationState>) -> Self {
        Self { state }
    }

    pub fn prepare(
        &mut self,
        response: AvailabilityResponse,
        gate: RuntimeGate,
    ) -> PrepareOutcome<'_> {
        if !gate.enabled || gate.paused {
            return PrepareOutcome::Ignored(IgnoreReason::Inactive);
        }
        if response.generation != gate.generation {
            return PrepareOutcome::Ignored(IgnoreReason::StaleGeneration);
        }
        if self
            .state
            .is_some_and(|state| response.sequence <= state.request_sequence)
        {
            return PrepareOutcome::Ignored(IgnoreReason::StaleSequence);
        }

        let Ok(result) = response.result else {
            return PrepareOutcome::Failed;
        };
        if (result.stock.is_none()
            && (result.is_show != 0 || result.availability != Availability::OutOfStock))
            || (result.availability == Availability::InStock
                && !result
                    .stock
                    .is_some_and(|stock| stock.is_finite() && stock > 0.0))
        {
            return PrepareOutcome::Failed;
        }

        let listing = result.is_show == 1 && self.state.is_none_or(|state| state.is_show != 1);
        let restock = result.availability == Availability::InStock
            && self
                .state
                .is_none_or(|state| state.availability == Availability::OutOfStock);
        let increased = result.is_show == 1
            && result.availability == Availability::InStock
            && self.state.is_some_and(|previous| {
                previous.is_show == 1
                    && previous.availability == Availability::InStock
                    && previous
                        .stock
                        .zip(result.stock)
                        .is_some_and(|(before, after)| after > before)
            });
        let transition = if listing || restock {
            Some(if self.state.is_none() {
                AvailabilityTransition::FirstObservedInStock
            } else {
                AvailabilityTransition::OutOfStockToInStock
            })
        } else {
            increased.then_some(AvailabilityTransition::StockIncreased)
        };
        let state = ObservationState {
            availability: result.availability,
            is_show: result.is_show,
            stock: result.stock,
            request_sequence: response.sequence,
            observed_at_ms: result.observed_at_ms,
        };

        PrepareOutcome::Prepared(PreparedObservation {
            reducer: self,
            state,
            transition,
        })
    }

    pub fn state(&self) -> Option<&ObservationState> {
        self.state.as_ref()
    }
}

pub enum PrepareOutcome<'a> {
    Prepared(PreparedObservation<'a>),
    Failed,
    Ignored(IgnoreReason),
}

/// A reducer decision that remains uncommitted until its persistence transaction succeeds.
pub struct PreparedObservation<'a> {
    reducer: &'a mut ObservationReducer,
    state: ObservationState,
    transition: Option<AvailabilityTransition>,
}

impl PreparedObservation<'_> {
    pub fn state(&self) -> ObservationState {
        self.state
    }

    pub fn transition(&self) -> Option<AvailabilityTransition> {
        self.transition
    }

    pub fn commit(self) -> ApplyOutcome {
        let Self {
            reducer,
            state,
            transition,
        } = self;
        reducer.state = Some(state);
        ApplyOutcome::Applied { transition }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn listed_stock_increase_notifies_once_but_decrease_does_not() {
        let mut reducer = ObservationReducer::new(Some(state(Availability::InStock, 1)));
        let mut increased = success(2, 1, Availability::InStock);
        increased.result.as_mut().unwrap().stock = Some(5.0);
        assert_eq!(
            apply(&mut reducer, increased, gate(1)),
            ApplyOutcome::Applied {
                transition: Some(AvailabilityTransition::StockIncreased)
            }
        );
        increased.sequence = 3;
        assert_eq!(
            apply(&mut reducer, increased, gate(1)),
            ApplyOutcome::Applied { transition: None }
        );
        assert_eq!(
            apply(&mut reducer, success(4, 1, Availability::InStock), gate(1)),
            ApplyOutcome::Applied { transition: None }
        );
        increased.sequence = 3;
        assert_eq!(
            apply(&mut reducer, increased, gate(1)),
            ApplyOutcome::Ignored(IgnoreReason::StaleSequence)
        );
        increased.sequence = 5;
        increased.result.as_mut().unwrap().is_show = 0;
        assert_eq!(
            apply(&mut reducer, increased, gate(1)),
            ApplyOutcome::Applied { transition: None }
        );
    }

    #[test]
    fn listing_with_zero_stock_notifies_once_and_restock_still_notifies() {
        let mut reducer = ObservationReducer::new(None);
        let mut listed = success(1, 1, Availability::OutOfStock);
        listed.result.as_mut().unwrap().is_show = 1;
        assert_eq!(
            apply(&mut reducer, listed, gate(1)),
            ApplyOutcome::Applied {
                transition: Some(AvailabilityTransition::FirstObservedInStock)
            }
        );
        listed.sequence = 2;
        assert_eq!(
            apply(&mut reducer, listed, gate(1)),
            ApplyOutcome::Applied { transition: None }
        );
        assert_eq!(
            apply(&mut reducer, success(3, 1, Availability::InStock), gate(1)),
            ApplyOutcome::Applied {
                transition: Some(AvailabilityTransition::OutOfStockToInStock)
            }
        );
        apply(
            &mut reducer,
            success(4, 1, Availability::OutOfStock),
            gate(1),
        );
        listed.sequence = 5;
        assert_eq!(
            apply(&mut reducer, listed, gate(1)),
            ApplyOutcome::Applied {
                transition: Some(AvailabilityTransition::OutOfStockToInStock)
            }
        );
    }

    fn gate(generation: u64) -> RuntimeGate {
        RuntimeGate {
            generation,
            enabled: true,
            paused: false,
        }
    }

    fn success(sequence: u64, generation: u64, availability: Availability) -> AvailabilityResponse {
        AvailabilityResponse {
            sequence,
            generation,
            result: Ok(ObservationResult {
                availability,
                is_show: u8::from(availability == Availability::InStock),
                stock: Some(if availability == Availability::InStock {
                    3.0
                } else {
                    0.0
                }),
                observed_at_ms: sequence as i64 * 100,
            }),
        }
    }

    fn failure(sequence: u64, generation: u64) -> AvailabilityResponse {
        AvailabilityResponse {
            sequence,
            generation,
            result: Err(ResponseFailure::Failed),
        }
    }

    fn apply(
        reducer: &mut ObservationReducer,
        response: AvailabilityResponse,
        gate: RuntimeGate,
    ) -> ApplyOutcome {
        match reducer.prepare(response, gate) {
            PrepareOutcome::Prepared(prepared) => prepared.commit(),
            PrepareOutcome::Failed => ApplyOutcome::Failed,
            PrepareOutcome::Ignored(reason) => ApplyOutcome::Ignored(reason),
        }
    }

    fn state(availability: Availability, sequence: u64) -> ObservationState {
        ObservationState {
            availability,
            is_show: u8::from(availability == Availability::InStock),
            stock: Some(if availability == Availability::InStock {
                3.0
            } else {
                0.0
            }),
            request_sequence: sequence,
            observed_at_ms: sequence as i64 * 100,
        }
    }

    #[test]
    fn older_valid_response_does_not_replace_newer_applied_state() {
        let mut reducer = ObservationReducer::new(Some(state(Availability::OutOfStock, 8)));

        assert_eq!(
            apply(
                &mut reducer,
                success(10, 1, Availability::OutOfStock),
                gate(1)
            ),
            ApplyOutcome::Applied { transition: None }
        );
        assert_eq!(
            apply(&mut reducer, success(9, 1, Availability::InStock), gate(1)),
            ApplyOutcome::Ignored(IgnoreReason::StaleSequence)
        );
        assert_eq!(reducer.state(), Some(&state(Availability::OutOfStock, 10)));
    }

    #[test]
    fn earlier_success_can_apply_after_a_newer_failure() {
        let mut reducer = ObservationReducer::new(None);

        assert_eq!(
            apply(&mut reducer, failure(2, 1), gate(1)),
            ApplyOutcome::Failed
        );
        assert_eq!(reducer.state(), None);
        assert_eq!(
            apply(&mut reducer, success(1, 1, Availability::InStock), gate(1)),
            ApplyOutcome::Applied {
                transition: Some(AvailabilityTransition::FirstObservedInStock)
            }
        );
        assert_eq!(reducer.state(), Some(&state(Availability::InStock, 1)));
    }

    #[test]
    fn multiple_outlets_create_only_one_in_stock_transition() {
        let mut reducer = ObservationReducer::new(Some(state(Availability::OutOfStock, 0)));

        let newer = apply(&mut reducer, success(2, 1, Availability::InStock), gate(1));
        let older = apply(&mut reducer, success(1, 1, Availability::InStock), gate(1));

        assert_eq!(
            newer,
            ApplyOutcome::Applied {
                transition: Some(AvailabilityTransition::OutOfStockToInStock)
            }
        );
        assert_eq!(older, ApplyOutcome::Ignored(IgnoreReason::StaleSequence));
        assert_eq!(reducer.state(), Some(&state(Availability::InStock, 2)));
    }

    #[test]
    fn first_in_stock_observation_is_marked_once() {
        let mut reducer = ObservationReducer::new(None);

        assert_eq!(
            apply(&mut reducer, success(1, 1, Availability::InStock), gate(1)),
            ApplyOutcome::Applied {
                transition: Some(AvailabilityTransition::FirstObservedInStock)
            }
        );
        assert_eq!(
            apply(&mut reducer, success(2, 1, Availability::InStock), gate(1)),
            ApplyOutcome::Applied { transition: None }
        );
    }

    #[test]
    fn dropping_prepared_observation_does_not_mutate_reducer() {
        let mut reducer = ObservationReducer::new(Some(state(Availability::OutOfStock, 1)));

        let PrepareOutcome::Prepared(prepared) =
            reducer.prepare(success(2, 1, Availability::InStock), gate(1))
        else {
            panic!("有效响应应通过 reducer 判定");
        };
        assert_eq!(prepared.state(), state(Availability::InStock, 2));
        assert_eq!(
            prepared.transition(),
            Some(AvailabilityTransition::OutOfStockToInStock)
        );
        drop(prepared);

        assert_eq!(reducer.state(), Some(&state(Availability::OutOfStock, 1)));
    }

    #[test]
    fn paused_response_is_ignored_without_changing_state() {
        let mut reducer = ObservationReducer::new(Some(state(Availability::OutOfStock, 1)));
        let paused = RuntimeGate {
            generation: 1,
            enabled: true,
            paused: true,
        };

        assert_eq!(
            apply(&mut reducer, success(2, 1, Availability::InStock), paused),
            ApplyOutcome::Ignored(IgnoreReason::Inactive)
        );
        assert_eq!(reducer.state(), Some(&state(Availability::OutOfStock, 1)));
    }

    #[test]
    fn cancelled_product_response_is_ignored_without_changing_state() {
        let mut reducer = ObservationReducer::new(Some(state(Availability::OutOfStock, 1)));
        let cancelled = RuntimeGate {
            generation: 1,
            enabled: false,
            paused: false,
        };

        assert_eq!(
            apply(
                &mut reducer,
                success(2, 1, Availability::InStock),
                cancelled
            ),
            ApplyOutcome::Ignored(IgnoreReason::Inactive)
        );
        assert_eq!(reducer.state(), Some(&state(Availability::OutOfStock, 1)));
    }

    #[test]
    fn previous_generation_response_after_resume_is_ignored() {
        let mut reducer = ObservationReducer::new(Some(state(Availability::OutOfStock, 1)));

        assert_eq!(
            apply(&mut reducer, success(2, 1, Availability::InStock), gate(2)),
            ApplyOutcome::Ignored(IgnoreReason::StaleGeneration)
        );
        assert_eq!(reducer.state(), Some(&state(Availability::OutOfStock, 1)));
        assert_eq!(
            apply(&mut reducer, success(3, 2, Availability::InStock), gate(2)),
            ApplyOutcome::Applied {
                transition: Some(AvailabilityTransition::OutOfStockToInStock)
            }
        );
    }

    #[test]
    fn absent_stock_is_preserved_and_only_positive_stock_notifies_once() {
        let mut reducer = ObservationReducer::new(None);
        let mut absent = success(1, 1, Availability::OutOfStock);
        absent.result.as_mut().unwrap().stock = None;
        assert_eq!(
            apply(&mut reducer, absent, gate(1)),
            ApplyOutcome::Applied { transition: None }
        );
        assert_eq!(reducer.state().unwrap().stock, None);
        assert_eq!(
            apply(&mut reducer, success(2, 1, Availability::InStock), gate(1)),
            ApplyOutcome::Applied {
                transition: Some(AvailabilityTransition::OutOfStockToInStock)
            }
        );
        assert_eq!(
            apply(&mut reducer, success(3, 1, Availability::InStock), gate(1)),
            ApplyOutcome::Applied { transition: None }
        );
    }

    #[test]
    fn absence_requires_unlisted_out_of_stock_and_in_stock_requires_positive_stock() {
        let mut reducer = ObservationReducer::new(None);
        for (availability, is_show, stock) in [
            (Availability::OutOfStock, 1, None),
            (Availability::InStock, 1, None),
            (Availability::InStock, 1, Some(0.0)),
            (Availability::InStock, 1, Some(-1.0)),
        ] {
            let response = AvailabilityResponse {
                sequence: 1,
                generation: 1,
                result: Ok(ObservationResult {
                    availability,
                    is_show,
                    stock,
                    observed_at_ms: 100,
                }),
            };
            assert_eq!(apply(&mut reducer, response, gate(1)), ApplyOutcome::Failed);
            assert_eq!(reducer.state(), None);
        }
    }
}
