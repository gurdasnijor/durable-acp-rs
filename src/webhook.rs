use std::collections::HashMap;
use std::time::Duration;

use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use tokio::sync::broadcast;
use tracing::{debug, warn};

use crate::state::{
    CollectionChange, PendingRequestState, PermissionRow, PromptTurnRow, PromptTurnState, StreamDb,
};

#[derive(Debug, Clone, Deserialize)]
pub struct WebhookConfig {
    pub url: String,
    #[serde(default = "default_events")]
    pub events: Vec<String>,
    pub secret: Option<String>,
}

fn default_events() -> Vec<String> {
    vec!["*".to_string()]
}

/// RFC-aligned payload: { sessionId, eventId, timestamp, event: { type, data } }
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct WebhookPayload {
    session_id: String,
    event_id: String,
    timestamp: String,
    event: WebhookEvent,
}

#[derive(Debug, Serialize)]
struct WebhookEvent {
    #[serde(rename = "type")]
    event_type: String,
    data: serde_json::Value,
}

/// Retry schedule per RFC: 5s, 30s, 2m, 10m, 30m
const RETRY_DELAYS: [Duration; 5] = [
    Duration::from_secs(5),
    Duration::from_secs(30),
    Duration::from_secs(120),
    Duration::from_secs(600),
    Duration::from_secs(1800),
];

/// Spawn a background task that forwards coalesced state transitions to webhook URLs.
pub fn spawn_forwarder(
    stream_db: StreamDb,
    webhooks: Vec<WebhookConfig>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        run_forwarder(stream_db, webhooks).await;
    })
}

async fn run_forwarder(stream_db: StreamDb, webhooks: Vec<WebhookConfig>) {
    if webhooks.is_empty() {
        return;
    }

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .expect("build reqwest client");

    let mut rx = stream_db.subscribe_changes();

    // Track previous states to detect transitions
    let mut prev_turn_states: HashMap<String, PromptTurnState> = HashMap::new();
    let mut prev_perm_states: HashMap<String, PendingRequestState> = HashMap::new();

    loop {
        match rx.recv().await {
            Ok(change) => {
                let snapshot = stream_db.snapshot().await;
                let events = match change {
                    CollectionChange::PromptTurns => {
                        detect_turn_events(&snapshot.prompt_turns, &mut prev_turn_states)
                    }
                    CollectionChange::Permissions => {
                        detect_permission_events(&snapshot.permissions, &mut prev_perm_states)
                    }
                    _ => vec![],
                };

                for payload in events {
                    let body = match serde_json::to_string(&payload) {
                        Ok(b) => b,
                        Err(e) => {
                            warn!(error = %e, "webhook: failed to serialize payload");
                            continue;
                        }
                    };

                    for hook in &webhooks {
                        if !matches_filter(&hook.events, &payload.event.event_type) {
                            continue;
                        }
                        dispatch_with_retries(&client, hook, &body, &payload).await;
                    }
                }
            }
            Err(broadcast::error::RecvError::Lagged(n)) => {
                warn!(skipped = n, "webhook forwarder lagged, events dropped");
                // Re-sync state maps after lag
                let snapshot = stream_db.snapshot().await;
                prev_turn_states = snapshot
                    .prompt_turns
                    .iter()
                    .map(|(k, v)| (k.clone(), v.state.clone()))
                    .collect();
                prev_perm_states = snapshot
                    .permissions
                    .iter()
                    .map(|(k, v)| (k.clone(), v.state.clone()))
                    .collect();
            }
            Err(broadcast::error::RecvError::Closed) => {
                debug!("webhook forwarder: channel closed, shutting down");
                return;
            }
        }
    }
}

/// Detect prompt turn completions and errors by comparing current vs previous state.
fn detect_turn_events(
    turns: &std::collections::BTreeMap<String, PromptTurnRow>,
    prev: &mut HashMap<String, PromptTurnState>,
) -> Vec<WebhookPayload> {
    let mut payloads = Vec::new();

    for (id, turn) in turns {
        let prev_state = prev.get(id);
        let is_new_terminal = match &turn.state {
            PromptTurnState::Completed | PromptTurnState::Cancelled | PromptTurnState::TimedOut => {
                prev_state.map_or(true, |s| !is_terminal_state(s))
            }
            PromptTurnState::Broken => prev_state.map_or(true, |s| !is_terminal_state(s)),
            _ => false,
        };

        if is_new_terminal {
            let (event_type, data) = if turn.state == PromptTurnState::Broken {
                (
                    "error",
                    serde_json::json!({
                        "promptTurnId": turn.prompt_turn_id,
                        "stopReason": turn.stop_reason,
                        "text": turn.text,
                    }),
                )
            } else {
                (
                    "end_turn",
                    serde_json::json!({
                        "promptTurnId": turn.prompt_turn_id,
                        "stopReason": turn.stop_reason,
                        "text": turn.text,
                    }),
                )
            };

            payloads.push(WebhookPayload {
                session_id: turn.session_id.clone(),
                event_id: uuid::Uuid::new_v4().to_string(),
                timestamp: iso_now(),
                event: WebhookEvent {
                    event_type: event_type.to_string(),
                    data,
                },
            });
        }

        prev.insert(id.clone(), turn.state.clone());
    }

    payloads
}

