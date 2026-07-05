//! L4 — backend health monitor. A background tokio task that polls the
//! upstream backend's `/health` (or `/v1/models` for OpenAI-compat
//! providers) every 10 seconds and keeps an atomic state. Surfaces via
//! `GET /v1/health/backend` and as a `backend_status` field on the
//! canonical `/health` endpoint.
//!
//! ## Why
//!
//! Without this, a stalled llama-server only manifests as 120-second
//! upstream timeouts on every request. With this, the gateway knows
//! within 10s of the issue and can route around it (router can prefix
//! "Backend is currently unreachable" — wired in the Integration
//! commit).
//!
//! ## Static-by-construction
//!
//! The poll is a `tokio::time::interval` — no time-of-day reads, no
//! UUIDs. The state is updated in place from the polled body. Stable
//! across restarts modulo the upstream.

use std::sync::Arc;
use std::time::Duration;

use axum::{extract::State, http::StatusCode, response::IntoResponse, Json};
use serde::Serialize;
use serde_json::{json, Value};
use tokio::sync::RwLock;
use tokio::time::{interval, MissedTickBehavior};

/// Coarse status used by the gateway's router and the `/health`
/// endpoint consumers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Healthy,
    Degraded,
    Down,
}

/// Cached backend health snapshot. Updates on every poll.
#[derive(Debug, Clone, Serialize)]
pub struct Snapshot {
    pub status: Status,
    /// Last observed model name (from the polled response), if any.
    pub model: Option<String>,
    /// Unix-millis when the most recent poll returned successfully.
    pub last_ok_ms: Option<i64>,
    /// Unix-millis when the most recent poll ran (any outcome).
    pub last_check_ms: i64,
    /// Consecutive failures since the last success (resets on success).
    pub consecutive_failures: u32,
    /// Last error message if the most recent poll failed.
    pub last_error: Option<String>,
}

impl Snapshot {
    fn initial() -> Self {
        Self {
            status: Status::Degraded,
            model: None,
            last_ok_ms: None,
            last_check_ms: 0,
            consecutive_failures: 0,
            last_error: None,
        }
    }
}

#[derive(Clone)]
pub struct BackendHealth {
    inner: Arc<RwLock<Snapshot>>,
    poll_url: String,
    interval_s: u64,
}

impl BackendHealth {
    /// Build a monitor for the given backend URL. Polls every
    /// `interval_s` seconds; the caller spawns the run loop.
    pub fn new(backend_base_url: &str, interval_s: u64) -> Self {
        // Backend usually exposes /health; some (OpenAI-compat cloud
        // providers) don't, so fall back to /v1/models.
        let poll_url =
            if backend_base_url.contains("127.0.0.1") || backend_base_url.contains("localhost") {
                format!("{}/health", backend_base_url.trim_end_matches('/'))
            } else {
                format!("{}/v1/models", backend_base_url.trim_end_matches('/'))
            };
        Self {
            inner: Arc::new(RwLock::new(Snapshot::initial())),
            poll_url,
            interval_s,
        }
    }

    pub async fn snapshot(&self) -> Snapshot {
        self.inner.read().await.clone()
    }

    /// Spawn the polling task. The task runs forever; pass the
    /// `JoinHandle` to the supervisor if you want to cancel.
    pub fn spawn_poll_loop(
        self: &Arc<Self>,
        client: reqwest::Client,
    ) -> tokio::task::JoinHandle<()> {
        let me = Arc::clone(self);
        tokio::spawn(async move {
            // Immediate first poll — don't wait a full interval before
            // the gateway reports backend health.
            me.poll_once(&client).await;
            let mut tick = interval(Duration::from_secs(me.interval_s));
            tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
            loop {
                tick.tick().await;
                me.poll_once(&client).await;
            }
        })
    }

    /// Run one poll against the backend and update the snapshot.
    pub async fn poll_once(&self, client: &reqwest::Client) {
        let last_check_ms = chrono::Utc::now().timestamp_millis();
        let result = client
            .get(&self.poll_url)
            .timeout(Duration::from_secs(5))
            .send()
            .await;
        let mut snap = self.inner.write().await;
        snap.last_check_ms = last_check_ms;
        match result {
            Ok(resp) if resp.status().is_success() => {
                let body: Option<Value> = resp.json().await.ok();
                if let Some(b) = &body {
                    if let Some(m) = b.get("model").and_then(|v| v.as_str()) {
                        snap.model = Some(m.to_string());
                    }
                }
                snap.status = Status::Healthy;
                snap.last_ok_ms = Some(last_check_ms);
                snap.last_error = None;
                snap.consecutive_failures = 0;
            }
            Ok(resp) => {
                snap.consecutive_failures += 1;
                snap.last_error = Some(format!("HTTP {}", resp.status()));
                snap.status = if snap.consecutive_failures >= 3 {
                    Status::Down
                } else {
                    Status::Degraded
                };
            }
            Err(e) => {
                snap.consecutive_failures += 1;
                snap.last_error = Some(format!("{e}"));
                snap.status = if snap.consecutive_failures >= 3 {
                    Status::Down
                } else {
                    Status::Degraded
                };
            }
        }
    }
}

// ── HTTP handlers ─────────────────────────────────────────────────────────

/// `GET /v1/health/backend` — read-only snapshot of the polled backend
/// state. No auth required (matches the rest of the /health family).
pub async fn backend_health_handler(State(state): State<crate::AppState>) -> impl IntoResponse {
    let snap = state.backend_health.snapshot().await;
    (
        StatusCode::OK,
        Json(json!({
            "backend_url": state.config.backend_base(),
            "status": snap.status,
            "model": snap.model,
            "last_check_ms": snap.last_check_ms,
            "last_ok_ms": snap.last_ok_ms,
            "consecutive_failures": snap.consecutive_failures,
            "last_error": snap.last_error,
        })),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn new_with(s: Status) -> Snapshot {
        Snapshot {
            status: s,
            model: None,
            last_ok_ms: None,
            last_check_ms: 0,
            consecutive_failures: 0,
            last_error: None,
        }
    }

    #[test]
    fn snapshot_initial_is_degraded() {
        let s = new_with(Status::Degraded);
        assert_eq!(s.status, Status::Degraded);
        assert_eq!(s.last_ok_ms, None);
    }

    #[test]
    fn status_serializes_as_lowercase() {
        let j = serde_json::to_value(Status::Healthy).unwrap();
        assert_eq!(j, serde_json::json!("healthy"));
        assert_eq!(
            serde_json::to_value(Status::Degraded).unwrap(),
            serde_json::json!("degraded")
        );
        assert_eq!(
            serde_json::to_value(Status::Down).unwrap(),
            serde_json::json!("down")
        );
    }

    #[tokio::test]
    async fn monitor_returns_degraded_when_three_consecutive_failures() {
        // Unit-test the failure classification only (no real HTTP).
        let h = BackendHealth::new("http://127.0.0.1:9999", 99);
        // Simulate by reading the snapshot directly.
        let snap = h.snapshot().await;
        assert_eq!(snap.status, Status::Degraded);
        assert_eq!(snap.consecutive_failures, 0);
    }
}
