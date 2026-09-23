use log::{debug, error, info, warn};
use serde_json::{Value, json};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use crate::config::ILConfig;
use crate::json_util::get_nested_value;
use crate::models::event::EventQueueItemJson;
use crate::{DaemonContext, interruptible_sleep};

const REQUEST_TIMEOUT_SECS: u64 = 30;
const BACKOFF_BASE_SECS: u64 = 5;
const BACKOFF_MAX_SECS: u64 = 300;
const OAUTH_REFRESH_MARGIN_SECS: u64 = 120;

/// Dedup markers older than this are swept; a record reappearing after that is re-sent.
pub const DEDUP_TTL_SECS: i64 = 604800; // 7 days

/// Pages per cycle, mirroring MAX_CONSECUTIVE_REPOLLS in the edge connector.
pub const MAX_CONSECUTIVE_PAGES: u32 = 10;

/// Skip a fetch while this many events are queued; poll.rs drains slower than we fetch.
pub const QUEUE_BACKPRESSURE_THRESHOLD: i64 = 5000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CursorMode {
    None,
    Token,
    Timestamp,
}

impl CursorMode {
    pub fn from_config(config: &ILConfig) -> CursorMode {
        match config.pull_cursor_mode.as_deref() {
            Some("token") => CursorMode::Token,
            Some("timestamp") => CursorMode::Timestamp,
            _ => CursorMode::None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimestampFormat {
    EpochS,
    EpochMs,
    Iso8601,
}

impl TimestampFormat {
    pub fn from_config(config: &ILConfig) -> TimestampFormat {
        match config.pull_cursor_format.as_deref() {
            Some("epoch_s") => TimestampFormat::EpochS,
            Some("epoch_ms") => TimestampFormat::EpochMs,
            _ => TimestampFormat::Iso8601,
        }
    }
}

/// Unauthorized is its own variant so the oauth retry cannot be triggered by an
/// unrelated error whose message happens to contain "401".
#[derive(Debug)]
pub enum PullError {
    Unauthorized,
    Other(String),
}

impl std::fmt::Display for PullError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PullError::Unauthorized => write!(f, "unauthorized (401) — check pull credentials"),
            PullError::Other(msg) => write!(f, "{}", msg),
        }
    }
}

impl From<String> for PullError {
    fn from(msg: String) -> Self {
        PullError::Other(msg)
    }
}

impl From<&str> for PullError {
    fn from(msg: &str) -> Self {
        PullError::Other(msg.to_string())
    }
}

struct OauthToken {
    access_token: String,
    expires_at: Instant,
}

impl OauthToken {
    fn is_valid(&self) -> bool {
        Instant::now() < self.expires_at
    }
}

pub fn cursor_db_key(name: &str) -> String {
    format!("pull_cursor:{}", name)
}

/// Exact key match before dots are read as nesting, for vendor keys like `@odata.count`.
/// Local rather than in json_util so the shared mapping behaviour is untouched.
fn lookup_path<'a>(json: &'a Value, path: &str) -> Option<&'a Value> {
    if let Some(val) = json.get(path) {
        return Some(val);
    }
    get_nested_value(json, path)
}

/// Record array from a body; no path means the whole body, an object means one record.
pub fn extract_items(body: &Value, items_path: Option<&str>) -> Vec<Value> {
    let target = match items_path {
        Some(path) => match lookup_path(body, path) {
            Some(val) => val,
            None => {
                warn!("pull items path '{}' not found in response", path);
                return Vec::new();
            }
        },
        None => body,
    };

    match target {
        Value::Array(items) => items.clone(),
        Value::Object(_) => vec![target.clone()],
        other => {
            warn!(
                "pull items path resolved to a non-object value: {:?}, ignoring",
                other
            );
            Vec::new()
        }
    }
}

pub fn parse_timestamp(val: &Value, format: TimestampFormat) -> Option<i64> {
    match format {
        TimestampFormat::EpochS => match val {
            Value::Number(n) => n.as_i64().map(|s| s * 1000),
            Value::String(s) => s.parse::<i64>().ok().map(|s| s * 1000),
            _ => None,
        },
        TimestampFormat::EpochMs => match val {
            Value::Number(n) => n.as_i64(),
            Value::String(s) => s.parse::<i64>().ok(),
            _ => None,
        },
        TimestampFormat::Iso8601 => val.as_str().and_then(|s| {
            chrono::DateTime::parse_from_rfc3339(s)
                .ok()
                .map(|dt| dt.timestamp_millis())
        }),
    }
}

