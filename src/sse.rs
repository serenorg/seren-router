// ABOUTME: Frames OpenAI-compatible server-sent events across arbitrary HTTP chunks.
// ABOUTME: Adds exact served-provider cost to supported terminal usage shapes.

use serde_json::Value;

#[cfg(test)]
use crate::pricing::ModelPrices;
use crate::pricing::ProviderPrices;
use crate::usage_cost::{
    CostedUsage, inject_usage_cost_value, sanitize_public_completion_value,
    strip_provider_specific_fields,
};

pub(crate) struct UsageCostTransformer {
    pending: Vec<u8>,
    prices: Option<ProviderPrices>,
    response_model: Option<String>,
    costed_usage: Option<CostedUsage>,
    closed: bool,
}

impl UsageCostTransformer {
    pub(crate) fn new(prices: ProviderPrices) -> Self {
        Self {
            pending: Vec::new(),
            prices: Some(prices),
            response_model: None,
            costed_usage: None,
            closed: false,
        }
    }

    #[cfg(test)]
    pub(crate) fn model_only(response_model: &str) -> Self {
        Self {
            pending: Vec::new(),
            prices: None,
            response_model: Some(response_model.to_owned()),
            costed_usage: None,
            closed: false,
        }
    }

    pub(crate) fn with_response_model(mut self, response_model: &str) -> Self {
        self.response_model = Some(response_model.to_owned());
        self
    }

    pub(crate) fn is_closed(&self) -> bool {
        self.closed
    }

    pub(crate) fn transform(mut self, bytes: &[u8]) -> (Self, Vec<u8>, Option<CostedUsage>) {
        if self.closed {
            return (self, Vec::new(), None);
        }
        self.pending.extend_from_slice(bytes);
        let mut output = Vec::with_capacity(self.pending.len());
        let mut completed = None;

        while let Some(event_end) = next_event_end(&self.pending) {
            let event: Vec<_> = self.pending.drain(..event_end).collect();
            let (transformed, event_completion) = self.transform_event(&event);
            output.extend_from_slice(&transformed);
            if event_completion.is_some() {
                completed = event_completion;
            }
            if self.closed {
                self.pending.clear();
                break;
            }
        }

        (self, output, completed)
    }

    pub(crate) fn finish(mut self) -> (Vec<u8>, Option<CostedUsage>) {
        if self.closed {
            return (Vec::new(), None);
        }
        let final_line = self
            .pending
            .strip_suffix(b"\r\n")
            .or_else(|| self.pending.strip_suffix(b"\n"))
            .unwrap_or(&self.pending);
        if let Some(response_model) = self.response_model.as_deref() {
            if single_data_line(final_line).is_some_and(|(_, data)| data == b"[DONE]") {
                if self.prices.is_some() && self.costed_usage.is_none() {
                    return (generic_upstream_error_event(response_model), None);
                }
                return (self.pending, self.costed_usage.take());
            }
            self.costed_usage = None;
            return (generic_upstream_error_event(response_model), None);
        }
        let completed = single_data_line(final_line)
            .filter(|(_, data)| *data == b"[DONE]")
            .and_then(|_| self.costed_usage.take());

        (self.pending, completed)
    }

    fn transform_event(&mut self, event: &[u8]) -> (Vec<u8>, Option<CostedUsage>) {
        if let Some(response_model) = self.response_model.clone() {
            return self.transform_public_event(event, &response_model);
        }

        self.transform_internal_event(event)
    }

