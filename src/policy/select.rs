// ABOUTME: Selects one healthy provider/model route from validated policy inputs.
// ABOUTME: Keeps price, performance, hysteresis, and traffic-share math deterministic.

use std::cmp::Ordering;
use std::collections::{HashMap, VecDeque};

use rand::Rng;
use rand::distr::{Distribution, weighted::WeightedIndex};
use rust_decimal::Decimal;
use rust_decimal::prelude::ToPrimitive;
use thiserror::Error;

use super::measurements::ModelMeasurements;
use super::preference::Preference;

#[derive(Clone, Debug, PartialEq)]
pub struct Candidate {
    provider_id: String,
    combined_price_per_mtok: Decimal,
    priority: u8,
    measurements: Option<ModelMeasurements>,
    healthy: bool,
}

impl Candidate {
    pub fn new(
        provider_id: impl Into<String>,
        input_price_per_mtok: Decimal,
        output_price_per_mtok: Decimal,
        priority: u8,
        measurements: Option<ModelMeasurements>,
        healthy: bool,
    ) -> Result<Self, CandidateError> {
        if input_price_per_mtok < Decimal::ZERO {
            return Err(CandidateError::NegativeInputPrice);
        }
        if output_price_per_mtok < Decimal::ZERO {
            return Err(CandidateError::NegativeOutputPrice);
        }
        let combined_price_per_mtok = input_price_per_mtok
            .checked_add(output_price_per_mtok)
            .ok_or(CandidateError::PriceOverflow)?;
        if measurements.is_some_and(|measurements| {
            !measurements.throughput_tokens_per_second.is_finite()
                || measurements.throughput_tokens_per_second < 0.0
                || !measurements.time_to_first_token_seconds.is_finite()
                || measurements.time_to_first_token_seconds < 0.0
        }) {
            return Err(CandidateError::InvalidMeasurements);
        }

        Ok(Self {
            provider_id: provider_id.into(),
            combined_price_per_mtok,
            priority,
            measurements,
            healthy,
        })
    }

    pub fn provider_id(&self) -> &str {
        &self.provider_id
    }

    pub fn combined_price_per_mtok(&self) -> Decimal {
        self.combined_price_per_mtok
    }

    pub fn priority(&self) -> u8 {
        self.priority
    }

    pub fn measurements(&self) -> Option<ModelMeasurements> {
        self.measurements
    }

    pub fn is_healthy(&self) -> bool {
        self.healthy
    }
}

#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum CandidateError {
    #[error("candidate input price must not be negative")]
    NegativeInputPrice,
    #[error("candidate output price must not be negative")]
    NegativeOutputPrice,
    #[error("candidate combined input and output price is too large")]
    PriceOverflow,
    #[error("candidate measurements must be finite and non-negative")]
    InvalidMeasurements,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PolicyConfig {
    combined_price_ceiling_per_mtok: Decimal,
    hysteresis_fraction: f64,
    max_share: Decimal,
}