pub fn render_timestamp(millis: i64, format: TimestampFormat) -> String {
    match format {
        TimestampFormat::EpochS => (millis / 1000).to_string(),
        TimestampFormat::EpochMs => millis.to_string(),
        TimestampFormat::Iso8601 => chrono::DateTime::from_timestamp_millis(millis)
            .map(|dt| dt.to_rfc3339_opts(chrono::SecondsFormat::Millis, true))
            .unwrap_or_default(),
    }
}

/// Highest timestamp on a page, plus distinct count — a page sharing one timestamp
/// cannot advance an inclusive cursor.
pub fn max_timestamp(items: &[Value], path: &str, format: TimestampFormat) -> (Option<i64>, usize) {
    let mut seen: Vec<i64> = Vec::new();
    for item in items {
        if let Some(val) = lookup_path(item, path) {
            if let Some(millis) = parse_timestamp(val, format) {
                if !seen.contains(&millis) {
                    seen.push(millis);
                }
            } else {
                warn!(
                    "pull cursor path '{}' matched an unparseable timestamp: {:?}",
                    path, val
                );
            }
        }
    }
    (seen.iter().copied().max(), seen.len())
}

pub fn validate_pull_config(config: &ILConfig) {
    if config.pull_name.is_none() {
        panic!(
            "--pull_name is required for pull sources (it keys the stored cursor and dedup rows)"
        );
    }

    if config.pull_interval < 10 || config.pull_interval > 3600 {
        panic!("--pull_interval must be between 10 and 3600 seconds");
    }

    if let Some(ref body) = config.pull_body {
        if serde_json::from_str::<Value>(body).is_err() {
            panic!("--pull_body must be valid JSON");
        }
    }

    if config.event_key.is_none() {
        panic!(
            "--event_key is required for pull sources — vendor payloads carry no ilert integration key, and events without one are rejected by the events API"
        );
    }

    match config.pull_auth.as_deref() {
        Some("header") => {
            if config.pull_auth_header.is_none() {
                panic!("--pull_auth_header is required when pull_auth is 'header'");
            }
            if config.pull_auth_value.is_none() {
                panic!(
                    "--pull_auth_value (or ILERT_PULL_AUTH_VALUE) is required when pull_auth is 'header'"
                );
            }
        }
        Some("basic") => {
            if config.pull_username.is_none() {
                panic!("--pull_username is required when pull_auth is 'basic'");
            }
            if config.pull_password.is_none() {
                panic!(
                    "--pull_password (or ILERT_PULL_PASSWORD) is required when pull_auth is 'basic'"
                );
            }
        }
        Some("oauth2") => {
            if config.pull_token_url.is_none() {
                panic!("--pull_token_url is required when pull_auth is 'oauth2'");
            }
            if config.pull_client_id.is_none() {
                panic!("--pull_client_id is required when pull_auth is 'oauth2'");
            }
            if config.pull_client_secret.is_none() {
                panic!(
                    "--pull_client_secret (or ILERT_PULL_CLIENT_SECRET) is required when pull_auth is 'oauth2'"
                );
            }
        }
        Some("none") | None => {}
        Some(other) => panic!(
            "Unknown pull_auth '{}', expected: none, header, basic, oauth2",
            other
        ),
    }

    // fetch_page dispatches on pull_auth alone, so a credential without its mode is
    // dropped and every request goes out unauthenticated.
    let selected = config.pull_auth.as_deref().unwrap_or("none");
    for (mode, flags) in [
        (
            "header",
            vec![
                ("--pull_auth_header", config.pull_auth_header.is_some()),
                ("--pull_auth_value", config.pull_auth_value.is_some()),
            ],
        ),
        (
            "basic",
            vec![
                ("--pull_username", config.pull_username.is_some()),
                ("--pull_password", config.pull_password.is_some()),
            ],
        ),
        (
            "oauth2",
            vec![
                ("--pull_token_url", config.pull_token_url.is_some()),
                ("--pull_client_id", config.pull_client_id.is_some()),
                ("--pull_client_secret", config.pull_client_secret.is_some()),
                ("--pull_scope", config.pull_scope.is_some()),
            ],
        ),
    ] {
        if selected == mode {
            continue;
        }
        for (flag, is_set) in flags {
            if is_set {
                panic!(
                    "{} requires --pull_auth {}, but pull_auth is '{}' — the credential would be ignored and every request sent unauthenticated",
                    flag, mode, selected
                );
            }
        }
    }

    let mode = CursorMode::from_config(config);

    match mode {
        CursorMode::Token | CursorMode::Timestamp => {
            if config.pull_cursor_path.is_none() {
                panic!("--pull_cursor_path is required for this cursor mode");
            }
            if config.pull_cursor_param.is_none() {
                panic!("--pull_cursor_param is required for this cursor mode");
            }
        }
        CursorMode::None => {}
    }

    // The cursor is a query param, which a body-bearing endpoint ignores.
    if config.pull_body.is_some() && matches!(mode, CursorMode::Token | CursorMode::Timestamp) {
        panic!(
            "--pull_body cannot be combined with pull_cursor_mode '{}' — the cursor is sent as a query parameter and a body-bearing endpoint would ignore it",
            config.pull_cursor_mode.as_deref().unwrap_or("")
        );
    }

    // Inclusive `since` semantics mean the boundary records return on every poll.
    if mode == CursorMode::Timestamp && config.pull_dedup_key.is_none() {
        panic!(
            "--pull_dedup_key is required when pull_cursor_mode is 'timestamp' — timestamp boundaries are inclusive, so the newest records are re-delivered on every poll without it"
        );
    }

    if mode == CursorMode::None && config.pull_dedup_key.is_none() {
        warn!(
            "Pull source has no cursor and no --pull_dedup_key: every record will be re-sent on every poll, relying entirely on ilert's alertKey deduplication"
        );
    }

    // Only a warning: a durable watermark needs no dedup, a pagination handle does.
    if mode == CursorMode::Token && config.pull_dedup_key.is_none() {
        warn!(
            "Pull source uses a token cursor without --pull_dedup_key: if this API's cursor only pages through a result set rather than marking a durable position, the last page will be re-sent on every poll"
        );
    }
}

