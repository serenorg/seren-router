// ABOUTME: Frames OpenAI-compatible server-sent events across arbitrary HTTP chunks.
// ABOUTME: Adds the exact customer sell subtotal to supported terminal usage shapes.

use serde_json::Value;

use crate::pricing::BillingPrices;
#[cfg(test)]
use crate::pricing::ModelPrices;
use crate::usage_cost::{CostedUsage, inject_usage_cost_value};

pub(crate) struct UsageCostTransformer {
    pending: Vec<u8>,
    prices: BillingPrices,
    costed_usage: Option<CostedUsage>,
}

impl UsageCostTransformer {
    pub(crate) fn new(prices: BillingPrices) -> Self {
        Self {
            pending: Vec::new(),
            prices,
            costed_usage: None,
        }
    }

    pub(crate) fn transform(mut self, bytes: &[u8]) -> (Self, Vec<u8>, Option<CostedUsage>) {
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
        }

        (self, output, completed)
    }

    pub(crate) fn finish(mut self) -> (Vec<u8>, Option<CostedUsage>) {
        let final_line = self
            .pending
            .strip_suffix(b"\r\n")
            .or_else(|| self.pending.strip_suffix(b"\n"))
            .unwrap_or(&self.pending);
        let completed = single_data_line(final_line)
            .filter(|(_, data)| *data == b"[DONE]")
            .and_then(|_| self.costed_usage.take());

        (self.pending, completed)
    }

    fn transform_event(&mut self, event: &[u8]) -> (Vec<u8>, Option<CostedUsage>) {
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
        if !is_terminal_usage_event(&value) {
            return (event.to_vec(), None);
        }
        let Ok(costed_usage) = inject_usage_cost_value(&mut value, &self.prices) else {
            return (event.to_vec(), None);
        };
        self.costed_usage = Some(costed_usage);

        let Ok(json) = serde_json::to_vec(&value) else {
            self.costed_usage = None;
            return (event.to_vec(), None);
        };
        let mut transformed = Vec::with_capacity(data_prefix.len() + json.len() + separator.len());
        transformed.extend_from_slice(data_prefix);
        transformed.extend_from_slice(&json);
        transformed.extend_from_slice(separator);
        (transformed, None)
    }
}

fn is_terminal_usage_event(value: &Value) -> bool {
    let Some(choices) = value.get("choices").and_then(Value::as_array) else {
        return false;
    };
    choices.is_empty()
        || choices.iter().any(|choice| {
            choice
                .get("finish_reason")
                .is_some_and(|reason| !reason.is_null())
        })
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
            "\"cost\":0.0000088000,\"prompt_tokens\":16}}\n\n",
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

    fn assert_every_boundary(input: &[u8], expected: &[u8], expected_usage: Option<CostedUsage>) {
        for split in 0..=input.len() {
            let transformer = UsageCostTransformer::new(test_prices());
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

        let mut transformer = UsageCostTransformer::new(test_prices());
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

    fn test_prices() -> BillingPrices {
        BillingPrices {
            provider_cost: ModelPrices {
                input_price_per_mtok: Decimal::new(10, 2),
                output_price_per_mtok: Decimal::new(20, 2),
            },
            sell_price: ModelPrices {
                input_price_per_mtok: Decimal::new(40, 2),
                output_price_per_mtok: Decimal::new(80, 2),
            },
        }
    }

    fn test_usage() -> CostedUsage {
        CostedUsage {
            response_id: Some("chatcmpl-olxkqmuioetwzpcvhdrkm".to_owned()),
            usage: crate::pricing::Usage {
                prompt_tokens: 16,
                completion_tokens: 3,
            },
            provider_cost_usd: "0.0000022000".parse().unwrap(),
            sell_price_usd: "0.0000088000".parse().unwrap(),
        }
    }
}
