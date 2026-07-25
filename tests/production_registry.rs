// ABOUTME: Pins the reviewed production OpenRouter fallback model coverage and prices.
// ABOUTME: Detects accidental provider enablement, alias drift, or unreviewed price changes.

use rust_decimal::Decimal;
use seren_router::pricing::{ModelPrices, PriceTable};
use seren_router::registry::Registry;

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
        Some(&ModelPrices {
            input_price_per_mtok: Decimal::new(13, 2),
            output_price_per_mtok: Decimal::new(40, 2),
        })
    );
    assert!(prices.get("fireworks", "unknown").is_none());
}
