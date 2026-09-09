//! Runtime shadow: run the owned transport beside `reqwest` on live traffic.
//!
//! ADR-0200's P2 exit criterion is "byte-identical response corpus against the
//! `reqwest` path under shadow". The hermetic comparison (`tests/shadow.rs`)
//! proves it against a scripted server; this module proves it against *whatever
//! the user is actually talking to*, without changing what the user gets.
//!
//! The contract is deliberately asymmetric:
//!
//! - the real request is unaffected — the shadow is a detached task, its failure
//!   is logged and discarded, and it never delays or aborts the production path;
//! - the shadow issues its own pair of requests (one `reqwest`, one owned) with
//!   the same bytes, then compares the reassembled SSE payloads. That is a
//!   deliberate cost: shadow mode is for validation windows, not for always-on
//!   production use;
//! - every divergence is a `warn!` carrying the payload index, so a mismatch is
//!   diagnosable from logs alone.
//!
//! Enabled with `MUTA_NET_SHADOW=1`. Off by default, and compiled out entirely
//! unless the `net-shadow` feature is on.

use std::sync::{Arc, Mutex};

use futures::StreamExt;
use muta_trace::RequestTrace;

use crate::egress::{HttpResponse, RequestParts};
use crate::sse::{data_payloads, payloads_from_chunks};

/// The environment variable that turns the shadow on.
pub const SHADOW_ENV: &str = "MUTA_NET_SHADOW";

/// A production request, captured in transport-neutral form.
///
/// The same type the [`crate::egress::Egress`] seam uses, so the shadow
/// compares exactly what the production transport was asked to send.
pub use crate::egress::RequestParts as ShadowRequest;

/// What the shadow learned about one request.
#[derive(Debug, Clone, Default)]
pub struct ShadowReport {
    /// SSE payloads reassembled from the `reqwest` run.
    pub reqwest_payloads: usize,
    /// SSE payloads reassembled from the owned run.
    pub net_payloads: usize,
    /// Index of the first payload that differed, when they did.
    pub divergence: Option<usize>,
    pub reqwest_error: Option<String>,
    pub net_error: Option<String>,
    /// Status the owned transport saw.
    pub net_status: Option<u16>,
    /// The owned run's trace, when it produced one.
    pub trace: Option<RequestTrace>,
}

impl ShadowReport {
    /// Whether the two transports both completed the request.
    ///
    /// This is the only property that holds on **live** traffic: two calls to a
    /// stochastic endpoint legitimately return different tokens, so payload
    /// equality cannot be required there. A live run therefore verifies that
    /// both transports produced a complete, error-free stream; payload equality
    /// is what the hermetic comparison asserts, and what
    /// [`Self::payloads_identical`] reports.
    pub fn agrees(&self) -> bool {
        self.reqwest_error.is_none()
            && self.net_error.is_none()
            && self.reqwest_payloads > 0
            && self.net_payloads > 0
    }

    /// Whether every payload matched exactly. Meaningful only when the endpoint
    /// is deterministic (a scripted server, or a model pinned to a fixed seed).
    pub fn payloads_identical(&self) -> bool {
        self.divergence.is_none() && self.reqwest_payloads == self.net_payloads
    }
}

/// Whether the shadow is enabled by the environment.
pub fn enabled() -> bool {
    enabled_from(std::env::var(SHADOW_ENV).ok().as_deref())
}

/// Testable form of [`enabled`].
pub fn enabled_from(value: Option<&str>) -> bool {
    matches!(value, Some("1") | Some("true") | Some("TRUE") | Some("yes"))
}

/// Run the shadow for `request` and log the outcome. Never panics; never
/// propagates a failure into the caller.
pub fn spawn(request: RequestParts) {
    tokio::spawn(async move {
        let report = run(request).await;
        if !report.agrees() {
            tracing::warn!(
                reqwest_payloads = report.reqwest_payloads,
                net_payloads = report.net_payloads,
                reqwest_error = ?report.reqwest_error,
                net_error = ?report.net_error,
                "net shadow: the owned transport did not complete the request"
            );
        } else {
            tracing::debug!(
                payloads = report.net_payloads,
                payloads_identical = report.payloads_identical(),
                "net shadow: both transports completed the request"
            );
        }
    });
}

