// ABOUTME: Tracks smoothed provider/model throughput and time-to-first-token observations.
// ABOUTME: Keeps routing measurements shared in memory without reading the wall clock.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use thiserror::Error;

// Fixed by the M5 policy; changing smoothing is an operations tuning decision.
const EWMA_ALPHA: f64 = 0.2;

type MeasurementsByModel = HashMap<String, ModelMeasurements>;
type MeasurementsByProvider = HashMap<String, MeasurementsByModel>;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ModelMeasurements {
    pub throughput_tokens_per_second: f64,
    pub time_to_first_token_seconds: f64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Observation {
    pub completion_tokens: u64,
    pub stream_duration: Duration,
    pub time_to_first_token: Duration,
}

#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum MeasurementError {
    #[error("stream duration must be greater than zero")]
    ZeroStreamDuration,
}

#[derive(Clone, Debug, Default)]
pub struct MeasurementStore {
    measurements: Arc<RwLock<MeasurementsByProvider>>,
}

impl MeasurementStore {
    pub fn observe(
        &self,
        provider_id: &str,
        canonical_slug: &str,
        observation: Observation,
    ) -> Result<ModelMeasurements, MeasurementError> {
        if observation.stream_duration.is_zero() {
            return Err(MeasurementError::ZeroStreamDuration);
        }

        let sample = ModelMeasurements {
            throughput_tokens_per_second: observation.completion_tokens as f64
                / observation.stream_duration.as_secs_f64(),
            time_to_first_token_seconds: observation.time_to_first_token.as_secs_f64(),
        };
        let mut measurements = self
            .measurements
            .write()
            .expect("measurement store lock poisoned");
        let current = measurements
            .entry(provider_id.to_owned())
            .or_default()
            .entry(canonical_slug.to_owned())
            .and_modify(|current| {
                current.throughput_tokens_per_second = ewma(
                    current.throughput_tokens_per_second,
                    sample.throughput_tokens_per_second,
                );
                current.time_to_first_token_seconds = ewma(
                    current.time_to_first_token_seconds,
                    sample.time_to_first_token_seconds,
                );
            })
            .or_insert(sample);

        Ok(*current)
    }

    pub fn get(&self, provider_id: &str, canonical_slug: &str) -> Option<ModelMeasurements> {
        self.measurements
            .read()
            .expect("measurement store lock poisoned")
            .get(provider_id)
            .and_then(|provider| provider.get(canonical_slug))
            .copied()
    }
}

fn ewma(previous: f64, sample: f64) -> f64 {
    EWMA_ALPHA * sample + (1.0 - EWMA_ALPHA) * previous
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ewma_converges_to_exact_values_for_fixed_observations() {
        let measurements = MeasurementStore::default();

        assert_eq!(
            measurements.observe(
                "provider-a",
                "vendor/model",
                Observation {
                    completion_tokens: 100,
                    stream_duration: Duration::from_secs(1),
                    time_to_first_token: Duration::from_secs(1),
                },
            ),
            Ok(ModelMeasurements {
                throughput_tokens_per_second: 100.0,
                time_to_first_token_seconds: 1.0,
            })
        );
        assert_eq!(
            measurements.observe(
                "provider-a",
                "vendor/model",
                Observation {
                    completion_tokens: 150,
                    stream_duration: Duration::from_secs(2),
                    time_to_first_token: Duration::from_millis(2_250),
                },
            ),
            Ok(ModelMeasurements {
                throughput_tokens_per_second: 95.0,
                time_to_first_token_seconds: 1.25,
            })
        );
        assert_eq!(
            measurements.observe(
                "provider-a",
                "vendor/model",
                Observation {
                    completion_tokens: 210,
                    stream_duration: Duration::from_secs(3),
                    time_to_first_token: Duration::from_millis(2_500),
                },
            ),
            Ok(ModelMeasurements {
                throughput_tokens_per_second: 90.0,
                time_to_first_token_seconds: 1.5,
            })
        );
        assert_eq!(
            measurements.clone().get("provider-a", "vendor/model"),
            Some(ModelMeasurements {
                throughput_tokens_per_second: 90.0,
                time_to_first_token_seconds: 1.5,
            })
        );
    }

    #[test]
    fn unseen_pairs_are_isolated_and_invalid_observations_are_not_stored() {
        let measurements = MeasurementStore::default();
        measurements
            .observe(
                "provider-a",
                "vendor/model-a",
                Observation {
                    completion_tokens: 50,
                    stream_duration: Duration::from_secs(1),
                    time_to_first_token: Duration::from_millis(500),
                },
            )
            .unwrap();

        assert_eq!(measurements.get("provider-a", "vendor/model-b"), None);
        assert_eq!(measurements.get("provider-b", "vendor/model-a"), None);
        assert_eq!(
            measurements.observe(
                "provider-b",
                "vendor/model-b",
                Observation {
                    completion_tokens: 50,
                    stream_duration: Duration::ZERO,
                    time_to_first_token: Duration::from_millis(500),
                },
            ),
            Err(MeasurementError::ZeroStreamDuration)
        );
        assert_eq!(measurements.get("provider-b", "vendor/model-b"), None);
    }
}
