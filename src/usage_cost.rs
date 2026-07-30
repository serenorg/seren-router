// ABOUTME: Resolves the served provider's reviewed prices for one request.
// ABOUTME: Injects only exact provider cost into OpenAI-compatible usage.cost.

use std::fmt;
use std::str::FromStr;

use serde_json::{Number, Value};

use crate::attribution::ServedProvider;
use crate::pricing::{PriceTable, ProviderPrices, Usage, normalize_cost_usd, provider_cost_usd};

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct CostedUsage {
    pub(crate) response_id: Option<String>,
    pub(crate) usage: Usage,
    pub(crate) cost_usd: rust_decimal::Decimal,
}

pub(crate) struct CostedResponse {
    pub(crate) body: Vec<u8>,
    pub(crate) usage: CostedUsage,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ProviderReportedCostPolicy {
    UsageCostOnly,
    DeepInfraEstimatedCost,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CostOmission {
    InvalidJson,
    MissingUsage,
    InvalidUsage,
    InvalidProviderCost,
    UnknownPrice,
    UnresolvedProviderCost,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PublicResponseRejection {
    InvalidJson,
    EmbeddedError,
    UnrecognizedShape,
}

impl fmt::Display for PublicResponseRejection {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidJson => formatter.write_str("response is not valid JSON"),
            Self::EmbeddedError => formatter.write_str("successful response contains an error"),
            Self::UnrecognizedShape => {
                formatter.write_str("response is not an OpenAI completion object")
            }
        }
    }
}

impl fmt::Display for CostOmission {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidJson => formatter.write_str("response is not valid JSON"),
            Self::MissingUsage => formatter.write_str("response usage object is missing"),
            Self::InvalidUsage => formatter.write_str("response token usage is invalid"),
            Self::InvalidProviderCost => {
                formatter.write_str("provider-reported usage cost is invalid")
            }
            Self::UnknownPrice => formatter.write_str("provider/model price is unknown"),
            Self::UnresolvedProviderCost => {
                formatter.write_str("provider cost requires exact cached-token usage")
            }
        }
    }
}

pub(crate) fn prices_for_request<'a>(
    requested_model: &str,
    served_provider: &ServedProvider,
    price_table: &'a PriceTable,
) -> Result<&'a ProviderPrices, CostOmission> {
    let canonical_slug = canonical_slug(requested_model, served_provider.as_str());
    price_table
        .get(served_provider.as_str(), canonical_slug)
        .ok_or(CostOmission::UnknownPrice)
}

pub(crate) fn inject_usage_cost(
    body: &[u8],
    requested_model: &str,
    served_provider: &ServedProvider,
    price_table: &PriceTable,
) -> Result<CostedResponse, CostOmission> {
    let mut response: Value =
        serde_json::from_slice(body).map_err(|_| CostOmission::InvalidJson)?;
    strip_provider_specific_fields(&mut response);
    let prices = prices_for_request(requested_model, served_provider, price_table)?;
    let usage = inject_usage_cost_value_with_policy(
        &mut response,
        prices,
        provider_reported_cost_policy(served_provider),
    )?;
    let body = serde_json::to_vec(&response).map_err(|_| CostOmission::InvalidJson)?;

    Ok(CostedResponse { body, usage })
}