impl PolicyConfig {
    pub fn new(
        combined_price_ceiling_per_mtok: Decimal,
        hysteresis_fraction: f64,
        max_share: Decimal,
    ) -> Result<Self, PolicyConfigError> {
        if combined_price_ceiling_per_mtok < Decimal::ZERO {
            return Err(PolicyConfigError::NegativePriceCeiling);
        }
        if !hysteresis_fraction.is_finite() || hysteresis_fraction < 0.0 {
            return Err(PolicyConfigError::InvalidHysteresis);
        }
        if max_share <= Decimal::ZERO || max_share > Decimal::ONE {
            return Err(PolicyConfigError::InvalidMaxShare);
        }

        Ok(Self {
            combined_price_ceiling_per_mtok,
            hysteresis_fraction,
            max_share,
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum PolicyConfigError {
    #[error("combined price ceiling must not be negative")]
    NegativePriceCeiling,
    #[error("hysteresis fraction must be finite and non-negative")]
    InvalidHysteresis,
    #[error("max share must be greater than zero and at most one")]
    InvalidMaxShare,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ShareTracker {
    capacity: usize,
    history: VecDeque<String>,
    counts: HashMap<String, usize>,
}

impl ShareTracker {
    pub fn new(capacity: usize) -> Result<Self, ShareTrackerError> {
        if capacity == 0 {
            return Err(ShareTrackerError::ZeroCapacity);
        }

        Ok(Self {
            capacity,
            history: VecDeque::with_capacity(capacity),
            counts: HashMap::new(),
        })
    }

    pub fn record(&mut self, provider_id: impl Into<String>) {
        if self.history.len() == self.capacity {
            let expired = self
                .history
                .pop_front()
                .expect("a full share window has an oldest entry");
            let count = self
                .counts
                .get_mut(&expired)
                .expect("every share history entry has a count");
            *count -= 1;
            if *count == 0 {
                self.counts.remove(&expired);
            }
        }

        let provider_id = provider_id.into();
        *self.counts.entry(provider_id.clone()).or_default() += 1;
        self.history.push_back(provider_id);
    }

    pub fn share(&self, provider_id: &str) -> Decimal {
        if self.history.is_empty() {
            return Decimal::ZERO;
        }

        Decimal::from(self.count(provider_id)) / Decimal::from(self.history.len())
    }

    fn allows_next(&self, provider_id: &str, max_share: Decimal) -> bool {
        let next_len = self.history.len().saturating_add(1).min(self.capacity);
        let expired_matches = usize::from(
            self.history.len() == self.capacity
                && self
                    .history
                    .front()
                    .is_some_and(|expired| expired == provider_id),
        );
        let next_count = self.count(provider_id) + 1 - expired_matches;
        let quota = (max_share * Decimal::from(next_len))
            .ceil()
            .to_usize()
            .expect("validated share quota fits usize");

        next_count <= quota
    }

    fn incumbent_provider_id(&self) -> Option<&str> {
        let highest_count = self.counts.values().copied().max()?;

        self.history
            .iter()
            .rev()
            .find(|provider_id| self.count(provider_id) == highest_count)
            .map(String::as_str)
    }

    fn count(&self, provider_id: &str) -> usize {
        self.counts.get(provider_id).copied().unwrap_or_default()
    }
}

#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum ShareTrackerError {
    #[error("share tracking capacity must be greater than zero")]
    ZeroCapacity,
}

pub fn select_route<'a, R: Rng + ?Sized>(
    candidates: &'a [Candidate],
    preference: Preference,
    config: &PolicyConfig,
    recent_share: &ShareTracker,
    rng: &mut R,
) -> Option<&'a Candidate> {
    match preference {
        Preference::Default => select_default(candidates, config, recent_share, rng),
        Preference::Balanced => select_balanced(candidates, rng),
        Preference::Price => healthy(candidates).min_by(|left, right| compare_price(left, right)),
        Preference::Throughput => {
            healthy(candidates).max_by(|left, right| compare_throughput(left, right))
        }
        Preference::Latency => {
            healthy(candidates).min_by(|left, right| compare_latency(left, right))
        }
    }
}

fn select_default<'a, R: Rng + ?Sized>(
    candidates: &'a [Candidate],
    config: &PolicyConfig,
    recent_share: &ShareTracker,
    rng: &mut R,
) -> Option<&'a Candidate> {
    let eligible: Vec<_> = healthy(candidates)
        .filter(|candidate| {
            candidate.combined_price_per_mtok() <= config.combined_price_ceiling_per_mtok
                && recent_share.allows_next(candidate.provider_id(), config.max_share)
        })
        .collect();
    let incumbent = recent_share
        .incumbent_provider_id()
        .and_then(|provider_id| {
            eligible
                .iter()
                .copied()
                .find(|candidate| candidate.provider_id() == provider_id)
        });
    let incumbent_throughput = incumbent.and_then(throughput);
    let weights: Vec<_> = eligible
        .iter()
        .map(|candidate| {
            let Some(candidate_throughput) = throughput(candidate) else {
                return 0.0;
            };
            let Some(incumbent_throughput) = incumbent_throughput else {
                return candidate_throughput;
            };

            if incumbent.is_some_and(|incumbent| incumbent.provider_id() != candidate.provider_id())
                && candidate_throughput > incumbent_throughput
                && candidate_throughput
                    <= incumbent_throughput + incumbent_throughput * config.hysteresis_fraction
            {
                incumbent_throughput
            } else {
                candidate_throughput
            }
        })
        .collect();

    weighted_pick(&eligible, &weights, rng).or_else(|| strict_priority(eligible.into_iter()))
}

