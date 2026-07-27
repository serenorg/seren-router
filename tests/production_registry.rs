// ABOUTME: Pins the reviewed production OpenRouter fallback model coverage and prices.
// ABOUTME: Detects accidental provider enablement, alias drift, or unreviewed price changes.

use rust_decimal::Decimal;
use seren_router::pricing::{BillingPrices, ModelPrices, PriceTable, Usage, cost_usd};
use seren_router::registry::Registry;
use seren_router::routing_profile::RoutingProfile;

#[test]
fn production_registry_contains_only_the_reviewed_openrouter_fallback() {
    let registry: Registry =
        serde_yaml::from_str(include_str!("../registry/providers.yaml")).unwrap();
    registry.validate().unwrap();

    let enabled: Vec<_> = registry
        .providers
        .iter()
        .filter(|provider| provider.enabled)
        .collect();
    assert_eq!(enabled.len(), 1);
    let openrouter = enabled[0];
    assert_eq!(openrouter.id, "openrouter");
    assert_eq!(openrouter.base_url, "https://openrouter.ai/api/v1");
    assert_eq!(openrouter.secret_env, "SEREN_ROUTER_KEY_OPENROUTER");
    assert_eq!(openrouter.priority, u8::MAX);
    assert_eq!(
        openrouter.profiles,
        [RoutingProfile::Production, RoutingProfile::Beta]
            .into_iter()
            .collect()
    );
    let deepinfra = registry
        .providers
        .iter()
        .find(|provider| provider.id == "deepinfra")
        .unwrap();
    assert!(!deepinfra.enabled);
    assert_eq!(
        deepinfra.profiles,
        [RoutingProfile::Beta].into_iter().collect()
    );
    assert_eq!(deepinfra.base_url, "https://api.deepinfra.com/v1/openai");
    assert_eq!(deepinfra.secret_env, "SEREN_ROUTER_KEY_DEEPINFRA");
    assert_eq!(deepinfra.priority, 0);
    assert_eq!(deepinfra.models.len(), 1);
    let deepinfra_llama = &deepinfra.models[0];
    assert_eq!(deepinfra_llama.slug, "meta-llama/llama-3.3-70b-instruct");
    assert_eq!(
        deepinfra_llama.provider_model_id,
        "meta-llama/Llama-3.3-70B-Instruct-Turbo"
    );
    assert_eq!(deepinfra_llama.context_length, 131_072);
    assert_eq!(deepinfra_llama.input_price_per_mtok, Decimal::new(10, 2));
    assert_eq!(deepinfra_llama.output_price_per_mtok, Decimal::new(32, 2));

    let actual: Vec<_> = openrouter
        .models
        .iter()
        .map(|model| {
            (
                model.slug.as_str(),
                model.provider_model_id.as_str(),
                model.context_length,
                model.input_price_per_mtok,
                model.output_price_per_mtok,
            )
        })
        .collect();
    assert_eq!(
        actual,
        [
            (
                "openai/gpt-5.6-luna",
                "openai/gpt-5.6-luna",
                1_050_000,
                Decimal::new(100, 2),
                Decimal::new(600, 2),
            ),
            (
                "anthropic/claude-sonnet-5",
                "anthropic/claude-sonnet-5",
                1_000_000,
                Decimal::new(200, 2),
                Decimal::new(1000, 2),
            ),
            (
                "google/gemini-3.6-flash",
                "google/gemini-3.6-flash",
                1_048_576,
                Decimal::new(150, 2),
                Decimal::new(750, 2),
            ),
            (
                "meta-llama/llama-3.3-70b-instruct",
                "meta-llama/llama-3.3-70b-instruct",
                131_072,
                Decimal::new(13, 2),
                Decimal::new(40, 2),
            ),
            (
                "deepseek/deepseek-v4-flash",
                "deepseek/deepseek-v4-flash",
                1_048_576,
                Decimal::new(938, 4),
                Decimal::new(1876, 4),
            ),
            (
                "z-ai/glm-5.2",
                "z-ai/glm-5.2",
                1_048_576,
                Decimal::new(70, 2),
                Decimal::new(220, 2),
            ),
            (
                "moonshotai/kimi-k3",
                "moonshotai/kimi-k3",
                1_048_576,
                Decimal::new(300, 2),
                Decimal::new(1500, 2),
            ),
        ]
    );

    let prices = PriceTable::from_registry(&registry).unwrap();
    assert_eq!(
        prices.get("openrouter", "meta-llama/llama-3.3-70b-instruct"),
        Some(&BillingPrices {
            provider_cost: ModelPrices {
                input_price_per_mtok: Decimal::new(13, 2),
                output_price_per_mtok: Decimal::new(40, 2),
            },
            sell_price: ModelPrices {
                input_price_per_mtok: Decimal::new(13, 2),
                output_price_per_mtok: Decimal::new(40, 2),
            },
        })
    );
    assert!(
        prices
            .get("deepinfra", "meta-llama/llama-3.3-70b-instruct")
            .is_none(),
        "disabled provider prices must not enter the live price table"
    );
}

#[test]
fn direct_and_fallback_routes_keep_one_reviewed_sell_price_and_separate_costs() {
    let mut registry: Registry =
        serde_yaml::from_str(include_str!("../registry/providers.yaml")).unwrap();
    registry
        .providers
        .iter_mut()
        .find(|provider| provider.id == "deepinfra")
        .unwrap()
        .enabled = true;
    let prices = PriceTable::from_registry(&registry).unwrap();
    let slug = "meta-llama/llama-3.3-70b-instruct";
    let fallback = prices.get("openrouter", slug).unwrap();
    let direct = prices.get("deepinfra", slug).unwrap();

    assert_eq!(fallback.sell_price, direct.sell_price);
    assert_eq!(
        direct.provider_cost,
        ModelPrices {
            input_price_per_mtok: Decimal::new(10, 2),
            output_price_per_mtok: Decimal::new(32, 2),
        }
    );
    assert_eq!(fallback.provider_cost, fallback.sell_price);

    let observed_usage = Usage {
        prompt_tokens: 3_962,
        completion_tokens: 214,
    };
    let sell_subtotal = cost_usd(&direct.sell_price, &observed_usage);
    let direct_provider_cost = cost_usd(&direct.provider_cost, &observed_usage);
    let fallback_provider_cost = cost_usd(&fallback.provider_cost, &observed_usage);

    assert_eq!(sell_subtotal.to_string(), "0.0006006600");
    assert_eq!(fallback_provider_cost, sell_subtotal);
    assert_eq!(direct_provider_cost.to_string(), "0.0004646800");
    assert_eq!(
        (sell_subtotal - direct_provider_cost).to_string(),
        "0.0001359800"
    );
}
