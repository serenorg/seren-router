// ABOUTME: Pins the reviewed production and isolated-beta provider contracts.
// ABOUTME: Detects routing, public-alias, sidecar, or exact-price drift before deployment.

use rust_decimal::Decimal;
use serde_json::json;
use serde_yaml::Value;
use seren_router::config::RoutingConfig;
use seren_router::policy::measurements::MeasurementStore;
use seren_router::policy::routing::{CompletionEndpoint, RouteRequestError, RoutingPolicy};
use seren_router::pricing::{
    BillingPrices, ModelPrices, PriceTable, Usage, cost_usd, provider_cost_usd,
};
use seren_router::registry::Registry;
use seren_router::routing_profile::RoutingProfile;
use seren_router::sidecar_config::{SidecarConfigOptions, compile};

#[test]
fn checked_registry_activates_reviewed_routes_and_isolates_beta_providers() {
    let registry = checked_registry();
    registry.validate().unwrap();

    let enabled: Vec<_> = registry
        .providers
        .iter()
        .filter(|provider| provider.enabled)
        .collect();
    assert_eq!(enabled.len(), 4);

    let production_enabled: Vec<_> = enabled
        .iter()
        .copied()
        .filter(|provider| provider.supports(RoutingProfile::Production))
        .collect();
    assert_eq!(production_enabled.len(), 2);
    let beta_enabled: Vec<_> = enabled
        .iter()
        .copied()
        .filter(|provider| provider.supports(RoutingProfile::Beta))
        .collect();
    assert_eq!(beta_enabled.len(), 4);
    let openrouter = production_enabled
        .iter()
        .copied()
        .find(|provider| provider.id == "openrouter")
        .expect("OpenRouter must remain the production fallback");
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

    let modal = registry
        .providers
        .iter()
        .find(|provider| provider.id == "modal")
        .expect("the checked registry must carry the reviewed Modal route");
    assert!(modal.enabled, "the approved Modal route must be enabled");
    assert_eq!(modal.display_name, "Modal");
    assert_eq!(
        modal.public_display_name.as_deref(),
        Some("Seren Inference")
    );
    assert_eq!(modal.public_tag.as_deref(), Some("seren"));
    assert_eq!(modal.base_url, "https://inference.us-west.modal.direct/v1");
    assert_eq!(modal.secret_env, "SEREN_ROUTER_KEY_MODAL");
    assert_eq!(modal.priority, 0);
    assert_eq!(
        modal.profiles,
        [RoutingProfile::Production, RoutingProfile::Beta]
            .into_iter()
            .collect()
    );
    assert!(modal.supports(RoutingProfile::Production));
    assert_eq!(modal.models.len(), 1);

    let modal_kimi = &modal.models[0];
    assert_eq!(modal_kimi.slug, "moonshotai/kimi-k3");
    assert_eq!(modal_kimi.name, "MoonshotAI Kimi K3");
    assert_eq!(modal_kimi.context_length, 1_048_576);
    assert_ne!(modal_kimi.provider_model_id, modal_kimi.slug);
    assert_modal_endpoint_hostname(&modal_kimi.provider_model_id);
    // Authenticated Modal dashboard evidence for the Seren workspace, 2026-07-29.
    const VERIFIED_MODAL_KIMI_ENDPOINT_HOSTNAME: &str =
        "serendb--ep-seren-kimi-k3-beta-server.us-west.modal.direct";
    assert_eq!(
        modal_kimi.provider_model_id,
        VERIFIED_MODAL_KIMI_ENDPOINT_HOSTNAME
    );
    assert_eq!(modal_kimi.input_price_per_mtok, Decimal::new(300, 2));
    assert_eq!(
        modal_kimi.cached_input_price_per_mtok,
        Some(Decimal::new(30, 2))
    );
    assert_eq!(modal_kimi.output_price_per_mtok, Decimal::new(1500, 2));
    assert_eq!(
        modal_kimi.request_constraints.endpoints,
        [CompletionEndpoint::Chat].into_iter().collect()
    );
    assert!(
        !modal_kimi.request_constraints.supports_streaming,
        "Modal streaming must remain ineligible while its SSE omits terminal usage"
    );
    let top_p = modal_kimi
        .request_constraints
        .top_p
        .as_ref()
        .expect("the Modal Kimi route must pin its supported top_p interval");
    assert_eq!(top_p.min, Decimal::new(95, 2));
    assert_eq!(top_p.max, Decimal::ONE);

    let deepinfra = registry
        .providers
        .iter()
        .find(|provider| provider.id == "deepinfra")
        .unwrap();
    assert!(deepinfra.enabled);
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
    assert_eq!(
        deepinfra_llama.request_constraints.endpoints,
        [CompletionEndpoint::Chat].into_iter().collect()
    );

    let blackbox = registry
        .providers
        .iter()
        .find(|provider| provider.id == "blackbox")
        .unwrap();
    assert!(blackbox.enabled);
    assert_eq!(
        blackbox.profiles,
        [RoutingProfile::Beta].into_iter().collect()
    );
    assert_eq!(blackbox.base_url, "https://api.blackbox.ai/v1");
    assert_eq!(blackbox.secret_env, "SEREN_ROUTER_KEY_BLACKBOX");
    assert_eq!(blackbox.priority, 0);
    assert_eq!(blackbox.models.len(), 1);
    let blackbox_glm = &blackbox.models[0];
    assert_eq!(blackbox_glm.slug, "z-ai/glm-5.2");
    assert_eq!(blackbox_glm.provider_model_id, "z-ai/glm-5.2");
    assert_eq!(blackbox_glm.context_length, 1_000_000);
    assert_eq!(blackbox_glm.input_price_per_mtok, Decimal::new(140, 2));
    assert_eq!(
        blackbox_glm.cached_input_price_per_mtok,
        Some(Decimal::new(14, 2))
    );
    assert_eq!(blackbox_glm.output_price_per_mtok, Decimal::new(440, 2));
    assert_eq!(
        blackbox_glm.request_constraints.endpoints,
        [CompletionEndpoint::Chat].into_iter().collect()
    );
    assert!(blackbox_glm.request_constraints.supports_streaming);

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
            provider_cached_input_price_per_mtok: None,
            sell_price: ModelPrices {
                input_price_per_mtok: Decimal::new(13, 2),
                output_price_per_mtok: Decimal::new(40, 2),
            },
        })
    );
    assert_eq!(
        prices.get("deepinfra", "meta-llama/llama-3.3-70b-instruct"),
        Some(&BillingPrices {
            provider_cost: ModelPrices {
                input_price_per_mtok: Decimal::new(10, 2),
                output_price_per_mtok: Decimal::new(32, 2),
            },
            provider_cached_input_price_per_mtok: None,
            sell_price: ModelPrices {
                input_price_per_mtok: Decimal::new(13, 2),
                output_price_per_mtok: Decimal::new(40, 2),
            },
        })
    );
    assert!(
        prices.get("modal", "moonshotai/kimi-k3").is_some(),
        "the enabled Modal beta route must enter the live price table"
    );
    assert!(
        prices.get("blackbox", "z-ai/glm-5.2").is_some(),
        "the enabled Blackbox beta route must enter the live price table"
    );
}