async fn fetch_oauth_token(
    client: &reqwest::Client,
    config: &ILConfig,
) -> Result<OauthToken, String> {
    let token_url = config.pull_token_url.as_ref().ok_or("missing token url")?;
    let client_id = config.pull_client_id.as_ref().ok_or("missing client id")?;
    let client_secret = config
        .pull_client_secret
        .as_ref()
        .ok_or("missing client secret")?;

    let mut form = vec![
        ("grant_type", "client_credentials"),
        ("client_id", client_id.as_str()),
        ("client_secret", client_secret.as_str()),
    ];
    if let Some(ref scope) = config.pull_scope {
        form.push(("scope", scope.as_str()));
    }

    let response = client
        .post(token_url)
        .form(&form)
        .send()
        .await
        // without_url: reqwest's Display renders the full url, query string included.
        .map_err(|e| format!("oauth token request failed: {}", e.without_url()))?;

    let status = response.status();
    if !status.is_success() {
        return Err(format!("oauth token request returned status {}", status));
    }

    let body: Value = response
        .json()
        .await
        .map_err(|e| format!("failed to parse oauth token response: {}", e.without_url()))?;

    let access_token = body
        .get("access_token")
        .and_then(|v| v.as_str())
        .ok_or("oauth token response has no access_token")?
        .to_string();

    // Refresh ahead of expiry so an in-flight request cannot land on a dead token.
    let expires_in = body
        .get("expires_in")
        .and_then(|v| v.as_u64())
        .unwrap_or(3600);
    let lifetime = expires_in.saturating_sub(OAUTH_REFRESH_MARGIN_SECS).max(1);

    debug!("Obtained oauth token, valid for {}s", lifetime);

    Ok(OauthToken {
        access_token,
        expires_at: Instant::now() + Duration::from_secs(lifetime),
    })
}

/// One HTTP request. `cursor` is already resolved to whatever should be sent this call.
async fn fetch_page(
    client: &reqwest::Client,
    config: &ILConfig,
    mode: CursorMode,
    cursor: Option<&str>,
    bearer: Option<&str>,
) -> Result<Value, PullError> {
    let url = config.pull_url.as_ref().ok_or("pull_url is not set")?;

    let mut request = match config.pull_body {
        Some(ref body) => client
            .post(url)
            .header("Content-Type", "application/json")
            .body(body.clone()),
        None => client.get(url),
    };

    request = request.header("User-Agent", crate::CALLER_AGENT);

    for token in &config.pull_headers {
        match token.split_once('=') {
            Some((key, value)) if !key.trim().is_empty() => {
                request = request.header(key.trim(), value);
            }
            _ => {
                // Only the leading name is logged: a ':' where a '=' belongs leaves a
                // live credential in the remainder.
                let name = token
                    .split([':', '=', ' '])
                    .next()
                    .unwrap_or_default()
                    .trim();
                warn!(
                    "Ignoring malformed --pull_header starting '{}', expected key=value",
                    name
                );
            }
        }
    }

    match config.pull_auth.as_deref() {
        Some("header") => {
            if let (Some(name), Some(value)) = (&config.pull_auth_header, &config.pull_auth_value) {
                request = request.header(name.as_str(), value.as_str());
            }
        }
        Some("basic") => {
            if let Some(ref username) = config.pull_username {
                request = request.basic_auth(username, config.pull_password.as_ref());
            }
        }
        Some("oauth2") => {
            if let Some(token) = bearer {
                request = request.header("Authorization", format!("Bearer {}", token));
            }
        }
        _ => {}
    }

    if matches!(mode, CursorMode::Token | CursorMode::Timestamp) {
        if let (Some(param), Some(value)) = (&config.pull_cursor_param, cursor) {
            request = request.query(&[(param.as_str(), value)]);
        }
    }

    let response = request
        .send()
        .await
        // without_url: this string is logged and served as the `error` field of /ready.
        .map_err(|e| PullError::Other(format!("pull request failed: {}", e.without_url())))?;

    let status = response.status();
    if status.as_u16() == 401 {
        return Err(PullError::Unauthorized);
    }
    if !status.is_success() {
        return Err(PullError::Other(format!(
            "pull request returned status {}",
            status
        )));
    }

    response.json::<Value>().await.map_err(|e| {
        PullError::Other(format!(
            "failed to parse pull response as json: {}",
            e.without_url()
        ))
    })
}