    fn transform_public_event(
        &mut self,
        event: &[u8],
        response_model: &str,
    ) -> (Vec<u8>, Option<CostedUsage>) {
        let (body, separator) = split_separator(event);
        let Some((data_prefix, data)) = single_data_line(body) else {
            return self.close_with_generic_error(response_model);
        };
        if data == b"[DONE]" {
            if self.prices.is_some() && self.costed_usage.is_none() {
                return self.close_with_generic_error(response_model);
            }
            self.closed = true;
            return (event.to_vec(), self.costed_usage.take());
        }

        let Ok(mut value) = serde_json::from_slice::<Value>(data) else {
            return self.close_with_generic_error(response_model);
        };
        if sanitize_public_completion_value(&mut value, response_model).is_err() {
            return self.close_with_generic_error(response_model);
        }
        let costed_usage = self.prices.as_ref().and_then(|prices| {
            is_terminal_usage_event(&value)
                .then(|| inject_usage_cost_value(&mut value, prices).ok())
                .flatten()
        });

        let Ok(json) = serde_json::to_vec(&value) else {
            return self.close_with_generic_error(response_model);
        };
        if let Some(costed_usage) = costed_usage {
            self.costed_usage = Some(costed_usage);
        }
        let mut transformed = Vec::with_capacity(data_prefix.len() + json.len() + separator.len());
        transformed.extend_from_slice(data_prefix);
        transformed.extend_from_slice(&json);
        transformed.extend_from_slice(separator);
        (transformed, None)
    }

    fn transform_internal_event(&mut self, event: &[u8]) -> (Vec<u8>, Option<CostedUsage>) {
        let (body, separator) = split_separator(event);
        let Some((data_prefix, data)) = single_data_line(body) else {
            return (event.to_vec(), None);
        };
        if data == b"[DONE]" {
            return (event.to_vec(), self.costed_usage.take());
        }

        let Ok(mut value) = serde_json::from_slice::<Value>(data) else {
            return (event.to_vec(), None);
        };
        let sanitized = strip_provider_specific_fields(&mut value);
        let costed_usage = self.prices.as_ref().and_then(|prices| {
            is_terminal_usage_event(&value)
                .then(|| inject_usage_cost_value(&mut value, prices).ok())
                .flatten()
        });
        if costed_usage.is_none() && !sanitized {
            return (event.to_vec(), None);
        }

        let Ok(json) = serde_json::to_vec(&value) else {
            return (event.to_vec(), None);
        };
        if let Some(costed_usage) = costed_usage {
            self.costed_usage = Some(costed_usage);
        }
        let mut transformed = Vec::with_capacity(data_prefix.len() + json.len() + separator.len());
        transformed.extend_from_slice(data_prefix);
        transformed.extend_from_slice(&json);
        transformed.extend_from_slice(separator);
        (transformed, None)
    }

    fn close_with_generic_error(&mut self, response_model: &str) -> (Vec<u8>, Option<CostedUsage>) {
        self.closed = true;
        self.costed_usage = None;
        (generic_upstream_error_event(response_model), None)
    }
}

fn generic_upstream_error(canonical_model: &str) -> Value {
    serde_json::json!({
        "error": {
            "code": "upstream_error",
            "message": "upstream request failed",
            "metadata": {
                "model": canonical_model
            }
        }
    })
}

fn generic_upstream_error_event(canonical_model: &str) -> Vec<u8> {
    let error = serde_json::to_vec(&generic_upstream_error(canonical_model))
        .expect("static error serializes");
    let mut event = Vec::with_capacity(error.len() + 31);
    event.extend_from_slice(b"data: ");
    event.extend_from_slice(&error);
    event.extend_from_slice(b"\n\ndata: [DONE]\n\n");
    event
}

fn is_terminal_usage_event(value: &Value) -> bool {
    value.get("usage").is_some_and(Value::is_object)
}

fn next_event_end(bytes: &[u8]) -> Option<usize> {
    let lf = find_subslice(bytes, b"\n\n").map(|position| position + 2);
    let crlf = find_subslice(bytes, b"\r\n\r\n").map(|position| position + 4);

    match (lf, crlf) {
        (Some(lf), Some(crlf)) => Some(lf.min(crlf)),
        (Some(lf), None) => Some(lf),
        (None, Some(crlf)) => Some(crlf),
        (None, None) => None,
    }
}

fn split_separator(event: &[u8]) -> (&[u8], &[u8]) {
    if event.ends_with(b"\r\n\r\n") {
        event.split_at(event.len() - 4)
    } else {
        event.split_at(event.len() - 2)
    }
}