#[test]
fn kimi_cached_provider_cost_is_exact_and_provider_independent() {
    let registry = modal_beta_registry();
    let prices = PriceTable::from_registry(&registry).unwrap();
    let slug = "moonshotai/kimi-k3";
    let modal = prices.get("modal", slug).unwrap();
    let openrouter = prices.get("openrouter", slug).unwrap();

    assert_eq!(
        openrouter, modal,
        "OpenRouter and Modal publish the same reviewed Kimi K3 gross rates"
    );
    assert_eq!(
        modal,
        &BillingPrices {
            provider_cost: ModelPrices {
                input_price_per_mtok: Decimal::new(300, 2),
                output_price_per_mtok: Decimal::new(1500, 2),
            },
            provider_cached_input_price_per_mtok: Some(Decimal::new(30, 2)),
            sell_price: ModelPrices {
                input_price_per_mtok: Decimal::new(300, 2),
                output_price_per_mtok: Decimal::new(1500, 2),
            },
        }
    );

    let usage = Usage {
        prompt_tokens: 1_000,
        completion_tokens: 100,
    };
    assert_eq!(
        cost_usd(&modal.sell_price, &usage).to_string(),
        "0.0045000000"
    );
    for provider in [openrouter, modal] {
        assert_eq!(
            provider_cost_usd(provider, &usage, Some(600))
                .unwrap()
                .to_string(),
            "0.0028800000"
        );
        assert_eq!(provider_cost_usd(provider, &usage, None), None);
        assert_eq!(provider_cost_usd(provider, &usage, Some(1_001)), None);
    }
}