fn select_balanced<'a, R: Rng + ?Sized>(
    candidates: &'a [Candidate],
    rng: &mut R,
) -> Option<&'a Candidate> {
    let healthy: Vec<_> = healthy(candidates).collect();
    let free: Vec<_> = healthy
        .iter()
        .copied()
        .filter(|candidate| candidate.combined_price_per_mtok().is_zero())
        .collect();
    if !free.is_empty() {
        return weighted_pick(&free, &vec![1.0; free.len()], rng);
    }

    let cheapest = healthy
        .iter()
        .map(|candidate| candidate.combined_price_per_mtok())
        .min()?;
    let weights: Vec<_> = healthy
        .iter()
        .map(|candidate| {
            let ratio = (cheapest / candidate.combined_price_per_mtok())
                .to_f64()
                .expect("a normalized positive Decimal price ratio fits f64");
            ratio * ratio
        })
        .collect();

    weighted_pick(&healthy, &weights, rng)
}

fn healthy(candidates: &[Candidate]) -> impl Iterator<Item = &Candidate> {
    candidates.iter().filter(|candidate| candidate.is_healthy())
}

fn throughput(candidate: &Candidate) -> Option<f64> {
    candidate
        .measurements()
        .map(|measurements| measurements.throughput_tokens_per_second)
        .filter(|throughput| *throughput > 0.0)
}

fn weighted_pick<'a, R: Rng + ?Sized>(
    candidates: &[&'a Candidate],
    weights: &[f64],
    rng: &mut R,
) -> Option<&'a Candidate> {
    let max_weight = weights.iter().copied().fold(0.0, f64::max);
    if max_weight <= 0.0 {
        return None;
    }
    let normalized: Vec<_> = weights.iter().map(|weight| weight / max_weight).collect();
    let distribution = WeightedIndex::new(normalized).ok()?;

    candidates.get(distribution.sample(rng)).copied()
}

fn strict_priority<'a>(candidates: impl Iterator<Item = &'a Candidate>) -> Option<&'a Candidate> {
    candidates.min_by(|left, right| compare_priority(left, right))
}

fn compare_price(left: &Candidate, right: &Candidate) -> Ordering {
    left.combined_price_per_mtok()
        .cmp(&right.combined_price_per_mtok())
        .then_with(|| compare_priority(left, right))
}

fn compare_throughput(left: &Candidate, right: &Candidate) -> Ordering {
    compare_optional_f64(
        left.measurements()
            .map(|measurements| measurements.throughput_tokens_per_second),
        right
            .measurements()
            .map(|measurements| measurements.throughput_tokens_per_second),
    )
    .then_with(|| compare_priority(right, left))
}

fn compare_latency(left: &Candidate, right: &Candidate) -> Ordering {
    match (
        left.measurements()
            .map(|measurements| measurements.time_to_first_token_seconds),
        right
            .measurements()
            .map(|measurements| measurements.time_to_first_token_seconds),
    ) {
        (Some(left), Some(right)) => left.total_cmp(&right),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    }
    .then_with(|| compare_priority(left, right))
}

fn compare_optional_f64(left: Option<f64>, right: Option<f64>) -> Ordering {
    match (left, right) {
        (Some(left), Some(right)) => left.total_cmp(&right),
        (Some(_), None) => Ordering::Greater,
        (None, Some(_)) => Ordering::Less,
        (None, None) => Ordering::Equal,
    }
}

fn compare_priority(left: &Candidate, right: &Candidate) -> Ordering {
    left.priority()
        .cmp(&right.priority())
        .then_with(|| left.provider_id().cmp(right.provider_id()))
}

#[cfg(test)]
mod tests {
    use rand::SeedableRng;
    use rand::rngs::StdRng;

    use super::*;

    fn candidate(
        provider_id: &str,
        price: i64,
        priority: u8,
        throughput: Option<f64>,
        latency: Option<f64>,
        healthy: bool,
    ) -> Candidate {
        let measurements = throughput.zip(latency).map(
            |(throughput_tokens_per_second, time_to_first_token_seconds)| ModelMeasurements {
                throughput_tokens_per_second,
                time_to_first_token_seconds,
            },
        );
        Candidate::new(
            provider_id,
            Decimal::from(price),
            Decimal::ZERO,
            priority,
            measurements,
            healthy,
        )
        .unwrap()
    }

    fn config(price_ceiling: i64, hysteresis: f64, max_share: &str) -> PolicyConfig {
        PolicyConfig::new(
            Decimal::from(price_ceiling),
            hysteresis,
            max_share.parse().unwrap(),
        )
        .unwrap()
    }