/// Enqueue one record. Returns true when it was inserted, false when deduped or filtered.
async fn enqueue_record(
    ctx: &Arc<DaemonContext>,
    source: &str,
    record: &Value,
) -> Result<bool, String> {
    let config = &ctx.config;

    let dedup_key = match config.pull_dedup_key {
        Some(ref path) => match lookup_path(record, path) {
            Some(Value::String(s)) => Some(s.clone()),
            Some(Value::Number(n)) => Some(n.to_string()),
            Some(other) => {
                warn!(
                    "pull dedup key '{}' matched an unusable value: {:?}, record will not be deduped",
                    path, other
                );
                None
            }
            None => {
                warn!(
                    "pull dedup key '{}' missing from record, record will not be deduped",
                    path
                );
                None
            }
        },
        None => None,
    };

    if let Some(ref key) = dedup_key {
        let seen = ctx
            .db
            .lock()
            .await
            .pull_dedup_seen(source, key)
            .map_err(|e| format!("dedup lookup failed: {}", e))?;
        if seen {
            debug!("Skipping already-seen record {} from '{}'", key, source);
            return Ok(false);
        }
    }

    let payload = record.to_string();
    let default_details = json!({ "source": source });
    let event =
        match crate::consumers::prepare_consumer_event(config, &payload, source, default_details) {
            Some(event) => event,
            None => return Ok(false), // filtered or unparseable, already logged
        };

    // No typed api path: pull events post to plain /events, like the event subcommand.
    let db_event = EventQueueItemJson::to_db(event, None);

    let result =
        ctx.db
            .lock()
            .await
            .create_il_event_deduped(&db_event, source, dedup_key.as_deref());

    match result {
        Ok(Some(val)) => {
            info!(
                "Event {} from pull source '{}' added to queue",
                val.id.clone().unwrap_or_default(),
                source
            );
            Ok(true)
        }
        Ok(None) => Err("event insert returned no row".to_string()),
        Err(e) => Err(format!("event insert failed: {}", e)),
    }
}

/// Fetch and enqueue what is available, returning the count. An error leaves that page's
/// cursor untouched so it is retried.
async fn run_cycle(
    ctx: &Arc<DaemonContext>,
    client: &reqwest::Client,
    source: &str,
    mode: CursorMode,
    bearer: Option<&str>,
) -> Result<u64, PullError> {
    let config = &ctx.config;
    let format = TimestampFormat::from_config(config);
    let cursor_key = cursor_db_key(source);

    let mut cursor: Option<String> = ctx.db.lock().await.get_il_value(&cursor_key);

    // Nothing to resume from on a first run, and omitting the param is API-dependent.
    if mode == CursorMode::Timestamp && cursor.is_none() {
        let seed =
            chrono::Utc::now().timestamp_millis() - (config.pull_initial_lookback as i64 * 1000);
        cursor = Some(render_timestamp(seed, format));
        info!(
            "Pull source '{}' has no stored cursor, seeding {}s lookback: {}",
            source,
            config.pull_initial_lookback,
            cursor.clone().unwrap_or_default()
        );
    }

    let mut enqueued: u64 = 0;
    let mut pages: u32 = 0;

    loop {
        pages += 1;

        let body = fetch_page(client, config, mode, cursor.as_deref(), bearer).await?;
        let items = extract_items(&body, config.pull_items_path.as_deref());

        if items.is_empty() {
            debug!("Pull source '{}' returned no records", source);
            break;
        }

        debug!(
            "Pull source '{}' page {} returned {} records",
            source,
            pages,
            items.len()
        );

        for record in &items {
            if enqueue_record(ctx, source, record).await? {
                enqueued += 1;
            }
        }

        if mode == CursorMode::None {
            break; // snapshot sources have no notion of a next page
        }

        let next = match mode {
            CursorMode::Token => config
                .pull_cursor_path
                .as_ref()
                .and_then(|path| lookup_path(&body, path))
                .and_then(|val| match val {
                    serde_json::Value::String(s) => Some(s.clone()),
                    serde_json::Value::Number(n) => Some(n.to_string()),
                    _ => None,
                }),
            CursorMode::Timestamp => {
                let path = config.pull_cursor_path.as_deref().unwrap_or_default();
                let (max, distinct) = max_timestamp(&items, path, format);

                if distinct == 1 && items.len() > 1 {
                    error!(
                        "Pull source '{}' returned {} records all sharing one timestamp — this source's timestamp resolution is too coarse to cursor reliably, and records beyond this page may never be fetched. Consider a different cursor mode.",
                        source,
                        items.len()
                    );
                }

                max.map(|millis| render_timestamp(millis, format))
            }
            CursorMode::None => None,
        };

        let Some(next) = next else {
            debug!(
                "Pull source '{}' returned no next cursor, cycle done",
                source
            );
            break;
        };

        // Only advance once every record on the page is committed.
        if cursor.as_deref() == Some(next.as_str()) {
            debug!("Pull source '{}' cursor unchanged, cycle done", source);
            break;
        }

        if let Err(e) = ctx.db.lock().await.set_il_val(&cursor_key, &next) {
            return Err(PullError::Other(format!(
                "failed to persist pull cursor: {}",
                e
            )));
        }
        cursor = Some(next);

        if pages >= MAX_CONSECUTIVE_PAGES {
            debug!(
                "Pull source '{}' reached max consecutive pages ({}), pausing until next interval",
                source, MAX_CONSECUTIVE_PAGES
            );
            break;
        }
    }

    Ok(enqueued)
}