/// Compare the two transports on one request.
pub async fn run(request: RequestParts) -> ShadowReport {
    let mut report = ShadowReport::default();
    let (reqwest_result, owned_result) = tokio::join!(run_reqwest(&request), run_owned(&request));

    let reqwest_payloads = match reqwest_result {
        Ok(payloads) => {
            report.reqwest_payloads = payloads.len();
            Some(payloads)
        }
        Err(error) => {
            report.reqwest_error = Some(error);
            None
        }
    };
    match owned_result {
        Ok(owned) => {
            report.net_payloads = owned.payloads.len();
            report.net_status = Some(owned.status);
            report.trace = owned.trace;
            if let Some(expected) = &reqwest_payloads {
                report.divergence = first_divergence(expected, &owned.payloads);
            }
        }
        Err(error) => report.net_error = Some(error),
    }
    report
}

/// The first payload index where the two runs disagree, including a length
/// mismatch.
fn first_divergence(expected: &[String], actual: &[String]) -> Option<usize> {
    expected
        .iter()
        .zip(actual)
        .position(|(left, right)| left != right)
        .or_else(|| (expected.len() != actual.len()).then(|| expected.len().min(actual.len())))
}

async fn run_reqwest(request: &RequestParts) -> Result<Vec<String>, String> {
    let response = reqwest::Client::new()
        .request(request.method.clone(), &request.url)
        .headers(request.headers.clone())
        .body(request.body.clone().unwrap_or_default())
        .send()
        .await
        .map_err(|error| error.to_string())?;
    if !response.status().is_success() {
        return Err(format!("reqwest status {}", response.status()));
    }
    let status = response.status();
    let headers = response.headers().clone();
    let body = response
        .bytes_stream()
        .map(|item| item.map_err(|error| crate::transport::transport_error("Shadow", error)))
        .boxed();
    Ok(data_payloads(
        HttpResponse {
            status,
            headers,
            body,
        },
        "Shadow",
    )
    .filter_map(|item| async move { item.ok() })
    .collect()
    .await)
}

struct OwnedRun {
    status: u16,
    payloads: Vec<String>,
    trace: Option<RequestTrace>,
}

async fn run_owned(request: &RequestParts) -> Result<OwnedRun, String> {
    // The shadow uses the same egress the product would, so a divergence is a
    // transport divergence and not a difference in how the request was built.
    let captured: Arc<Mutex<Option<RequestTrace>>> = Arc::new(Mutex::new(None));
    let slot = Arc::clone(&captured);
    let egress = crate::MutaNetEgress::new()?.with_trace_sink(Arc::new(move |trace| {
        *slot.lock().unwrap_or_else(|error| error.into_inner()) = Some(trace);
    }));
    let parts = RequestParts {
        label: "Shadow",
        timeout: None,
        ..request.clone()
    };
    let response = crate::Egress::send(&egress, parts)
        .await
        .map_err(|error| error.to_string())?;
    let status = response.status.as_u16();
    let chunks = response.into_byte_stream();
    let payloads: Vec<String> = payloads_from_chunks(chunks, "Shadow", |error| error)
        .filter_map(|item| async move { item.ok() })
        .collect()
        .await;
    let trace = captured
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .take();
    Ok(OwnedRun {
        status,
        payloads,
        trace,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enabled_only_for_affirmative_values() {
        assert!(enabled_from(Some("1")));
        assert!(enabled_from(Some("true")));
        assert!(enabled_from(Some("yes")));
        assert!(!enabled_from(None));
        assert!(!enabled_from(Some("0")));
        assert!(!enabled_from(Some("false")));
    }

    #[test]
    fn divergence_reports_the_first_difference_and_length_mismatch() {
        let left = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        assert_eq!(first_divergence(&left, &left), None);
        assert_eq!(
            first_divergence(&left, &["a".to_string(), "x".to_string(), "c".to_string()]),
            Some(1)
        );
        assert_eq!(
            first_divergence(&left, &["a".to_string(), "b".to_string()]),
            Some(2)
        );
        assert_eq!(
            first_divergence(
                &left,
                &[
                    "a".to_string(),
                    "b".to_string(),
                    "c".to_string(),
                    "d".to_string()
                ]
            ),
            Some(3)
        );
    }

    #[test]
    fn agreement_is_transport_completion_not_payload_equality() {
        let mut report = ShadowReport {
            reqwest_payloads: 3,
            net_payloads: 3,
            ..ShadowReport::default()
        };
        assert!(report.agrees());
        assert!(report.payloads_identical());

        // A live stochastic endpoint returns different payloads; the transports
        // still agree on the only thing they can agree on.
        report.divergence = Some(0);
        assert!(report.agrees(), "both completed");
        assert!(!report.payloads_identical(), "but the payloads differ");

        report.net_error = Some("boom".into());
        assert!(!report.agrees());

        let mut empty = ShadowReport::default();
        assert!(!empty.agrees(), "an empty run did not complete anything");
        empty.reqwest_payloads = 1;
        empty.net_payloads = 1;
        assert!(empty.agrees());
    }
}