#[test]
fn routing_policy_selects_modal_in_both_profiles_and_rejects_provider_selection() {
    let registry = modal_beta_registry();
    let routing = RoutingPolicy::from_registry(
        &registry,
        RoutingConfig::new(Decimal::new(100, 0), 0.1, Decimal::ONE, 100).unwrap(),
        MeasurementStore::default(),
    )
    .unwrap();
    let mut production = json!({
        "model": "moonshotai/kimi-k3",
        "provider": {"sort": "price"}
    });
    let mut beta = production.clone();

    let production_decision = routing
        .route(RoutingProfile::Production, &mut production)
        .unwrap();
    let beta_decision = routing.route(RoutingProfile::Beta, &mut beta).unwrap();

    assert_eq!(production_decision.selected_provider, "modal");
    assert_eq!(production["model"], "modal/moonshotai/kimi-k3");
    assert_eq!(
        production_decision.fallback_model.as_deref(),
        Some("openrouter/moonshotai/kimi-k3")
    );
    assert_eq!(beta_decision.selected_provider, "modal");
    assert_eq!(beta["model"], "modal/moonshotai/kimi-k3");
    assert_eq!(
        beta_decision.fallback_model.as_deref(),
        Some("openrouter/moonshotai/kimi-k3")
    );

    for (endpoint, top_p) in [
        (CompletionEndpoint::Chat, json!(0.949)),
        (CompletionEndpoint::Legacy, json!(0.95)),
    ] {
        let mut constrained = json!({
            "model": "moonshotai/kimi-k3",
            "provider": {"sort": "price"},
            "top_p": top_p,
        });
        let decision = routing
            .route_for_endpoint(RoutingProfile::Beta, endpoint, &mut constrained)
            .unwrap();

        assert_eq!(decision.selected_provider, "openrouter");
        assert_eq!(constrained["model"], "openrouter/moonshotai/kimi-k3");
        assert_eq!(
            decision.fallback_model, None,
            "Modal was filtered, leaving no second compatible route"
        );
        assert!(!decision.has_alternatives);
    }

    let mut streaming = json!({
        "model": "moonshotai/kimi-k3",
        "provider": {"sort": "price"},
        "stream": true,
        "top_p": 0.95,
    });
    let decision = routing
        .route_for_endpoint(
            RoutingProfile::Beta,
            CompletionEndpoint::Chat,
            &mut streaming,
        )
        .unwrap();
    assert_eq!(decision.selected_provider, "openrouter");
    assert_eq!(streaming["model"], "openrouter/moonshotai/kimi-k3");
    assert_eq!(decision.fallback_model, None);
    assert!(!decision.has_alternatives);

    let mut concrete_provider =
        json!({"model": "modal/moonshotai/kimi-k3", "provider": {"sort": "price"}});
    assert_eq!(
        routing
            .route(RoutingProfile::Beta, &mut concrete_provider)
            .unwrap_err(),
        RouteRequestError::UnknownModel
    );

    for field in ["only", "ignore", "order"] {
        let mut override_request = json!({
            "model": "moonshotai/kimi-k3",
            "provider": {field: ["modal"]}
        });
        assert_eq!(
            routing
                .route(RoutingProfile::Beta, &mut override_request)
                .unwrap_err(),
            RouteRequestError::UnsupportedProviderOverride
        );
    }
}

