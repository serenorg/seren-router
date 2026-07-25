// ABOUTME: Defines the sidecar-to-router marker for the provider that served a response.
// ABOUTME: Keeps provider attribution typed and internal instead of guessing from model slugs.

use axum::http::HeaderMap;

pub const SERVED_PROVIDER_HEADER: &str = "x-seren-served-provider";

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ServedProvider(String);

impl ServedProvider {
    pub fn from_headers(headers: &HeaderMap) -> Option<Self> {
        let provider_id = headers.get(SERVED_PROVIDER_HEADER)?.to_str().ok()?;
        if provider_id.is_empty() || provider_id.trim() != provider_id {
            return None;
        }

        Some(Self(provider_id.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[cfg(test)]
mod tests {
    use axum::http::HeaderValue;

    use super::*;

    #[test]
    fn provider_marker_must_be_present_nonempty_and_valid_text() {
        let mut headers = HeaderMap::new();
        assert_eq!(ServedProvider::from_headers(&headers), None);

        headers.insert(SERVED_PROVIDER_HEADER, HeaderValue::from_static(""));
        assert_eq!(ServedProvider::from_headers(&headers), None);

        headers.insert(
            SERVED_PROVIDER_HEADER,
            HeaderValue::from_bytes(&[0xff]).unwrap(),
        );
        assert_eq!(ServedProvider::from_headers(&headers), None);

        headers.insert(SERVED_PROVIDER_HEADER, HeaderValue::from_static(" local "));
        assert_eq!(ServedProvider::from_headers(&headers), None);

        headers.insert(SERVED_PROVIDER_HEADER, HeaderValue::from_static("local"));
        assert_eq!(
            ServedProvider::from_headers(&headers),
            Some(ServedProvider("local".to_owned()))
        );
    }
}