fn single_data_line(body: &[u8]) -> Option<(&[u8], &[u8])> {
    if body.contains(&b'\n') || body.contains(&b'\r') {
        return None;
    }

    let data = body.strip_prefix(b"data:")?;
    let data = data.strip_prefix(b" ").unwrap_or(data);
    let prefix_length = body.len() - data.len();
    Some(body.split_at(prefix_length))
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

#[cfg(test)]
mod tests {
    use rust_decimal::Decimal;

    use super::*;

    #[test]
    fn real_session_is_transformed_across_every_boundary() {
        let input = include_bytes!("../tests/fixtures/streaming_chat_response.sse");
        let expected = include_bytes!("../tests/golden/streaming_chat_cost.sse");
        let expected_usage = Some(test_usage());
        assert_every_boundary(input, expected, expected_usage.clone());

        let crlf_input = String::from_utf8_lossy(input).replace('\n', "\r\n");
        let crlf_expected = String::from_utf8_lossy(expected).replace('\n', "\r\n");
        assert_every_boundary(
            crlf_input.as_bytes(),
            crlf_expected.as_bytes(),
            expected_usage,
        );

        let malformed =
            b"data: {\"choices\":[],\"usage\":{\"prompt_tokens\":\"invalid\"}}\n\ndata: [DONE]\n\n";
        assert_every_boundary(malformed, malformed, None);
    }

    #[test]
    fn usage_attached_to_the_final_choice_is_transformed_across_every_boundary() {
        let input = concat!(
            "data: {\"id\":\"chatcmpl-final\",\"choices\":[{\"delta\":{},",
            "\"finish_reason\":\"stop\",\"index\":0}],\"usage\":",
            "{\"prompt_tokens\":16,\"completion_tokens\":3}}\n\n",
            "data: [DONE]\n\n"
        );
        let expected = concat!(
            "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\",\"index\":0}],",
            "\"id\":\"chatcmpl-final\",\"usage\":{\"completion_tokens\":3,",
            "\"cost\":0.0000022000,\"prompt_tokens\":16}}\n\n",
            "data: [DONE]\n\n"
        );

        assert_every_boundary(
            input.as_bytes(),
            expected.as_bytes(),
            Some(CostedUsage {
                response_id: Some("chatcmpl-final".to_owned()),
                ..test_usage()
            }),
        );
    }

    #[test]
    fn cached_prompt_usage_requires_exact_terminal_details() {
        let input = concat!(
            "data: {\"id\":\"chatcmpl-cached\",\"choices\":[],\"usage\":",
            "{\"prompt_tokens\":100,\"completion_tokens\":10,",
            "\"prompt_tokens_details\":{\"cached_tokens\":40}}}\n\n",
            "data: [DONE]\n\n"
        );
        let expected = concat!(
            "data: {\"choices\":[],\"id\":\"chatcmpl-cached\",\"usage\":",
            "{\"completion_tokens\":10,\"cost\":0.0003420000,\"prompt_tokens\":100,",
            "\"prompt_tokens_details\":{\"cached_tokens\":40}}}\n\n",
            "data: [DONE]\n\n"
        );
        assert_every_boundary_with_prices(
            input.as_bytes(),
            expected.as_bytes(),
            Some(CostedUsage {
                response_id: Some("chatcmpl-cached".to_owned()),
                usage: crate::pricing::Usage {
                    prompt_tokens: 100,
                    completion_tokens: 10,
                },
                cost_usd: "0.0003420000".parse().unwrap(),
            }),
            cached_test_prices(),
        );

        let missing_details = concat!(
            "data: {\"id\":\"chatcmpl-cached\",\"choices\":[],\"usage\":",
            "{\"prompt_tokens\":100,\"completion_tokens\":10}}\n\n",
            "data: [DONE]\n\n"
        );
        let expected_missing_details = concat!(
            "data: {\"error\":{\"code\":\"upstream_error\",",
            "\"message\":\"upstream request failed\",",
            "\"metadata\":{\"model\":\"canonical/model\"}}}\n\n",
            "data: [DONE]\n\n"
        );
        assert_every_boundary_with_options(
            missing_details.as_bytes(),
            expected_missing_details.as_bytes(),
            None,
            cached_test_prices(),
            Some("canonical/model"),
        );
    }

    #[test]
    fn terminal_provider_reported_cost_takes_precedence_over_registry_estimates() {
        let input = concat!(
            "data: {\"id\":\"chatcmpl-provider-cost\",\"choices\":[],\"usage\":",
            "{\"prompt_tokens\":41,\"completion_tokens\":128,\"cost\":0.00034092}}\n\n",
            "data: [DONE]\n\n"
        );
        let expected = concat!(
            "data: {\"choices\":[],\"id\":\"chatcmpl-provider-cost\",\"usage\":",
            "{\"completion_tokens\":128,\"cost\":0.0003409200,\"prompt_tokens\":41}}\n\n",
            "data: [DONE]\n\n"
        );

        assert_every_boundary_with_prices(
            input.as_bytes(),
            expected.as_bytes(),
            Some(CostedUsage {
                response_id: Some("chatcmpl-provider-cost".to_owned()),
                usage: crate::pricing::Usage {
                    prompt_tokens: 41,
                    completion_tokens: 128,
                },
                cost_usd: "0.0003409200".parse().unwrap(),
            }),
            cached_test_prices(),
        );
    }

    #[test]
    fn public_stream_keeps_standard_fields_and_strips_private_metadata() {
        let input = concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"hello\",",
            "\"provider_specific_fields\":{\"provider\":\"private-nested-vendor\"}},",
            "\"index\":0}],",
            "\"id\":\"chatcmpl-private\",\"object\":\"chat.completion.chunk\",",
            "\"provider\":\"private-vendor\",\"account\":\"private-account\"}\n\n",
            "data: {\"choices\":[],\"id\":\"chatcmpl-private\",",
            "\"model\":\"private-endpoint\",\"object\":\"chat.completion.chunk\",",
            "\"metadata\":{\"provider\":\"private-vendor\"},\"usage\":",
            "{\"completion_tokens\":3,\"prompt_tokens\":16}}\n\n",
            "data: [DONE]\n\n"
        );
        let expected = concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"hello\"},\"index\":0}],",
            "\"id\":\"chatcmpl-private\",\"model\":\"canonical/model\",",
            "\"object\":\"chat.completion.chunk\"}\n\n",
            "data: {\"choices\":[],\"id\":\"chatcmpl-private\",",
            "\"model\":\"canonical/model\",\"object\":\"chat.completion.chunk\",",
            "\"usage\":{\"completion_tokens\":3,",
            "\"cost\":0.0000022000,\"prompt_tokens\":16}}\n\n",
            "data: [DONE]\n\n"
        );

        assert_every_boundary_with_options(
            input.as_bytes(),
            expected.as_bytes(),
            Some(CostedUsage {
                response_id: Some("chatcmpl-private".to_owned()),
                ..test_usage()
            }),
            test_prices(),
            Some("canonical/model"),
        );
        let output = String::from_utf8(expected.as_bytes().to_vec()).unwrap();
        for private_identifier in [
            "private-vendor",
            "private-account",
            "private-endpoint",
            "private-nested-vendor",
        ] {
            assert!(!output.contains(private_identifier));
        }
    }

    #[test]
    fn priced_internal_stream_strips_nested_provider_specific_fields() {
        let input = concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"hello\",",
            "\"provider_specific_fields\":{\"provider\":\"private-provider\"}},",
            "\"index\":0}],\"id\":\"chatcmpl-private\",",
            "\"object\":\"chat.completion.chunk\"}\n\n",
            "data: {\"choices\":[{\"delta\":{},\"index\":0}],",
            "\"id\":\"chatcmpl-private\",",
            "\"object\":\"chat.completion.chunk\",\"usage\":",
            "{\"completion_tokens\":3,\"prompt_tokens\":16}}\n\n",
            "data: [DONE]\n\n"
        );
        let expected = concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"hello\"},\"index\":0}],",
            "\"id\":\"chatcmpl-private\",\"object\":\"chat.completion.chunk\"}\n\n",
            "data: {\"choices\":[{\"delta\":{},\"index\":0}],",
            "\"id\":\"chatcmpl-private\",",
            "\"object\":\"chat.completion.chunk\",\"usage\":{\"completion_tokens\":3,",
            "\"cost\":0.0000022000,\"prompt_tokens\":16}}\n\n",
            "data: [DONE]\n\n"
        );

        assert_every_boundary_with_prices(
            input.as_bytes(),
            expected.as_bytes(),
            Some(CostedUsage {
                response_id: Some("chatcmpl-private".to_owned()),
                ..test_usage()
            }),
            test_prices(),
        );
        assert!(!String::from_utf8_lossy(expected.as_bytes()).contains("private-provider"));
    }

    #[test]
    fn public_stream_replaces_provider_error_events_without_pricing() {
        let input = concat!(
            "data: {\"error\":{\"message\":\"Modal account acme failed\",",
            "\"metadata\":{\"provider\":\"modal\"},\"type\":\"provider_error\"},",
            "\"id\":\"req-modal-acme\",\"model\":\"account.modal.direct\"}\n\n",
            "data: [DONE]\n\n"
        );
        let expected = concat!(
            "data: {\"error\":{\"code\":\"upstream_error\",",
            "\"message\":\"upstream request failed\",",
            "\"metadata\":{\"model\":\"canonical/model\"}}}\n\n",
            "data: [DONE]\n\n"
        );

        for split in 0..=input.len() {
            let transformer = UsageCostTransformer::model_only("canonical/model");
            let (transformer, first, first_usage) =
                transformer.transform(&input.as_bytes()[..split]);
            let (transformer, second, second_usage) =
                transformer.transform(&input.as_bytes()[split..]);
            let (remainder, final_usage) = transformer.finish();
            let output = [first, second, remainder].concat();

            assert_eq!(output, expected.as_bytes(), "split at byte {split}");
            assert_eq!(first_usage.or(second_usage).or(final_usage), None);
            let output = String::from_utf8(output).unwrap();
            for private_identifier in ["Modal", "modal", "acme", "req-modal-acme"] {
                assert!(
                    !output.contains(private_identifier),
                    "private identifier {private_identifier:?} leaked at byte {split}"
                );
            }
        }
    }

    #[test]
    fn public_priced_stream_fails_closed_when_done_has_no_usage() {
        let input = concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"hello\"},\"index\":0}],",
            "\"id\":\"chatcmpl-private\",\"object\":\"chat.completion.chunk\"}\n\n",
            "data: [DONE]\n\n"
        );
        let expected = concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"hello\"},\"index\":0}],",
            "\"id\":\"chatcmpl-private\",\"model\":\"canonical/model\",",
            "\"object\":\"chat.completion.chunk\"}\n\n",
            "data: {\"error\":{\"code\":\"upstream_error\",",
            "\"message\":\"upstream request failed\",",
            "\"metadata\":{\"model\":\"canonical/model\"}}}\n\n",
            "data: [DONE]\n\n"
        );

        assert_every_boundary_with_options(
            input.as_bytes(),
            expected.as_bytes(),
            None,
            test_prices(),
            Some("canonical/model"),
        );
    }

    #[test]
    fn public_stream_fails_closed_for_malformed_multiline_and_unrecognized_events() {
        let expected = concat!(
            "data: {\"error\":{\"code\":\"upstream_error\",",
            "\"message\":\"upstream request failed\",",
            "\"metadata\":{\"model\":\"canonical/model\"}}}\n\n",
            "data: [DONE]\n\n"
        );
        for input in [
            "data: not-json\n\ndata: {\"provider\":\"private-after-error\"}\n\n",
            concat!(
                "event: chunk\n",
                "data: {\"id\":\"chatcmpl-private\",\"object\":\"chat.completion.chunk\",",
                "\"choices\":[],\"provider\":\"private-multiline\"}\n\n"
            ),
            concat!(
                "data: {\"id\":\"chatcmpl-private\",\"object\":\"chat.completion.chunk\",",
                "\"provider\":\"private-unrecognized\"}\n\n",
                "data: [DONE]\n\n"
            ),
            concat!(
                "data: {\"id\":\"chatcmpl-private\",\"object\":\"chat.completion.chunk\",",
                "\"choices\":[],\"provider\":\"private-incomplete\"}"
            ),
        ] {
            assert_every_boundary_with_options(
                input.as_bytes(),
                expected.as_bytes(),
                None,
                test_prices(),
                Some("canonical/model"),
            );
        }
    }

    fn assert_every_boundary(input: &[u8], expected: &[u8], expected_usage: Option<CostedUsage>) {
        assert_every_boundary_with_prices(input, expected, expected_usage, test_prices());
    }

    fn assert_every_boundary_with_prices(
        input: &[u8],
        expected: &[u8],
        expected_usage: Option<CostedUsage>,
        prices: ProviderPrices,
    ) {
        assert_every_boundary_with_options(input, expected, expected_usage, prices, None);
    }

    fn assert_every_boundary_with_options(
        input: &[u8],
        expected: &[u8],
        expected_usage: Option<CostedUsage>,
        prices: ProviderPrices,
        response_model: Option<&str>,
    ) {
        for split in 0..=input.len() {
            let transformer = transformer(prices.clone(), response_model);
            let (transformer, first, first_usage) = transformer.transform(&input[..split]);
            let (transformer, second, second_usage) = transformer.transform(&input[split..]);
            let mut output = first;
            output.extend_from_slice(&second);
            let (remainder, final_usage) = transformer.finish();
            output.extend_from_slice(&remainder);
            assert_eq!(output, expected, "split at byte {split}");
            assert_eq!(
                first_usage.or(second_usage).or(final_usage),
                expected_usage,
                "completion metadata at byte {split}"
            );
        }

        let mut transformer = transformer(prices, response_model);
        let mut output = Vec::new();
        let mut completed = None;
        for byte in input {
            let (next, transformed, usage) = transformer.transform(std::slice::from_ref(byte));
            transformer = next;
            output.extend_from_slice(&transformed);
            if usage.is_some() {
                completed = usage;
            }
        }
        let (remainder, final_usage) = transformer.finish();
        output.extend_from_slice(&remainder);
        assert_eq!(output, expected, "one-byte chunks");
        assert_eq!(
            completed.or(final_usage),
            expected_usage,
            "one-byte completion metadata"
        );
    }

    fn transformer(prices: ProviderPrices, response_model: Option<&str>) -> UsageCostTransformer {
        match response_model {
            Some(response_model) => {
                UsageCostTransformer::new(prices).with_response_model(response_model)
            }
            None => UsageCostTransformer::new(prices),
        }
    }

    fn test_prices() -> ProviderPrices {
        ProviderPrices {
            provider_cost: ModelPrices {
                input_price_per_mtok: Decimal::new(10, 2),
                output_price_per_mtok: Decimal::new(20, 2),
            },
            provider_cached_input_price_per_mtok: None,
        }
    }

    fn cached_test_prices() -> ProviderPrices {
        ProviderPrices {
            provider_cost: ModelPrices {
                input_price_per_mtok: "3.00".parse().unwrap(),
                output_price_per_mtok: "15.00".parse().unwrap(),
            },
            provider_cached_input_price_per_mtok: Some("0.30".parse().unwrap()),
        }
    }

    fn test_usage() -> CostedUsage {
        CostedUsage {
            response_id: Some("chatcmpl-olxkqmuioetwzpcvhdrkm".to_owned()),
            usage: crate::pricing::Usage {
                prompt_tokens: 16,
                completion_tokens: 3,
            },
            cost_usd: "0.0000022000".parse().unwrap(),
        }
    }
}