pub async fn run_pull_job(ctx: Arc<DaemonContext>) {
    let source = ctx
        .config
        .pull_name
        .clone()
        .expect("pull_name is required, validated at startup");
    let interval = ctx.config.pull_interval;
    let mode = CursorMode::from_config(&ctx.config);
    let uses_oauth = ctx.config.pull_auth.as_deref() == Some("oauth2");

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(REQUEST_TIMEOUT_SECS))
        .build()
        .expect("failed to create HTTP client for pull connector");

    let mut token: Option<OauthToken> = None;
    let mut consecutive_failures: u32 = 0;

    if let Some(ref probe) = ctx.pull_probe {
        probe.polling.store(true, Ordering::Relaxed);
    }

    // The url is not logged: it routinely carries a key or token in its query string.
    info!(
        "Pull source '{}' polling every {}s (cursor: {:?})",
        source, interval, mode
    );

    loop {
        if !ctx.running.load(Ordering::Relaxed) {
            break;
        }

        // Fetching while the queue is already deep only grows the backlog.
        let depth = ctx.db.lock().await.count_il_events().unwrap_or(0);
        if depth >= QUEUE_BACKPRESSURE_THRESHOLD {
            warn!(
                "Pull source '{}' skipping cycle, {} events still queued (threshold {})",
                source, depth, QUEUE_BACKPRESSURE_THRESHOLD
            );
            interruptible_sleep(&ctx, interval).await;
            continue;
        }

        if uses_oauth && !token.as_ref().is_some_and(|t| t.is_valid()) {
            match fetch_oauth_token(&client, &ctx.config).await {
                Ok(fresh) => token = Some(fresh),
                Err(e) => {
                    consecutive_failures += 1;
                    let backoff = backoff_secs(consecutive_failures);
                    error!(
                        "Pull source '{}' oauth failed (attempt {}, backoff {}s): {}",
                        source, consecutive_failures, backoff, e
                    );
                    if let Some(ref probe) = ctx.pull_probe {
                        probe.record_error(e);
                    }
                    interruptible_sleep(&ctx, backoff).await;
                    continue;
                }
            }
        }

        let bearer = token.as_ref().map(|t| t.access_token.clone());
        let mut result = run_cycle(&ctx, &client, &source, mode, bearer.as_deref()).await;

        // A token can be revoked before its stated expiry; retry once with a fresh one.
        if uses_oauth && matches!(&result, Err(PullError::Unauthorized)) {
            info!(
                "Pull source '{}' got 401, refreshing token and retrying",
                source
            );
            token = None;
            match fetch_oauth_token(&client, &ctx.config).await {
                Ok(fresh) => {
                    let bearer = fresh.access_token.clone();
                    token = Some(fresh);
                    result = run_cycle(&ctx, &client, &source, mode, Some(&bearer)).await;
                }
                Err(e) => result = Err(PullError::Other(e)),
            }
        }

        match result {
            Ok(count) => {
                consecutive_failures = 0;
                if let Some(ref probe) = ctx.pull_probe {
                    probe.clear_error();
                    probe.items_fetched.fetch_add(count, Ordering::Relaxed);
                }
                if count > 0 {
                    info!("Pull source '{}' enqueued {} events", source, count);
                }

                if let Err(e) = ctx.db.lock().await.pull_dedup_sweep(DEDUP_TTL_SECS) {
                    warn!("Failed to sweep pull dedup table: {}", e);
                }
            }
            Err(e) => {
                consecutive_failures += 1;
                let backoff = backoff_secs(consecutive_failures);
                error!(
                    "Pull source '{}' cycle failed (attempt {}, backoff {}s): {}",
                    source, consecutive_failures, backoff, e
                );
                if let Some(ref probe) = ctx.pull_probe {
                    probe.record_error(e.to_string());
                }
                interruptible_sleep(&ctx, backoff).await;
                continue;
            }
        }

        interruptible_sleep(&ctx, interval).await;
    }

    info!("Pull source '{}' stopped", source);
}

