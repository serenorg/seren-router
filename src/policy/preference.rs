// ABOUTME: Parses OpenRouter-compatible routing preferences from completion requests.
// ABOUTME: Normalizes model shortcuts without changing unrelated request fields.

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::Value;
use thiserror::Error;

const NITRO_SUFFIX: &str = ":nitro";
const FLOOR_SUFFIX: &str = ":floor";

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Preference {
    #[default]
    Default,
    Balanced,
    Price,
    Throughput,
    Latency,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RequestPreference {
    pub canonical_model: String,
    pub preference: Preference,
}

#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum PreferenceError {
    #[error("model must be a string")]
    InvalidModel,
    #[error("provider.sort must be one of: price, throughput, latency")]
    InvalidSort,
}

impl IntoResponse for PreferenceError {
    fn into_response(self) -> Response {
        (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": {
                    "message": self.to_string()
                }
            })),
        )
            .into_response()
    }
}

pub fn parse_request(request: &mut Value) -> Result<RequestPreference, PreferenceError> {
    let model = request
        .get("model")
        .and_then(Value::as_str)
        .ok_or(PreferenceError::InvalidModel)?;
    let (canonical_model, suffix_preference) = if let Some(model) = model.strip_suffix(NITRO_SUFFIX)
    {
        (model, Preference::Throughput)
    } else if let Some(model) = model.strip_suffix(FLOOR_SUFFIX) {
        (model, Preference::Price)
    } else {
        (model, Preference::Default)
    };

    let explicit_preference = request
        .pointer("/provider/sort")
        .map(parse_sort)
        .transpose()?;
    let parsed = RequestPreference {
        canonical_model: canonical_model.to_owned(),
        preference: explicit_preference.unwrap_or(suffix_preference),
    };

    request["model"] = Value::String(parsed.canonical_model.clone());

    Ok(parsed)
}

fn parse_sort(sort: &Value) -> Result<Preference, PreferenceError> {
    match sort.as_str() {
        Some("price") => Ok(Preference::Price),
        Some("throughput") => Ok(Preference::Throughput),
        Some("latency") => Ok(Preference::Latency),
        _ => Err(PreferenceError::InvalidSort),
    }
}

#[cfg(test)]
mod tests {
    use axum::body::to_bytes;
    use serde_json::json;

    use super::*;

    #[test]
    fn routing_preferences_follow_suffix_and_body_precedence() {
        struct Case {
            name: &'static str,
            request: Value,
            expected: Result<RequestPreference, PreferenceError>,
        }

        let cases = [
            Case {
                name: "nitro suffix",
                request: json!({"model": "anthropic/claude-opus-5:nitro"}),
                expected: Ok(RequestPreference {
                    canonical_model: "anthropic/claude-opus-5".to_owned(),
                    preference: Preference::Throughput,
                }),
            },
            Case {
                name: "floor suffix",
                request: json!({"model": "anthropic/claude-opus-5:floor"}),
                expected: Ok(RequestPreference {
                    canonical_model: "anthropic/claude-opus-5".to_owned(),
                    preference: Preference::Price,
                }),
            },
            Case {
                name: "explicit price",
                request: json!({
                    "model": "anthropic/claude-opus-5",
                    "provider": {"sort": "price"}
                }),
                expected: Ok(RequestPreference {
                    canonical_model: "anthropic/claude-opus-5".to_owned(),
                    preference: Preference::Price,
                }),
            },
            Case {
                name: "explicit throughput",
                request: json!({
                    "model": "anthropic/claude-opus-5",
                    "provider": {"sort": "throughput"}
                }),
                expected: Ok(RequestPreference {
                    canonical_model: "anthropic/claude-opus-5".to_owned(),
                    preference: Preference::Throughput,
                }),
            },
            Case {
                name: "explicit latency",
                request: json!({
                    "model": "anthropic/claude-opus-5",
                    "provider": {"sort": "latency"}
                }),
                expected: Ok(RequestPreference {
                    canonical_model: "anthropic/claude-opus-5".to_owned(),
                    preference: Preference::Latency,
                }),
            },
            Case {
                name: "explicit body preference overrides suffix",
                request: json!({
                    "model": "anthropic/claude-opus-5:nitro",
                    "provider": {"sort": "latency"}
                }),
                expected: Ok(RequestPreference {
                    canonical_model: "anthropic/claude-opus-5".to_owned(),
                    preference: Preference::Latency,
                }),
            },
            Case {
                name: "no preference",
                request: json!({"model": "anthropic/claude-opus-5"}),
                expected: Ok(RequestPreference {
                    canonical_model: "anthropic/claude-opus-5".to_owned(),
                    preference: Preference::Default,
                }),
            },
            Case {
                name: "unknown sort",
                request: json!({
                    "model": "anthropic/claude-opus-5",
                    "provider": {"sort": "quality"}
                }),
                expected: Err(PreferenceError::InvalidSort),
            },
            Case {
                name: "non-string sort",
                request: json!({
                    "model": "anthropic/claude-opus-5",
                    "provider": {"sort": 7}
                }),
                expected: Err(PreferenceError::InvalidSort),
            },
            Case {
                name: "advanced object sort",
                request: json!({
                    "model": "anthropic/claude-opus-5",
                    "provider": {
                        "sort": {"by": "throughput", "partition": "model"}
                    }
                }),
                expected: Err(PreferenceError::InvalidSort),
            },
        ];

        for mut case in cases {
            assert_eq!(
                parse_request(&mut case.request),
                case.expected,
                "{}",
                case.name
            );
            if let Ok(expected) = &case.expected {
                assert_eq!(
                    case.request["model"], expected.canonical_model,
                    "{}",
                    case.name
                );
            }
        }
    }

    #[test]
    fn normalization_preserves_reasoning_and_unrelated_request_fields() {
        let mut request = json!({
            "model": "anthropic/claude-opus-5:nitro",
            "messages": [{"role": "user", "content": "hello"}],
            "reasoning": {"effort": "high"},
            "tools": [{"type": "function", "function": {"name": "lookup"}}],
            "provider": {"sort": "price", "ignore": ["provider-a"]},
            "custom": {"arbitrary_precision": 123456789012345678901234567890_u128}
        });
        let original = request.clone();

        let parsed = parse_request(&mut request).unwrap();

        assert_eq!(parsed.preference, Preference::Price);
        assert_eq!(request["model"], "anthropic/claude-opus-5");
        for field in ["messages", "reasoning", "tools", "provider", "custom"] {
            assert_eq!(request[field], original[field], "{field}");
        }
    }

    #[tokio::test]
    async fn invalid_sort_has_clear_bad_request_response() {
        let response = PreferenceError::InvalidSort.into_response();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        assert_eq!(
            serde_json::from_slice::<Value>(&body).unwrap(),
            json!({
                "error": {
                    "message": "provider.sort must be one of: price, throughput, latency"
                }
            })
        );
    }
}
