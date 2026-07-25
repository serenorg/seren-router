// ABOUTME: Frames OpenAI-compatible server-sent events across arbitrary HTTP chunks.
// ABOUTME: Adds exact provider cost only to a terminal empty-choices usage event.

use serde_json::Value;

use crate::pricing::ModelPrices;
use crate::usage_cost::inject_usage_cost_value;

pub(crate) struct UsageCostTransformer {
    pending: Vec<u8>,
    prices: ModelPrices,
}

impl UsageCostTransformer {
    pub(crate) fn new(prices: ModelPrices) -> Self {
        Self {
            pending: Vec::new(),
            prices,
        }
    }

    pub(crate) fn transform(mut self, bytes: &[u8]) -> (Self, Vec<u8>) {
        self.pending.extend_from_slice(bytes);
        let mut output = Vec::with_capacity(self.pending.len());

        while let Some(event_end) = next_event_end(&self.pending) {
            let event: Vec<_> = self.pending.drain(..event_end).collect();
            output.extend_from_slice(&self.transform_event(&event));
        }

        (self, output)
    }

    pub(crate) fn finish(self) -> Vec<u8> {
        self.pending
    }

    fn transform_event(&self, event: &[u8]) -> Vec<u8> {
        let (body, separator) = split_separator(event);
        let Some((data_prefix, data)) = single_data_line(body) else {
            return event.to_vec();
        };
        if data == b"[DONE]" {
            return event.to_vec();
        }

        let Ok(mut value) = serde_json::from_slice::<Value>(data) else {
            return event.to_vec();
        };
        if !value
            .get("choices")
            .and_then(Value::as_array)
            .is_some_and(Vec::is_empty)
        {
            return event.to_vec();
        }
        if inject_usage_cost_value(&mut value, &self.prices).is_err() {
            return event.to_vec();
        }

        let Ok(json) = serde_json::to_vec(&value) else {
            return event.to_vec();
        };
        let mut transformed = Vec::with_capacity(data_prefix.len() + json.len() + separator.len());
        transformed.extend_from_slice(data_prefix);
        transformed.extend_from_slice(&json);
        transformed.extend_from_slice(separator);
        transformed
    }
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
        assert_every_boundary(input, expected);

        let crlf_input = String::from_utf8_lossy(input).replace('\n', "\r\n");
        let crlf_expected = String::from_utf8_lossy(expected).replace('\n', "\r\n");
        assert_every_boundary(crlf_input.as_bytes(), crlf_expected.as_bytes());

        let malformed =
            b"data: {\"choices\":[],\"usage\":{\"prompt_tokens\":\"invalid\"}}\n\ndata: [DONE]\n\n";
        assert_every_boundary(malformed, malformed);
    }

    fn assert_every_boundary(input: &[u8], expected: &[u8]) {
        for split in 0..=input.len() {
            let transformer = UsageCostTransformer::new(test_prices());
            let (transformer, first) = transformer.transform(&input[..split]);
            let (transformer, second) = transformer.transform(&input[split..]);
            let mut output = first;
            output.extend_from_slice(&second);
            output.extend_from_slice(&transformer.finish());
            assert_eq!(output, expected, "split at byte {split}");
        }

        let mut transformer = UsageCostTransformer::new(test_prices());
        let mut output = Vec::new();
        for byte in input {
            let (next, transformed) = transformer.transform(std::slice::from_ref(byte));
            transformer = next;
            output.extend_from_slice(&transformed);
        }
        output.extend_from_slice(&transformer.finish());
        assert_eq!(output, expected, "one-byte chunks");
    }

    fn test_prices() -> ModelPrices {
        ModelPrices {
            input_price_per_mtok: Decimal::new(40, 2),
            output_price_per_mtok: Decimal::new(80, 2),
        }
    }
}