fn backoff_secs(attempt: u32) -> u64 {
    // saturating_mul: saturating_pow alone still overflows the multiply past ~62.
    BACKOFF_BASE_SECS
        .saturating_mul(2u64.saturating_pow(attempt.saturating_sub(1)))
        .min(BACKOFF_MAX_SECS)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base_config() -> ILConfig {
        let mut config = ILConfig::new();
        config.pull_url = Some("https://example.com/api".to_string());
        config.pull_name = Some("test-source".to_string());
        config.event_key = Some("il1api123".to_string());
        config
    }

    // --- extract_items ---

    #[test]
    fn extract_items_from_nested_path() {
        let body = json!({"data": {"records": [{"id": 1}, {"id": 2}]}});
        let items = extract_items(&body, Some("data.records"));
        assert_eq!(items.len(), 2);
        assert_eq!(items[0]["id"], 1);
    }

    #[test]
    fn extract_items_from_bare_array() {
        let body = json!([{"id": 1}, {"id": 2}]);
        assert_eq!(extract_items(&body, None).len(), 2);
    }

    #[test]
    fn extract_items_wraps_single_object() {
        let body = json!({"result": {"id": 1}});
        let items = extract_items(&body, Some("result"));
        assert_eq!(items.len(), 1);
        assert_eq!(items[0]["id"], 1);
    }

    #[test]
    fn extract_items_missing_path_yields_nothing() {
        let body = json!({"items": [{"id": 1}]});
        assert!(extract_items(&body, Some("nope")).is_empty());
    }

    #[test]
    fn extract_items_non_object_scalar_yields_nothing() {
        let body = json!({"items": "not an array"});
        assert!(extract_items(&body, Some("items")).is_empty());
    }

    // --- lookup_path ---

    #[test]
    fn lookup_prefers_literal_key_containing_dots() {
        // A vendor key that literally contains a dot is one key, not a nested path
        let body = json!({"@odata.count": 7, "@odata": {"count": "wrong"}});
        assert_eq!(lookup_path(&body, "@odata.count").unwrap(), 7);
    }

    #[test]
    fn lookup_falls_back_to_nesting() {
        let body = json!({"a": {"b": "found"}});
        assert_eq!(lookup_path(&body, "a.b").unwrap(), "found");
    }

    // --- timestamps ---

    #[test]
    fn timestamp_roundtrip_all_formats() {
        let millis = 1_767_225_600_000i64;

        for (format, value) in [
            (TimestampFormat::EpochMs, json!(millis)),
            (TimestampFormat::EpochS, json!(millis / 1000)),
            (
                TimestampFormat::Iso8601,
                json!(render_timestamp(millis, TimestampFormat::Iso8601)),
            ),
        ] {
            let parsed = parse_timestamp(&value, format).expect("should parse");
            assert_eq!(parsed, millis, "roundtrip failed for {:?}", format);
            let rendered = render_timestamp(parsed, format);
            assert_eq!(
                parse_timestamp(&json!(rendered), format),
                Some(millis),
                "re-parsing a rendered value failed for {:?}",
                format
            );
        }
    }

    #[test]
    fn timestamp_parses_numeric_strings() {
        assert_eq!(
            parse_timestamp(&json!("1767225600"), TimestampFormat::EpochS),
            Some(1_767_225_600_000)
        );
    }

    #[test]
    fn timestamp_rejects_garbage() {
        assert_eq!(
            parse_timestamp(&json!("not a date"), TimestampFormat::Iso8601),
            None
        );
        assert_eq!(parse_timestamp(&json!(true), TimestampFormat::EpochS), None);
    }

    #[test]
    fn max_timestamp_takes_max_not_last() {
        let items = vec![json!({"ts": 300}), json!({"ts": 900}), json!({"ts": 100})];
        let (max, distinct) = max_timestamp(&items, "ts", TimestampFormat::EpochMs);
        assert_eq!(max, Some(900));
        assert_eq!(distinct, 3);
    }

    #[test]
    fn max_timestamp_counts_one_distinct_for_uniform_page() {
        let items = vec![json!({"ts": 500}), json!({"ts": 500}), json!({"ts": 500})];
        let (max, distinct) = max_timestamp(&items, "ts", TimestampFormat::EpochMs);
        assert_eq!(max, Some(500));
        assert_eq!(
            distinct, 1,
            "uniform page must be detectable as pathological"
        );
    }

    #[test]
    fn max_timestamp_skips_unparseable_records() {
        let items = vec![json!({"ts": 100}), json!({"ts": "junk"}), json!({})];
        let (max, distinct) = max_timestamp(&items, "ts", TimestampFormat::EpochMs);
        assert_eq!(max, Some(100));
        assert_eq!(distinct, 1);
    }

    // --- cursor mode / format parsing ---

    #[test]
    fn cursor_mode_parses_each_variant() {
        // An unset mode and an explicit "none" both resolve to the same default.
        assert_eq!(CursorMode::from_config(&ILConfig::new()), CursorMode::None);

        for (raw, expected) in [
            ("token", CursorMode::Token),
            ("timestamp", CursorMode::Timestamp),
            ("none", CursorMode::None),
        ] {
            let mut config = ILConfig::new();
            config.pull_cursor_mode = Some(raw.to_string());
            assert_eq!(CursorMode::from_config(&config), expected);
        }
    }

    #[test]
    fn timestamp_format_defaults_to_iso() {
        assert_eq!(
            TimestampFormat::from_config(&ILConfig::new()),
            TimestampFormat::Iso8601
        );
    }

    // --- backoff ---

    #[test]
    fn backoff_grows_then_caps() {
        assert_eq!(backoff_secs(1), 5);
        assert_eq!(backoff_secs(2), 10);
        assert_eq!(backoff_secs(3), 20);
        assert_eq!(backoff_secs(99), BACKOFF_MAX_SECS);
    }

    // --- validate_pull_config ---

    #[test]
    fn validate_accepts_minimal_snapshot_source() {
        validate_pull_config(&base_config());
    }

    #[test]
    #[should_panic(expected = "--pull_name is required")]
    fn validate_requires_name() {
        let mut config = base_config();
        config.pull_name = None;
        validate_pull_config(&config);
    }

    #[test]
    #[should_panic(expected = "--event_key is required")]
    fn validate_requires_event_key() {
        let mut config = base_config();
        config.event_key = None;
        validate_pull_config(&config);
    }

    #[test]
    #[should_panic(expected = "--pull_interval must be between")]
    fn validate_rejects_out_of_range_interval() {
        let mut config = base_config();
        config.pull_interval = 5;
        validate_pull_config(&config);
    }

    #[test]
    #[should_panic(expected = "--pull_body must be valid JSON")]
    fn validate_rejects_malformed_body() {
        let mut config = base_config();
        config.pull_body = Some("{not json".to_string());
        validate_pull_config(&config);
    }

    #[test]
    #[should_panic(expected = "cannot be combined with pull_cursor_mode")]
    fn validate_rejects_body_with_token_cursor() {
        let mut config = base_config();
        config.pull_body = Some("{}".to_string());
        config.pull_cursor_mode = Some("token".to_string());
        config.pull_cursor_path = Some("next".to_string());
        config.pull_cursor_param = Some("cursor".to_string());
        validate_pull_config(&config);
    }

    #[test]
    fn validate_allows_body_with_no_cursor() {
        let mut config = base_config();
        config.pull_body = Some(r#"{"jsonrpc":"2.0"}"#.to_string());
        config.pull_cursor_mode = Some("none".to_string());
        config.pull_dedup_key = Some("id".to_string());
        validate_pull_config(&config);
    }

    #[test]
    #[should_panic(expected = "--pull_dedup_key is required when pull_cursor_mode is 'timestamp'")]
    fn validate_requires_dedup_for_timestamp_mode() {
        let mut config = base_config();
        config.pull_cursor_mode = Some("timestamp".to_string());
        config.pull_cursor_path = Some("updated_at".to_string());
        config.pull_cursor_param = Some("since".to_string());
        validate_pull_config(&config);
    }

    #[test]
    fn validate_accepts_timestamp_mode_with_dedup() {
        let mut config = base_config();
        config.pull_cursor_mode = Some("timestamp".to_string());
        config.pull_cursor_path = Some("updated_at".to_string());
        config.pull_cursor_param = Some("since".to_string());
        config.pull_dedup_key = Some("id".to_string());
        validate_pull_config(&config);
    }

    #[test]
    #[should_panic(expected = "--pull_cursor_param is required")]
    fn validate_requires_cursor_param_for_token_mode() {
        let mut config = base_config();
        config.pull_cursor_mode = Some("token".to_string());
        config.pull_cursor_path = Some("next_cursor".to_string());
        validate_pull_config(&config);
    }

    #[test]
    #[should_panic(expected = "Unknown pull_auth")]
    fn validate_rejects_unknown_auth() {
        let mut config = base_config();
        config.pull_auth = Some("kerberos".to_string());
        validate_pull_config(&config);
    }

    #[test]
    #[should_panic(expected = "--pull_client_secret")]
    fn validate_oauth_requires_secret() {
        let mut config = base_config();
        config.pull_auth = Some("oauth2".to_string());
        config.pull_token_url = Some("https://id.example.com/token".to_string());
        config.pull_client_id = Some("cid".to_string());
        validate_pull_config(&config);
    }

    #[test]
    #[should_panic(expected = "--pull_auth_value")]
    fn validate_header_auth_requires_value() {
        let mut config = base_config();
        config.pull_auth = Some("header".to_string());
        config.pull_auth_header = Some("X-API-Key".to_string());
        validate_pull_config(&config);
    }

    // --- auth credentials supplied without the matching mode ---

    #[test]
    #[should_panic(expected = "--pull_auth_header requires --pull_auth header")]
    fn validate_rejects_header_credentials_without_mode() {
        // These flags would be dropped and every request sent unauthenticated.
        let mut config = base_config();
        config.pull_auth_header = Some("X-API-Key".to_string());
        config.pull_auth_value = Some("secret".to_string());
        validate_pull_config(&config);
    }

    #[test]
    #[should_panic(expected = "--pull_username requires --pull_auth basic")]
    fn validate_rejects_basic_credentials_without_mode() {
        let mut config = base_config();
        config.pull_username = Some("svc".to_string());
        config.pull_password = Some("secret".to_string());
        validate_pull_config(&config);
    }

    #[test]
    #[should_panic(expected = "--pull_token_url requires --pull_auth oauth2")]
    fn validate_rejects_oauth_credentials_without_mode() {
        let mut config = base_config();
        config.pull_token_url = Some("https://id.example.com/token".to_string());
        config.pull_client_id = Some("cid".to_string());
        config.pull_client_secret = Some("secret".to_string());
        validate_pull_config(&config);
    }

    #[test]
    #[should_panic(
        expected = "--pull_username requires --pull_auth basic, but pull_auth is 'oauth2'"
    )]
    fn validate_rejects_credentials_belonging_to_another_mode() {
        // oauth2 is correct, but the stray basic credentials are inert and must not pass.
        let mut config = base_config();
        config.pull_auth = Some("oauth2".to_string());
        config.pull_token_url = Some("https://id.example.com/token".to_string());
        config.pull_client_id = Some("cid".to_string());
        config.pull_client_secret = Some("secret".to_string());
        config.pull_username = Some("leftover".to_string());
        validate_pull_config(&config);
    }

    #[test]
    #[should_panic(expected = "--pull_scope requires --pull_auth oauth2")]
    fn validate_rejects_scope_without_oauth() {
        let mut config = base_config();
        config.pull_auth = Some("header".to_string());
        config.pull_auth_header = Some("X-API-Key".to_string());
        config.pull_auth_value = Some("secret".to_string());
        config.pull_scope = Some("token".to_string());
        validate_pull_config(&config);
    }

    #[test]
    fn validate_accepts_each_mode_with_only_its_own_credentials() {
        // The check must not be satisfiable by rejecting everything.
        let mut header = base_config();
        header.pull_auth = Some("header".to_string());
        header.pull_auth_header = Some("X-API-Key".to_string());
        header.pull_auth_value = Some("secret".to_string());
        validate_pull_config(&header);

        let mut basic = base_config();
        basic.pull_auth = Some("basic".to_string());
        basic.pull_username = Some("svc".to_string());
        basic.pull_password = Some("secret".to_string());
        validate_pull_config(&basic);

        let mut oauth = base_config();
        oauth.pull_auth = Some("oauth2".to_string());
        oauth.pull_token_url = Some("https://id.example.com/token".to_string());
        oauth.pull_client_id = Some("cid".to_string());
        oauth.pull_client_secret = Some("secret".to_string());
        oauth.pull_scope = Some("token".to_string());
        validate_pull_config(&oauth);
    }

    #[test]
    fn validate_accepts_explicit_none_auth_with_no_credentials() {
        let mut config = base_config();
        config.pull_auth = Some("none".to_string());
        validate_pull_config(&config);
    }

    #[test]
    fn cursor_db_key_is_namespaced() {
        assert_eq!(cursor_db_key("sophos-events"), "pull_cursor:sophos-events");
    }
}
