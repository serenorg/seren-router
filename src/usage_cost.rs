// ABOUTME: Resolves reviewed provider-cost and customer sell prices for one request.
// ABOUTME: Injects only the exact sell subtotal into OpenAI-compatible usage.cost.

use std::fmt;
use std::str::FromStr;

use serde_json::{Number, Value};

use crate::attribution::ServedProvider;
use crate::pricing::{BillingPrices, PriceTable, Usage, cost_usd};

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct CostedUsage {
    pub(crate) response_id: Option<String>,
    pub(crate) usage: Usage,
    pub(crate) provider_cost_usd: rust_decimal::Decimal,
    pub(crate) sell_price_usd: rust_decimal::Decimal,
}

pub(crate) struct CostedResponse {
    pub(crate) body: Vec<u8>,
    pub(crate) usage: CostedUsage,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CostOmission {
    InvalidJson,
    MissingUsage,
    InvalidUsage,
    UnknownPrice,
}

impl fmt::Display for CostOmission {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidJson => formatter.write_str("response is not valid JSON"),
            Self::MissingUsage => formatter.write_str("response usage object is missing"),
            Self::InvalidUsage => formatter.write_str("response token usage is invalid"),
            Self::UnknownPrice => formatter.write_str("provider/model price is unknown"),
        }
    }
}

pub(crate) fn prices_for_request<'a>(
    requested_model: &str,
    served_provider: &ServedProvider,
    price_table: &'a PriceTable,
) -> Result<&'a BillingPrices, CostOmission> {
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
    let prices = prices_for_request(requested_model, served_provider, price_table)?;
    let usage = inject_usage_cost_value(&mut response, prices)?;
    let body = serde_json::to_vec(&response).map_err(|_| CostOmission::InvalidJson)?;

    Ok(CostedResponse { body, usage })
}

pub(crate) fn inject_usage_cost_value(
    response: &mut Value,
    prices: &BillingPrices,
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
    let provider_cost = cost_usd(&prices.provider_cost, &token_usage);
    let sell_price = cost_usd(&prices.sell_price, &token_usage);
    let cost_number = Number::from_str(&sell_price.to_string())
        .expect("Decimal always serializes as a JSON number");
    usage.insert("cost".to_owned(), Value::Number(cost_number));

    Ok(CostedUsage {
        response_id,
        usage: token_usage,
        provider_cost_usd: provider_cost,
        sell_price_usd: sell_price,
    })
}

pub(crate) fn canonical_slug<'a>(requested_model: &'a str, served_provider: &str) -> &'a str {
    requested_model
        .strip_prefix(served_provider)
        .and_then(|suffix| suffix.strip_prefix('/'))
        .filter(|slug| !slug.is_empty())
        .unwrap_or(requested_model)
}