#[test]
fn routing_policy_selects_deepinfra_only_for_beta_with_openrouter_fallback() {
    let registry = checked_registry();
    let routing = RoutingPolicy::from_registry(
        &registry,
        RoutingConfig::new(Decimal::new(100, 0), 0.1, Decimal::ONE, 100).unwrap(),
        MeasurementStore::default(),
    )
    .unwrap();
    let slug = "meta-llama/llama-3.3-70b-instruct";
    let mut production = json!({"model": slug, "provider": {"sort": "price"}});
    let mut beta = production.clone();

    let production_decision = routing
        .route(RoutingProfile::Production, &mut production)
        .unwrap();
    assert_eq!(production_decision.selected_provider, "openrouter");
    assert_eq!(
        production["model"],
        "openrouter/meta-llama/llama-3.3-70b-instruct"
    );
    assert_eq!(production_decision.fallback_model, None);
    assert!(!production_decision.has_alternatives);

    let beta_decision = routing.route(RoutingProfile::Beta, &mut beta).unwrap();
    assert_eq!(beta_decision.selected_provider, "deepinfra");
    assert_eq!(beta["model"], "deepinfra/meta-llama/llama-3.3-70b-instruct");
    assert_eq!(
        beta_decision.fallback_model.as_deref(),
        Some("openrouter/meta-llama/llama-3.3-70b-instruct")
    );
    assert!(beta_decision.has_alternatives);

    let mut production_forged = json!({
        "model": slug,
        "provider": {"sort": "price"},
        "routing_profile": "beta"
    });
    let forged_decision = routing
        .route(RoutingProfile::Production, &mut production_forged)
        .unwrap();
    assert_eq!(forged_decision.selected_provider, "openrouter");
    assert_eq!(
        production_forged["model"],
        "openrouter/meta-llama/llama-3.3-70b-instruct"
    );

    let mut legacy = json!({"model": slug, "provider": {"sort": "price"}});
    let legacy_decision = routing
        .route_for_endpoint(
            RoutingProfile::Beta,
            CompletionEndpoint::Legacy,
            &mut legacy,
        )
        .unwrap();
    assert_eq!(legacy_decision.selected_provider, "openrouter");
    assert_eq!(
        legacy["model"],
        "openrouter/meta-llama/llama-3.3-70b-instruct"
    );
    assert_eq!(legacy_decision.fallback_model, None);
}

#[test]
fn blackbox_glm_cost_is_exact_and_sell_price_remains_route_independent() {
    let registry = checked_registry();
    let prices = PriceTable::from_registry(&registry).unwrap();
    let slug = "z-ai/glm-5.2";
    let blackbox = prices.get("blackbox", slug).unwrap();
    let openrouter = prices.get("openrouter", slug).unwrap();

    assert_eq!(blackbox.sell_price, openrouter.sell_price);
    assert_eq!(
        blackbox,
        &BillingPrices {
            provider_cost: ModelPrices {
                input_price_per_mtok: Decimal::new(140, 2),
                output_price_per_mtok: Decimal::new(440, 2),
            },
            provider_cached_input_price_per_mtok: Some(Decimal::new(14, 2)),
            sell_price: ModelPrices {
                input_price_per_mtok: Decimal::new(70, 2),
                output_price_per_mtok: Decimal::new(220, 2),
            },
        }
    );

    let repeated_paid_probe = Usage {
        prompt_tokens: 17,
        completion_tokens: 4,
    };
    assert_eq!(
        provider_cost_usd(blackbox, &repeated_paid_probe, Some(16))
            .unwrap()
            .to_string(),
        "0.0000212400"
    );
    assert_eq!(
        cost_usd(&blackbox.sell_price, &repeated_paid_probe).to_string(),
        "0.0000207000"
    );
    assert_eq!(
        provider_cost_usd(blackbox, &repeated_paid_probe, None),
        None,
        "cached pricing requires exact cached-token telemetry"
    );
    assert_eq!(
        provider_cost_usd(blackbox, &repeated_paid_probe, Some(18)),
        None,
        "cached tokens must not exceed total prompt tokens"
    );
}

