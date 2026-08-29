//! Latest rate-limit state reported by a coding agent.
//!
//! Claude Code emits a `rate_limit_event` on its stream. The payload is
//! account-wide rather than per-session, and it only arrives while an agent is
//! running, so it does not belong in a session transcript and cannot be polled
//! for. This keeps the most recent report per limit window in memory for the
//! server to serve.
//!
//! A real payload looks like:
//!
//! ```json
//! {"status":"allowed","resetsAt":1788050400,"rateLimitType":"five_hour",
//!  "overageStatus":"rejected","isUsingOverage":false}
//! ```
//!
//! Note there is no utilisation percentage. The agent reports which window is
//! in force, whether requests are currently allowed, and when the window resets
//! — not how much of it has been consumed.

use std::{
    collections::HashMap,
    sync::{OnceLock, RwLock},
};

use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct RateLimitSnapshot {
    /// Window this describes, e.g. `five_hour` or `weekly`. Also the store key,
    /// so windows do not overwrite each other.
    pub rate_limit_type: String,
    /// Agent-reported status, e.g. `allowed`. Anything else means requests are
    /// being refused, which is the case worth surfacing prominently.
    pub status: String,
    /// Unix epoch seconds at which this window resets.
    pub resets_at: Option<i64>,
    pub is_using_overage: Option<bool>,
    pub overage_status: Option<String>,
    /// When this was observed, so a consumer can tell stale reports from fresh
    /// ones. Events only arrive during a run, so this can be well in the past.
    pub observed_at: String,
}

fn store() -> &'static RwLock<HashMap<String, RateLimitSnapshot>> {
    static STORE: OnceLock<RwLock<HashMap<String, RateLimitSnapshot>>> = OnceLock::new();
    STORE.get_or_init(|| RwLock::new(HashMap::new()))
}

/// Record a `rate_limit_info` payload. Unparseable or unkeyed payloads are
/// ignored: this is telemetry for display, never worth failing a run over.
pub fn record(info: &serde_json::Value) {
    let Some(rate_limit_type) = info
        .get("rateLimitType")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
    else {
        return;
    };

    let snapshot = RateLimitSnapshot {
        rate_limit_type: rate_limit_type.to_string(),
        status: info
            .get("status")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_string(),
        resets_at: info.get("resetsAt").and_then(|v| v.as_i64()),
        is_using_overage: info.get("isUsingOverage").and_then(|v| v.as_bool()),
        overage_status: info
            .get("overageStatus")
            .and_then(|v| v.as_str())
            .map(String::from),
        observed_at: chrono::Utc::now().to_rfc3339(),
    };

    if let Ok(mut guard) = store().write() {
        guard.insert(rate_limit_type.to_string(), snapshot);
    }
}

/// Every window seen so far this run, most recently reset first.
pub fn snapshots() -> Vec<RateLimitSnapshot> {
    let Ok(guard) = store().read() else {
        return Vec::new();
    };
    let mut out: Vec<_> = guard.values().cloned().collect();
    out.sort_by_key(|s| s.resets_at.unwrap_or(i64::MAX));
    out
}