/// Detect new pending permission requests.
fn detect_permission_events(
    permissions: &std::collections::BTreeMap<String, PermissionRow>,
    prev: &mut HashMap<String, PendingRequestState>,
) -> Vec<WebhookPayload> {
    let mut payloads = Vec::new();

    for (id, perm) in permissions {
        let is_new_pending =
            perm.state == PendingRequestState::Pending && !prev.contains_key(id);

        if is_new_pending {
            payloads.push(WebhookPayload {
                session_id: perm.session_id.clone(),
                event_id: uuid::Uuid::new_v4().to_string(),
                timestamp: iso_now(),
                event: WebhookEvent {
                    event_type: "permission_request".to_string(),
                    data: serde_json::json!({
                        "requestId": perm.request_id,
                        "promptTurnId": perm.prompt_turn_id,
                        "title": perm.title,
                        "options": perm.options,
                    }),
                },
            });
        }

        prev.insert(id.clone(), perm.state.clone());
    }

    payloads
}

fn is_terminal_state(state: &PromptTurnState) -> bool {
    matches!(
        state,
        PromptTurnState::Completed
            | PromptTurnState::Cancelled
            | PromptTurnState::Broken
            | PromptTurnState::TimedOut
    )
}

async fn dispatch_with_retries(
    client: &reqwest::Client,
    hook: &WebhookConfig,
    body: &str,
    payload: &WebhookPayload,
) {
    for attempt in 0..=RETRY_DELAYS.len() {
        if attempt > 0 {
            tokio::time::sleep(RETRY_DELAYS[attempt - 1]).await;
        }

        if try_dispatch(client, hook, body, payload).await {
            return;
        }

        if attempt < RETRY_DELAYS.len() {
            warn!(
                url = %hook.url,
                attempt = attempt + 1,
                next_retry_secs = RETRY_DELAYS[attempt].as_secs(),
                "webhook delivery failed, retrying"
            );
        }
    }

    warn!(url = %hook.url, event_id = %payload.event_id, "webhook delivery failed after all retries");
}

/// Returns true on successful delivery (2xx).
async fn try_dispatch(
    client: &reqwest::Client,
    hook: &WebhookConfig,
    body: &str,
    payload: &WebhookPayload,
) -> bool {
    let mut req = client
        .post(&hook.url)
        .header("Content-Type", "application/json")
        .header("X-Flamecast-Session-Id", &payload.session_id)
        .header("X-Flamecast-Event-Id", &payload.event_id)
        .body(body.to_string());

    if let Some(secret) = &hook.secret {
        let signature = compute_hmac(secret, body);
        req = req.header("X-Webhook-Signature", format!("sha256={signature}"));
    }

    match req.send().await {
        Ok(resp) if resp.status().is_success() => {
            debug!(url = %hook.url, event_id = %payload.event_id, "webhook delivered");
            true
        }
        Ok(resp) => {
            warn!(url = %hook.url, status = %resp.status(), "webhook: non-2xx response");
            false
        }
        Err(e) => {
            warn!(url = %hook.url, error = %e, "webhook: request error");
            false
        }
    }
}

fn compute_hmac(secret: &str, body: &str) -> String {
    let mut mac =
        Hmac::<Sha256>::new_from_slice(secret.as_bytes()).expect("HMAC accepts any key size");
    mac.update(body.as_bytes());
    let bytes = mac.finalize().into_bytes();
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn matches_filter(events: &[String], event_type: &str) -> bool {
    events.iter().any(|e| e == "*" || e == event_type)
}

fn iso_now() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("time")
        .as_secs();
    // Simple ISO 8601 without pulling in chrono
    let days = secs / 86400;
    let time_secs = secs % 86400;
    let h = time_secs / 3600;
    let m = (time_secs % 3600) / 60;
    let s = time_secs % 60;
    // Days since 1970-01-01 → date
    let (y, mo, d) = days_to_ymd(days);
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{m:02}:{s:02}Z")
}

fn days_to_ymd(mut days: u64) -> (u64, u64, u64) {
    // Algorithm from https://howardhinnant.github.io/date_algorithms.html
    days += 719468;
    let era = days / 146097;
    let doe = days - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}