#[test]
fn routing_policy_selects_blackbox_only_for_default_beta_requests() {
    let registry = checked_registry();
    let routing = RoutingPolicy::from_registry(
        &registry,
        RoutingConfig::new(Decimal::new(100, 0), 0.1, Decimal::ONE, 100).unwrap(),
        MeasurementStore::default(),
    )
    .unwrap();
    let slug = "z-ai/glm-5.2";
    let mut production = json!({"model": slug});
    let mut beta = production.clone();

    let production_decision = routing
        .route(RoutingProfile::Production, &mut production)
        .unwrap();
    assert_eq!(production_decision.selected_provider, "openrouter");
    assert_eq!(production["model"], "openrouter/z-ai/glm-5.2");
    assert_eq!(production_decision.fallback_model, None);

    let beta_decision = routing.route(RoutingProfile::Beta, &mut beta).unwrap();
    assert_eq!(beta_decision.selected_provider, "blackbox");
    assert_eq!(beta["model"], "blackbox/z-ai/glm-5.2");
    assert_eq!(
        beta_decision.fallback_model.as_deref(),
        Some("openrouter/z-ai/glm-5.2")
    );

    let mut explicit_price = json!({
        "model": slug,
        "provider": {"sort": "price"}
    });
    let price_decision = routing
        .route(RoutingProfile::Beta, &mut explicit_price)
        .unwrap();
    assert_eq!(price_decision.selected_provider, "openrouter");
    assert_eq!(explicit_price["model"], "openrouter/z-ai/glm-5.2");
    assert_eq!(
        price_decision.fallback_model.as_deref(),
        Some("blackbox/z-ai/glm-5.2")
    );

    let mut legacy = json!({"model": slug});
    let legacy_decision = routing
        .route_for_endpoint(
            RoutingProfile::Beta,
            CompletionEndpoint::Legacy,
            &mut legacy,
        )
        .unwrap();
    assert_eq!(legacy_decision.selected_provider, "openrouter");
    assert_eq!(legacy["model"], "openrouter/z-ai/glm-5.2");
    assert_eq!(legacy_decision.fallback_model, None);

    let mut production_forged = json!({
        "model": slug,
        "routing_profile": "beta"
    });
    let forged_decision = routing
        .route(RoutingProfile::Production, &mut production_forged)
        .unwrap();
    assert_eq!(forged_decision.selected_provider, "openrouter");
    assert_eq!(production_forged["model"], "openrouter/z-ai/glm-5.2");
}

#[test]
fn compiled_sidecar_keeps_modal_internal_with_openrouter_fallback() {
    let registry = modal_beta_registry();
    let modal_model = &registry
        .providers
        .iter()
        .find(|provider| provider.id == "modal")
        .unwrap()
        .models[0];
    let compiled = compile(&registry, SidecarConfigOptions::default()).unwrap();
    let config: Value = serde_yaml::from_slice(&compiled).unwrap();
    let models = config["llm"]["models"].as_sequence().unwrap();
    let modal_route = named_entry(models, "modal/moonshotai/kimi-k3");

    assert_eq!(modal_route["provider"], "openAI");
    assert_eq!(
        modal_route["params"]["baseUrl"],
        "https://inference.us-west.modal.direct/v1"
    );
    assert_eq!(
        modal_route["params"]["model"],
        modal_model.provider_model_id
    );
    assert_eq!(
        modal_route["overrides"]["model"],
        modal_model.provider_model_id
    );
    assert_eq!(modal_route["params"]["apiKey"], "$SEREN_ROUTER_KEY_MODAL");
    assert_eq!(
        modal_route["responseHeaders"]["set"]["x-seren-served-provider"],
        "modal"
    );
    assert_eq!(modal_route["health"]["eviction"]["consecutiveFailures"], 1);
    assert_eq!(modal_route["health"]["eviction"]["duration"], "60s");

    let virtual_models = config["llm"]["virtualModels"].as_sequence().unwrap();
    let production = named_entry(
        virtual_models,
        "seren-profile-production/moonshotai/kimi-k3",
    );
    let beta = named_entry(virtual_models, "seren-profile-beta/moonshotai/kimi-k3");
    assert_eq!(
        production["routing"]["failover"]["targets"],
        serde_yaml::from_str::<Value>(
            r#"
- model: modal/moonshotai/kimi-k3
  priority: 0
- model: openrouter/moonshotai/kimi-k3
  priority: 255
"#
        )
        .unwrap()
    );
    assert_eq!(
        beta["routing"]["failover"]["targets"],
        serde_yaml::from_str::<Value>(
            r#"
- model: modal/moonshotai/kimi-k3
  priority: 0
- model: openrouter/moonshotai/kimi-k3
  priority: 255
"#
        )
        .unwrap()
    );
}