    #[test]
    fn balanced_distribution_matches_inverse_square_price_weights() {
        let candidates = vec![
            candidate("cheap", 1, 0, None, None, true),
            candidate("middle", 2, 0, None, None, true),
            candidate("expensive", 4, 0, None, None, true),
        ];
        let config = config(100, 0.1, "0.6");
        let share = ShareTracker::new(100).unwrap();
        let mut rng = StdRng::seed_from_u64(7);
        let mut selected = HashMap::<String, usize>::new();

        for _ in 0..10_000 {
            let provider_id =
                select_route(&candidates, Preference::Balanced, &config, &share, &mut rng)
                    .unwrap()
                    .provider_id()
                    .to_owned();
            *selected.entry(provider_id).or_default() += 1;
        }

        let expected_cheapest_share = 1.0 / (1.0 + 0.25 + 0.0625);
        let actual_cheapest_share = selected["cheap"] as f64 / 10_000.0;
        assert!(
            (actual_cheapest_share - expected_cheapest_share).abs() <= 0.03,
            "expected {expected_cheapest_share:.4}, got {actual_cheapest_share:.4}"
        );
    }

    #[test]
    fn default_price_ceiling_excludes_a_faster_expensive_candidate() {
        let candidates = vec![
            candidate("within-ceiling", 10, 0, Some(20.0), Some(1.0), true),
            candidate("over-ceiling", 11, 0, Some(2_000.0), Some(0.1), true),
        ];
        let config = config(10, 0.1, "1");
        let share = ShareTracker::new(100).unwrap();
        let mut rng = StdRng::seed_from_u64(11);

        for _ in 0..1_000 {
            assert_eq!(
                select_route(&candidates, Preference::Default, &config, &share, &mut rng,)
                    .unwrap()
                    .provider_id(),
                "within-ceiling"
            );
        }
    }

    #[test]
    fn default_rolling_share_never_exceeds_the_full_window_quota() {
        let candidates = vec![
            candidate("dominant", 1, 0, Some(10_000.0), Some(1.0), true),
            candidate("alternate", 1, 1, Some(1.0), Some(1.0), true),
        ];
        let config = config(10, 0.0, "0.6");
        let mut share = ShareTracker::new(100).unwrap();
        let mut rng = StdRng::seed_from_u64(13);

        for _ in 0..10_000 {
            let selected =
                select_route(&candidates, Preference::Default, &config, &share, &mut rng).unwrap();
            share.record(selected.provider_id());
            if share.history.len() == share.capacity {
                assert!(share.share("dominant") <= "0.6".parse().unwrap());
            }
        }

        assert_eq!(share.share("dominant"), "0.6".parse().unwrap());
    }

    #[test]
    fn default_hysteresis_suppresses_noise_but_preserves_a_material_advantage() {
        let barely_faster = vec![
            candidate("incumbent", 1, 0, Some(100.0), Some(1.0), true),
            candidate("challenger", 1, 1, Some(101.0), Some(1.0), true),
        ];
        let materially_faster = vec![
            candidate("incumbent", 1, 0, Some(100.0), Some(1.0), true),
            candidate("challenger", 1, 1, Some(130.0), Some(1.0), true),
        ];
        let config = config(10, 0.1, "1");
        let mut share = ShareTracker::new(100).unwrap();
        for _ in 0..60 {
            share.record("incumbent");
        }
        for _ in 0..40 {
            share.record("challenger");
        }

        let challenger_share = |candidates: &[Candidate], seed| {
            let mut rng = StdRng::seed_from_u64(seed);
            let selected = (0..10_000)
                .filter(|_| {
                    select_route(candidates, Preference::Default, &config, &share, &mut rng)
                        .unwrap()
                        .provider_id()
                        == "challenger"
                })
                .count();
            selected as f64 / 10_000.0
        };

        let noise_share = challenger_share(&barely_faster, 17);
        let material_share = challenger_share(&materially_faster, 19);
        assert!((noise_share - 0.5).abs() <= 0.03, "{noise_share}");
        assert!(material_share > 0.53, "{material_share}");
    }

