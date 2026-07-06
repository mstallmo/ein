// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Mason Stallmo

//! OpenTelemetry glue (only compiled under the `otel` feature).
//!
//! `ein_agent` is transport-agnostic, so it never touches gRPC/HTTP types.
//! Drivers collect the inbound W3C trace headers (`traceparent`/`tracestate`)
//! from whatever carrier they have and hand them to
//! [`AgentBuilder::with_trace_headers`](crate::AgentBuilder::with_trace_headers)
//! as plain `(name, value)` pairs; [`HeaderExtractor`] adapts those pairs to the
//! OpenTelemetry [`Extractor`] interface so the globally-installed
//! `TextMapPropagator` can pull the remote context out of them.

use opentelemetry::propagation::Extractor;

/// Adapts a slice of `(name, value)` header pairs to [`Extractor`].
pub(crate) struct HeaderExtractor<'a>(pub(crate) &'a [(String, String)]);

impl Extractor for HeaderExtractor<'_> {
    /// Header names are case-insensitive (and gRPC lowercases them), so match
    /// the propagator's `traceparent`/`tracestate` lookups ASCII-insensitively.
    fn get(&self, key: &str) -> Option<&str> {
        self.0
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case(key))
            .map(|(_, value)| value.as_str())
    }

    fn keys(&self) -> Vec<&str> {
        self.0.iter().map(|(name, _)| name.as_str()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers() -> Vec<(String, String)> {
        vec![
            (
                "traceparent".to_string(),
                "00-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-01".to_string(),
            ),
            ("tracestate".to_string(), "vendor=value".to_string()),
        ]
    }

    #[test]
    fn get_matches_key_case_insensitively() {
        let headers = headers();
        let extractor = HeaderExtractor(&headers);

        // Propagators look up lowercase names; drivers may carry any casing.
        assert_eq!(
            extractor.get("traceparent"),
            Some("00-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-01"),
        );
        assert_eq!(extractor.get("TraceParent"), extractor.get("traceparent"));
        assert_eq!(extractor.get("tracestate"), Some("vendor=value"));
    }

    #[test]
    fn get_returns_none_for_absent_key() {
        let headers = headers();
        assert_eq!(HeaderExtractor(&headers).get("baggage"), None);
        assert_eq!(HeaderExtractor(&[]).get("traceparent"), None);
    }

    #[test]
    fn keys_lists_every_header_name() {
        let headers = headers();
        assert_eq!(
            HeaderExtractor(&headers).keys(),
            vec!["traceparent", "tracestate"],
        );
    }
}