#[test]
fn compiled_sidecar_keeps_deepinfra_beta_only_with_openrouter_fallback() {
    let registry = checked_registry();
    let compiled = compile(&registry, SidecarConfigOptions::default()).unwrap();
    let config: Value = serde_yaml::from_slice(&compiled).unwrap();
    let models = config["llm"]["models"].as_sequence().unwrap();
    let direct = named_entry(models, "deepinfra/meta-llama/llama-3.3-70b-instruct");

    assert_eq!(direct["provider"], "openAI");
    assert_eq!(
        direct["params"]["baseUrl"],
        "https://api.deepinfra.com/v1/openai"
    );
    assert_eq!(
        direct["params"]["model"],
        "meta-llama/Llama-3.3-70B-Instruct-Turbo"
    );
    assert_eq!(
        direct["overrides"]["model"],
        "meta-llama/Llama-3.3-70B-Instruct-Turbo"
    );
    assert_eq!(direct["params"]["apiKey"], "$SEREN_ROUTER_KEY_DEEPINFRA");
    assert_eq!(
        direct["responseHeaders"]["set"]["x-seren-served-provider"],
        "deepinfra"
    );

    let virtual_models = config["llm"]["virtualModels"].as_sequence().unwrap();
    let production = named_entry(
        virtual_models,
        "seren-profile-production/meta-llama/llama-3.3-70b-instruct",
    );
    let beta = named_entry(
        virtual_models,
        "seren-profile-beta/meta-llama/llama-3.3-70b-instruct",
    );
    assert_eq!(
        production["routing"]["failover"]["targets"],
        serde_yaml::from_str::<Value>(
            r#"
- model: openrouter/meta-llama/llama-3.3-70b-instruct
  priority: 255
"#
        )
        .unwrap()
    );
    assert_eq!(
        beta["routing"]["failover"]["targets"],
        serde_yaml::from_str::<Value>(
            r#"
- model: deepinfra/meta-llama/llama-3.3-70b-instruct
  priority: 0
- model: openrouter/meta-llama/llama-3.3-70b-instruct
  priority: 255
"#
        )
        .unwrap()
    );
}

#[test]
fn compiled_sidecar_keeps_blackbox_beta_only_with_openrouter_fallback() {
    let registry = checked_registry();
    let compiled = compile(&registry, SidecarConfigOptions::default()).unwrap();
    let config: Value = serde_yaml::from_slice(&compiled).unwrap();
    let models = config["llm"]["models"].as_sequence().unwrap();
    let direct = named_entry(models, "blackbox/z-ai/glm-5.2");

    assert_eq!(direct["provider"], "openAI");
    assert_eq!(direct["params"]["baseUrl"], "https://api.blackbox.ai/v1");
    assert_eq!(direct["params"]["model"], "z-ai/glm-5.2");
    assert_eq!(direct["overrides"]["model"], "z-ai/glm-5.2");
    assert_eq!(direct["params"]["apiKey"], "$SEREN_ROUTER_KEY_BLACKBOX");
    assert_eq!(
        direct["responseHeaders"]["set"]["x-seren-served-provider"],
        "blackbox"
    );

    let virtual_models = config["llm"]["virtualModels"].as_sequence().unwrap();
    let production = named_entry(virtual_models, "seren-profile-production/z-ai/glm-5.2");
    let beta = named_entry(virtual_models, "seren-profile-beta/z-ai/glm-5.2");
    assert_eq!(
        production["routing"]["failover"]["targets"],
        serde_yaml::from_str::<Value>(
            r#"
- model: openrouter/z-ai/glm-5.2
  priority: 255
"#
        )
        .unwrap()
    );
    assert_eq!(
        beta["routing"]["failover"]["targets"],
        serde_yaml::from_str::<Value>(
            r#"
- model: blackbox/z-ai/glm-5.2
  priority: 0
- model: openrouter/z-ai/glm-5.2
  priority: 255
"#
        )
        .unwrap()
    );
}

#[test]
fn direct_and_fallback_routes_keep_one_reviewed_sell_price_and_separate_costs() {
    let registry = checked_registry();
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

fn checked_registry() -> Registry {
    serde_yaml::from_str(include_str!("../registry/providers.yaml")).unwrap()
}

fn modal_beta_registry() -> Registry {
    checked_registry()
}

fn assert_modal_endpoint_hostname(hostname: &str) {
    assert!(!hostname.is_empty());
    assert_eq!(hostname, hostname.trim());
    assert!(hostname.ends_with(".us-west.modal.direct"));
    assert!(!hostname.contains("://"));
    assert!(!hostname.contains('/'));
    assert!(hostname.split('.').all(|label| {
        !label.is_empty()
            && label.len() <= 63
            && label
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
            && label
                .as_bytes()
                .first()
                .is_some_and(u8::is_ascii_alphanumeric)
            && label
                .as_bytes()
                .last()
                .is_some_and(u8::is_ascii_alphanumeric)
    }));
}

fn named_entry<'a>(entries: &'a [Value], name: &str) -> &'a Value {
    entries
        .iter()
        .find(|entry| entry["name"] == name)
        .unwrap_or_else(|| panic!("missing compiled sidecar entry {name}"))
}