    #[test]
    fn strict_sorts_return_the_best_healthy_candidate_with_stable_ties() {
        let candidates = vec![
            candidate("unhealthy", 0, 0, Some(10_000.0), Some(0.0), false),
            candidate("price", 1, 2, Some(10.0), Some(3.0), true),
            candidate("throughput", 2, 1, Some(200.0), Some(2.0), true),
            candidate("latency", 3, 0, Some(50.0), Some(0.1), true),
            candidate("unmeasured", 1, 1, None, None, true),
        ];
        let config = config(100, 0.1, "0.6");
        let share = ShareTracker::new(100).unwrap();
        let mut rng = StdRng::seed_from_u64(23);

        for (preference, expected_provider) in [
            (Preference::Price, "unmeasured"),
            (Preference::Throughput, "throughput"),
            (Preference::Latency, "latency"),
        ] {
            assert_eq!(
                select_route(&candidates, preference, &config, &share, &mut rng)
                    .unwrap()
                    .provider_id(),
                expected_provider
            );
        }

        let tied = vec![
            candidate("beta", 1, 1, Some(100.0), Some(1.0), true),
            candidate("alpha", 1, 1, Some(100.0), Some(1.0), true),
            candidate("later-priority", 1, 2, Some(100.0), Some(1.0), true),
        ];
        for preference in [
            Preference::Price,
            Preference::Throughput,
            Preference::Latency,
        ] {
            assert_eq!(
                select_route(&tied, preference, &config, &share, &mut rng)
                    .unwrap()
                    .provider_id(),
                "alpha"
            );
        }
    }

    #[test]
    fn free_balanced_candidates_are_finite_and_paid_candidates_never_win() {
        let candidates = vec![
            candidate("free-a", 0, 0, None, None, true),
            candidate("free-b", 0, 1, None, None, true),
            candidate("paid", 1, 0, None, None, true),
        ];
        let config = config(100, 0.1, "0.6");
        let share = ShareTracker::new(100).unwrap();
        let mut rng = StdRng::seed_from_u64(29);
        let selected: Vec<_> = (0..1_000)
            .map(|_| {
                select_route(&candidates, Preference::Balanced, &config, &share, &mut rng)
                    .unwrap()
                    .provider_id()
            })
            .collect();

        assert!(selected.contains(&"free-a"));
        assert!(selected.contains(&"free-b"));
        assert!(!selected.contains(&"paid"));
    }

    #[test]
    fn cold_start_uses_priority_and_empty_or_unhealthy_inputs_return_none() {
        let candidates = vec![
            candidate("later", 1, 2, None, None, true),
            candidate("first", 1, 1, None, None, true),
        ];
        let unhealthy = vec![candidate("down", 1, 0, Some(100.0), Some(1.0), false)];
        let config = config(100, 0.1, "0.6");
        let share = ShareTracker::new(100).unwrap();
        let mut rng = StdRng::seed_from_u64(31);

        assert_eq!(
            select_route(&candidates, Preference::Default, &config, &share, &mut rng,)
                .unwrap()
                .provider_id(),
            "first"
        );
        assert_eq!(
            select_route(&[], Preference::Default, &config, &share, &mut rng),
            None
        );
        assert_eq!(
            select_route(&unhealthy, Preference::Default, &config, &share, &mut rng,),
            None
        );
    }

    #[test]
    fn share_history_evicts_old_selections_and_invalid_inputs_are_rejected() {
        let mut share = ShareTracker::new(3).unwrap();
        share.record("a");
        share.record("b");
        share.record("a");
        share.record("b");

        assert_eq!(
            share.share("a"),
            "0.3333333333333333333333333333".parse().unwrap()
        );
        assert_eq!(
            share.share("b"),
            "0.6666666666666666666666666667".parse().unwrap()
        );
        assert_eq!(share.incumbent_provider_id(), Some("b"));
        assert_eq!(ShareTracker::new(0), Err(ShareTrackerError::ZeroCapacity));
        assert_eq!(
            PolicyConfig::new(Decimal::from(-1), 0.1, Decimal::ONE),
            Err(PolicyConfigError::NegativePriceCeiling)
        );
        assert_eq!(
            PolicyConfig::new(Decimal::ONE, f64::NAN, Decimal::ONE),
            Err(PolicyConfigError::InvalidHysteresis)
        );
        assert_eq!(
            PolicyConfig::new(Decimal::ONE, 0.1, Decimal::ZERO),
            Err(PolicyConfigError::InvalidMaxShare)
        );
        assert_eq!(
            Candidate::new("bad", Decimal::from(-1), Decimal::ZERO, 0, None, true,),
            Err(CandidateError::NegativeInputPrice)
        );
        assert_eq!(
            Candidate::new("huge", Decimal::MAX, Decimal::ONE, 0, None, true),
            Err(CandidateError::PriceOverflow)
        );
    }
}