pub(crate) fn inject_usage_cost_value_with_policy(
    response: &mut Value,
    prices: &ProviderPrices,
    provider_cost_policy: ProviderReportedCostPolicy,
) -> Result<CostedUsage, CostOmission> {
    let response_id = response
        .get("id")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let usage = response
        .get_mut("usage")
        .and_then(Value::as_object_mut)
        .ok_or(CostOmission::MissingUsage)?;
    let token_usage = Usage {
        prompt_tokens: usage
            .get("prompt_tokens")
            .and_then(Value::as_u64)
            .ok_or(CostOmission::InvalidUsage)?,
        completion_tokens: usage
            .get("completion_tokens")
            .and_then(Value::as_u64)
            .ok_or(CostOmission::InvalidUsage)?,
    };
    let cached_prompt_tokens = usage
        .get("prompt_tokens_details")
        .and_then(Value::as_object)
        .and_then(|details| details.get("cached_tokens"))
        .and_then(Value::as_u64);
    let provider_cost = match reported_cost(usage.get("cost"))? {
        Some(cost) => cost,
        None => {
            let estimated_cost =
                if provider_cost_policy == ProviderReportedCostPolicy::DeepInfraEstimatedCost {
                    reported_cost(usage.get("estimated_cost"))?
                } else {
                    None
                };
            estimated_cost
                .or_else(|| provider_cost_usd(prices, &token_usage, cached_prompt_tokens))
                .ok_or(CostOmission::UnresolvedProviderCost)?
        }
    };
    let cost_number = Number::from_str(&provider_cost.to_string())
        .expect("Decimal always serializes as a JSON number");
    usage.insert("cost".to_owned(), Value::Number(cost_number));
    if provider_cost_policy == ProviderReportedCostPolicy::DeepInfraEstimatedCost {
        usage.remove("estimated_cost");
    }

    Ok(CostedUsage {
        response_id,
        usage: token_usage,
        cost_usd: provider_cost,
    })
}

pub(crate) fn provider_reported_cost_policy(
    served_provider: &ServedProvider,
) -> ProviderReportedCostPolicy {
    match served_provider.as_str() {
        "deepinfra" | "deepinfra-glm" => ProviderReportedCostPolicy::DeepInfraEstimatedCost,
        _ => ProviderReportedCostPolicy::UsageCostOnly,
    }
}

fn reported_cost(value: Option<&Value>) -> Result<Option<rust_decimal::Decimal>, CostOmission> {
    let Some(value) = value else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }
    let Value::Number(cost) = value else {
        return Err(CostOmission::InvalidProviderCost);
    };
    let serialized = cost.to_string();
    let cost = rust_decimal::Decimal::from_str(&serialized)
        .or_else(|_| rust_decimal::Decimal::from_scientific(&serialized))
        .map_err(|_| CostOmission::InvalidProviderCost)?;
    normalize_cost_usd(cost)
        .map(Some)
        .ok_or(CostOmission::InvalidProviderCost)
}

pub(crate) fn sanitize_public_completion_value(
    response: &mut Value,
    canonical_model: &str,
) -> Result<(), PublicResponseRejection> {
    let response = response
        .as_object_mut()
        .ok_or(PublicResponseRejection::UnrecognizedShape)?;
    if response
        .get("error")
        .is_some_and(|upstream_error| !upstream_error.is_null())
    {
        return Err(PublicResponseRejection::EmbeddedError);
    }
    let object = response.get("object").and_then(Value::as_str);
    if response
        .get("id")
        .and_then(Value::as_str)
        .is_none_or(str::is_empty)
        || !object.is_some_and(|object| {
            matches!(
                object,
                "chat.completion" | "chat.completion.chunk" | "text_completion"
            )
        })
        || !response.get("choices").is_some_and(Value::is_array)
    {
        return Err(PublicResponseRejection::UnrecognizedShape);
    }

    for value in response.values_mut() {
        strip_provider_specific_fields(value);
    }
    response.retain(|field, _| {
        matches!(
            field.as_str(),
            "id" | "object"
                | "created"
                | "model"
                | "choices"
                | "usage"
                | "service_tier"
                | "system_fingerprint"
        )
    });
    response.insert(
        "model".to_owned(),
        Value::String(canonical_model.to_owned()),
    );
    Ok(())
}

pub(crate) fn strip_provider_specific_fields(value: &mut Value) -> bool {
    match value {
        Value::Object(object) => {
            let mut removed = object.remove("provider_specific_fields").is_some();
            for child in object.values_mut() {
                removed |= strip_provider_specific_fields(child);
            }
            removed
        }
        Value::Array(array) => {
            let mut removed = false;
            for child in array {
                removed |= strip_provider_specific_fields(child);
            }
            removed
        }
        _ => false,
    }
}

pub(crate) fn canonical_slug<'a>(requested_model: &'a str, served_provider: &str) -> &'a str {
    requested_model
        .strip_prefix(served_provider)
        .and_then(|suffix| suffix.strip_prefix('/'))
        .filter(|slug| !slug.is_empty())
        .unwrap_or(requested_model)
}
