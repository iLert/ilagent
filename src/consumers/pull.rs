use log::{debug, error, info, warn};
use reqwest::header::{HeaderName, HeaderValue};
use ring::digest;
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
/// Same as reqwest's default limit, which the same-origin policy replaces.
const MAX_REDIRECTS: usize = 10;

/// Pages per cycle, mirroring MAX_CONSECUTIVE_REPOLLS in the edge connector.
pub const MAX_CONSECUTIVE_PAGES: u32 = 10;

/// Stop fetching while this many events are queued; poll.rs drains slower than we fetch.
pub const QUEUE_BACKPRESSURE_THRESHOLD: i64 = 5000;

/// A response (or oauth token response) larger than this fails the cycle.
pub const MAX_RESPONSE_BYTES: usize = 10 * 1024 * 1024;

/// Unauthorized is its own variant so the oauth retry cannot be triggered by an
/// unrelated error whose message happens to contain "401".
#[derive(Debug)]
pub enum PullError {
    Unauthorized(u16),
    Other(String),
}

impl std::fmt::Display for PullError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PullError::Unauthorized(status) => {
                write!(f, "unauthorized ({}), check pull credentials", status)
            }
            PullError::Other(msg) => write!(f, "{}", msg),
        }
    }
}

impl From<String> for PullError {
    fn from(msg: String) -> Self {
        PullError::Other(msg)
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

/// Request settings resolved once at startup.
struct PullSource {
    url: String,
    headers: Vec<(HeaderName, HeaderValue)>,
    items_path: Option<String>,
    /// (response path, query param)
    cursor: Option<(String, String)>,
    cursor_key: String,
    /// Host of the pull url, used in logs, summaries and default customDetails.
    label: String,
}

impl PullSource {
    fn from_config(config: &ILConfig) -> PullSource {
        let url = config
            .pull_url
            .clone()
            .expect("pull_url is required, validated at startup");
        let headers = config
            .pull_headers
            .iter()
            .map(|token| parse_header_flag(token).expect("validated at startup"))
            .collect();
        let cursor = match (&config.pull_cursor_path, &config.pull_cursor_param) {
            (Some(path), Some(param)) => Some((path.clone(), param.clone())),
            _ => None,
        };
        let cursor_key = cursor_db_key(config.event_key.as_deref().unwrap_or_default(), &url);
        PullSource {
            label: source_label(&url),
            url,
            headers,
            items_path: config.pull_items_path.clone(),
            cursor,
            cursor_key,
        }
    }
}

/// `pull_cursor:<hash>` over the integration key and the url without its query string:
/// editing `?limit=` keeps the cursor, another endpoint starts fresh instead of being sent
/// a foreign cursor. Hashed like the edge connector's `edge_cursor:` key.
pub fn cursor_db_key(event_key: &str, pull_url: &str) -> String {
    let base = pull_url.split(['?', '#']).next().unwrap_or_default();
    let input = format!("{}\n{}", event_key, base);
    let hash = digest::digest(&digest::SHA256, input.as_bytes());
    let hex: String = hash.as_ref().iter().map(|b| format!("{:02x}", b)).collect();
    format!("pull_cursor:{}", &hex[..16])
}

/// The host only: the full url routinely carries a key in its query string.
fn source_label(url: &str) -> String {
    reqwest::Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(|h| h.to_string()))
        .unwrap_or_else(|| "pull".to_string())
}

/// `--pull_cursor PATH=PARAM`, both sides non-empty.
pub fn parse_cursor_flag(token: &str) -> Option<(String, String)> {
    let (path, param) = token.split_once('=')?;
    let (path, param) = (path.trim(), param.trim());
    if path.is_empty() || param.is_empty() {
        return None;
    }
    Some((path.to_string(), param.to_string()))
}

/// `--pull_header KEY=VALUE`. Errors name only the leading header name: a ':' where a
/// '=' belongs leaves a live credential in the remainder.
pub fn parse_header_flag(token: &str) -> Result<(HeaderName, HeaderValue), String> {
    let name_hint = || {
        if token.contains([':', '=', ' ']) {
            format!(
                " starting '{}'",
                token
                    .split([':', '=', ' '])
                    .next()
                    .unwrap_or_default()
                    .trim()
            )
        } else {
            // No separator at all: the whole token may be the secret.
            String::new()
        }
    };
    let Some((key, value)) = token.split_once('=') else {
        return Err(format!(
            "--pull_header{} is malformed, expected KEY=VALUE",
            name_hint()
        ));
    };
    let key = key.trim();
    if key.is_empty() {
        return Err("--pull_header has an empty name, expected KEY=VALUE".to_string());
    }
    let name = HeaderName::from_bytes(key.as_bytes())
        .map_err(|_| format!("--pull_header{} has an invalid header name", name_hint()))?;
    let mut value = HeaderValue::from_str(value)
        .map_err(|_| format!("--pull_header '{}' has an invalid header value", key))?;
    value.set_sensitive(true);
    Ok((name, value))
}

/// Exact key match before dots are read as nesting, for vendor keys like `@odata.count`.
/// Local rather than in json_util so the shared mapping behaviour is untouched.
fn lookup_path<'a>(json: &'a Value, path: &str) -> Option<&'a Value> {
    if let Some(val) = json.get(path) {
        return Some(val);
    }
    get_nested_value(json, path)
}

/// Record array from a body; no path means the whole body, an object means one record,
/// an explicit null means no records. A missing path is an error so an API error body
/// cannot pass as an empty poll; only key names are reported, never values.
pub fn extract_items(body: &Value, items_path: Option<&str>) -> Result<Vec<Value>, String> {
    let target = match items_path {
        Some(path) => match lookup_path(body, path) {
            Some(val) => val,
            None => {
                let keys = match body {
                    Value::Object(map) => {
                        let keys: Vec<&str> = map.keys().map(|k| k.as_str()).collect();
                        format!("top-level keys: {}", keys.join(", "))
                    }
                    _ => "response is not an object".to_string(),
                };
                return Err(format!(
                    "pull items path '{}' not found in response ({})",
                    path, keys
                ));
            }
        },
        None => body,
    };

    match target {
        Value::Array(items) => Ok(items.clone()),
        Value::Object(_) => Ok(vec![target.clone()]),
        Value::Null => Ok(Vec::new()),
        _ => Err(format!(
            "pull items path '{}' resolved to a value that is neither an array nor an object",
            items_path.unwrap_or("<whole body>")
        )),
    }
}

/// Next-page token from a response; an empty string counts as none.
fn next_cursor(body: &Value, path: &str) -> Option<String> {
    match lookup_path(body, path)? {
        Value::String(s) if !s.is_empty() => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

pub fn validate_pull_config(config: &ILConfig) {
    let Some(ref url) = config.pull_url else {
        panic!("--pull_url is required for the pull connector");
    };
    // The url is not echoed: it may carry a key.
    let Ok(parsed_url) = reqwest::Url::parse(url) else {
        panic!("--pull_url is not a valid URL");
    };

    if config.event_key.is_none() {
        panic!(
            "--event_key is required for the pull connector, vendor records carry no ilert integration key"
        );
    }

    if config.map_key_alert_key.is_none() {
        panic!(
            "--map_key_alert_key is required for the pull connector, ilert deduplicates re-delivered records by alertKey and without it every poll opens new alerts"
        );
    }

    if config.pull_interval < 10 || config.pull_interval > 3600 {
        panic!("--pull_interval must be between 10 and 3600 seconds");
    }

    match (&config.pull_cursor_path, &config.pull_cursor_param) {
        (Some(path), Some(param)) if !path.trim().is_empty() && !param.trim().is_empty() => {}
        (None, None) => {
            warn!(
                "Pull connector has no --pull_cursor: every record is sent on every poll and ilert folds them by alertKey (snapshot polling). A record that disappears from the source does not resolve its alert."
            );
        }
        _ => panic!("--pull_cursor must be PATH=PARAM (e.g. next_cursor=cursor)"),
    }

    for token in &config.pull_headers {
        if let Err(e) = parse_header_flag(token) {
            panic!("{}", e);
        }
    }

    let uses_oauth = config.pull_oauth_url.is_some();
    if let Some(ref oauth_url) = config.pull_oauth_url {
        if reqwest::Url::parse(oauth_url).is_err() {
            panic!("--pull_oauth_url is not a valid URL");
        }
        if config.pull_oauth_client_id.is_none() {
            panic!("--pull_oauth_client_id is required with --pull_oauth_url");
        }
        if config.pull_oauth_secret.is_none() {
            panic!("ILERT_PULL_OAUTH_SECRET env var is required with --pull_oauth_url");
        }
    } else if config.pull_oauth_client_id.is_some() || config.pull_oauth_scope.is_some() {
        panic!(
            "--pull_oauth_client_id and --pull_oauth_scope require --pull_oauth_url, without it no token is fetched and every request goes out unauthenticated"
        );
    }

    if uses_oauth || !config.pull_headers.is_empty() {
        let plain_http = parsed_url.scheme() == "http"
            || config
                .pull_oauth_url
                .as_deref()
                .and_then(|u| reqwest::Url::parse(u).ok())
                .is_some_and(|u| u.scheme() == "http");
        if plain_http {
            warn!(
                "Pull connector sends credentials or static headers over plain http://, use https:// outside of local testing"
            );
        }
    }
}

/// Vendor record to event through the configured mappings only, then the same
/// customDetails defaulting and static enrichment the MQTT/Kafka consumers apply.
pub fn prepare_vendor_event(
    config: &ILConfig,
    record: &Value,
    source: &str,
) -> Option<EventQueueItemJson> {
    let mut event = EventQueueItemJson::from_vendor_record(config, record, source)?;
    if event.customDetails.is_none() {
        event.customDetails = Some(if config.forward_message_payload {
            record.clone()
        } else {
            json!({ "source": source })
        });
    }
    crate::consumers::enrich_event(config, &mut event);
    Some(event)
}

/// Read a body in chunks, failing once it grows past `limit`.
async fn read_body_limited(
    mut response: reqwest::Response,
    limit: usize,
    what: &str,
) -> Result<Value, String> {
    let too_large = || format!("{} exceeds {} bytes", what, limit);
    if response.content_length().is_some_and(|n| n > limit as u64) {
        return Err(too_large());
    }
    let mut buf: Vec<u8> = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|e| format!("failed to read {}: {}", what, e.without_url()))?
    {
        if buf.len() + chunk.len() > limit {
            return Err(too_large());
        }
        buf.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&buf).map_err(|e| format!("failed to parse {} as json: {}", what, e))
}

async fn fetch_oauth_token(
    client: &reqwest::Client,
    config: &ILConfig,
) -> Result<OauthToken, String> {
    let token_url = config.pull_oauth_url.as_ref().ok_or("missing oauth url")?;
    let client_id = config
        .pull_oauth_client_id
        .as_ref()
        .ok_or("missing oauth client id")?;
    let client_secret = config
        .pull_oauth_secret
        .as_ref()
        .ok_or("missing oauth client secret")?;

    let mut form = vec![
        ("grant_type", "client_credentials"),
        ("client_id", client_id.as_str()),
        ("client_secret", client_secret.as_str()),
    ];
    if let Some(ref scope) = config.pull_oauth_scope {
        form.push(("scope", scope.as_str()));
    }

    let response = client
        .post(token_url)
        .header("User-Agent", crate::CALLER_AGENT)
        .form(&form)
        .send()
        .await
        // without_url: reqwest's Display renders the full url, query string included.
        .map_err(|e| format!("oauth token request failed: {}", e.without_url()))?;

    let status = response.status();
    if !status.is_success() {
        return Err(format!("oauth token request returned status {}", status));
    }

    let body = read_body_limited(response, MAX_RESPONSE_BYTES, "oauth token response").await?;

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

/// One HTTP request, with `cursor` sent as the configured query param when present.
async fn fetch_page(
    client: &reqwest::Client,
    source: &PullSource,
    cursor: Option<&str>,
    bearer: Option<&str>,
) -> Result<Value, PullError> {
    let mut request = client
        .get(&source.url)
        .header("User-Agent", crate::CALLER_AGENT);

    for (name, value) in &source.headers {
        request = request.header(name.clone(), value.clone());
    }

    if let Some(token) = bearer {
        request = request.bearer_auth(token);
    }

    if let (Some((_, param)), Some(value)) = (&source.cursor, cursor) {
        request = request.query(&[(param.as_str(), value)]);
    }

    let response = request
        .send()
        .await
        // without_url: this string is logged and served as the `error` field of /ready.
        .map_err(|e| PullError::Other(format!("pull request failed: {}", e.without_url())))?;

    let status = response.status();
    if status.as_u16() == 401 || status.as_u16() == 403 {
        return Err(PullError::Unauthorized(status.as_u16()));
    }
    if status.is_redirection() {
        // The Location is not included: it is as likely to carry a key as the pull url.
        return Err(PullError::Other(format!(
            "pull request returned redirect status {} that was not followed, only same-origin redirects are so credentials stay with the configured host; point --pull_url at the final location",
            status
        )));
    }
    if !status.is_success() {
        return Err(PullError::Other(format!(
            "pull request returned status {}",
            status
        )));
    }

    Ok(read_body_limited(response, MAX_RESPONSE_BYTES, "pull response").await?)
}

/// Enqueue one record. Returns true when it was inserted, false when skipped or filtered.
async fn enqueue_record(
    ctx: &Arc<DaemonContext>,
    source: &PullSource,
    record: &Value,
) -> Result<bool, String> {
    if !record.is_object() {
        warn!(
            "Pull source '{}' returned a record that is not a JSON object, skipping",
            source.label
        );
        return Ok(false);
    }

    let event = match prepare_vendor_event(&ctx.config, record, &source.label) {
        Some(event) => event,
        None => return Ok(false), // filtered, already logged
    };

    // No typed api path: pull events post to plain /events, like the event subcommand.
    let db_event = EventQueueItemJson::to_db(event, None);

    match ctx.db.lock().await.create_il_event(&db_event) {
        Ok(Some(val)) => {
            debug!(
                "Event {} from pull source '{}' added to queue",
                val.id.clone().unwrap_or_default(),
                source.label
            );
            Ok(true)
        }
        Ok(None) => Err("event insert returned no row".to_string()),
        Err(e) => Err(format!("event insert failed: {}", e)),
    }
}

async fn queue_depth(ctx: &Arc<DaemonContext>) -> i64 {
    ctx.db.lock().await.count_il_events().unwrap_or(0)
}

/// Fetch and enqueue what is available, returning the count. A page's cursor is only
/// persisted after all of its records are committed, so an error retries that page.
async fn run_cycle(
    ctx: &Arc<DaemonContext>,
    client: &reqwest::Client,
    source: &PullSource,
    bearer: Option<&str>,
) -> Result<u64, PullError> {
    let mut cursor: Option<String> = match source.cursor {
        Some(_) => ctx.db.lock().await.get_il_value(&source.cursor_key),
        None => None,
    };

    let mut enqueued: u64 = 0;
    let mut pages: u32 = 0;

    loop {
        // run_pull_job checked the first page; later pages re-check as the queue grows.
        if pages > 0 {
            let depth = queue_depth(ctx).await;
            if depth >= QUEUE_BACKPRESSURE_THRESHOLD {
                warn!(
                    "Pull source '{}' stopped paging, {} events still queued (threshold {})",
                    source.label, depth, QUEUE_BACKPRESSURE_THRESHOLD
                );
                break;
            }
        }

        pages += 1;

        let body = fetch_page(client, source, cursor.as_deref(), bearer).await?;
        let items = extract_items(&body, source.items_path.as_deref())?;

        debug!(
            "Pull source '{}' page {} returned {} records",
            source.label,
            pages,
            items.len()
        );

        for record in &items {
            if enqueue_record(ctx, source, record).await? {
                enqueued += 1;
            }
        }

        let Some((ref path, _)) = source.cursor else {
            break; // snapshot sources have no notion of a next page
        };

        // Read before the empty check: an empty page can still hand out a new cursor.
        let Some(next) = next_cursor(&body, path) else {
            debug!(
                "Pull source '{}' returned no next cursor, cycle done",
                source.label
            );
            break;
        };

        if cursor.as_deref() == Some(next.as_str()) {
            debug!(
                "Pull source '{}' cursor unchanged, cycle done",
                source.label
            );
            break;
        }

        if let Err(e) = ctx.db.lock().await.set_il_val(&source.cursor_key, &next) {
            return Err(PullError::Other(format!(
                "failed to persist pull cursor: {}",
                e
            )));
        }
        cursor = Some(next);

        if items.is_empty() {
            debug!(
                "Pull source '{}' returned an empty page, cycle done",
                source.label
            );
            break;
        }

        if pages >= MAX_CONSECUTIVE_PAGES {
            debug!(
                "Pull source '{}' reached max consecutive pages ({}), pausing until next interval",
                source.label, MAX_CONSECUTIVE_PAGES
            );
            break;
        }
    }

    Ok(enqueued)
}

/// reqwest strips only the standard auth headers on a cross-host redirect, so a key in a
/// `--pull_header` would follow it anywhere, and a 307/308 re-sends the oauth form body
/// with the client secret.
fn same_origin_redirects() -> reqwest::redirect::Policy {
    reqwest::redirect::Policy::custom(|attempt| {
        let same_origin = attempt
            .previous()
            .last()
            .is_some_and(|prev| prev.origin() == attempt.url().origin());
        if same_origin && attempt.previous().len() < MAX_REDIRECTS {
            attempt.follow()
        } else {
            attempt.stop()
        }
    })
}

pub async fn run_pull_job(ctx: Arc<DaemonContext>) {
    let source = PullSource::from_config(&ctx.config);
    let interval = ctx.config.pull_interval;
    let uses_oauth = ctx.config.pull_oauth_url.is_some();

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(REQUEST_TIMEOUT_SECS))
        .redirect(same_origin_redirects())
        .build()
        .expect("failed to create HTTP client for pull connector");

    let mut token: Option<OauthToken> = None;
    let mut consecutive_failures: u32 = 0;

    if let Some(ref probe) = ctx.pull_probe {
        probe.polling.store(true, Ordering::Relaxed);
    }

    // The url is not logged: it routinely carries a key or token in its query string.
    info!(
        "Pull source '{}' polling every {}s (cursor: {}, oauth: {})",
        source.label,
        interval,
        if source.cursor.is_some() {
            "token"
        } else {
            "none"
        },
        uses_oauth
    );

    let cycle_done = || {
        if let Some(ref probe) = ctx.pull_probe {
            probe.cycles.fetch_add(1, Ordering::Relaxed);
        }
    };

    loop {
        if !ctx.running.load(Ordering::Relaxed) {
            break;
        }

        // Fetching while the queue is already deep only grows the backlog.
        let depth = queue_depth(&ctx).await;
        if depth >= QUEUE_BACKPRESSURE_THRESHOLD {
            warn!(
                "Pull source '{}' skipping cycle, {} events still queued (threshold {})",
                source.label, depth, QUEUE_BACKPRESSURE_THRESHOLD
            );
            cycle_done();
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
                        source.label, consecutive_failures, backoff, e
                    );
                    if let Some(ref probe) = ctx.pull_probe {
                        probe.record_error(e);
                    }
                    cycle_done();
                    interruptible_sleep(&ctx, backoff).await;
                    continue;
                }
            }
        }

        let bearer = token.as_ref().map(|t| t.access_token.clone());
        let mut result = run_cycle(&ctx, &client, &source, bearer.as_deref()).await;

        // A token can be revoked before its stated expiry; retry once with a fresh one.
        if uses_oauth && let Err(PullError::Unauthorized(status)) = &result {
            info!(
                "Pull source '{}' got {}, refreshing token and retrying",
                source.label, status
            );
            token = None;
            match fetch_oauth_token(&client, &ctx.config).await {
                Ok(fresh) => {
                    let bearer = fresh.access_token.clone();
                    token = Some(fresh);
                    result = run_cycle(&ctx, &client, &source, Some(&bearer)).await;
                }
                Err(e) => result = Err(PullError::Other(e)),
            }
        }

        match result {
            Ok(count) => {
                consecutive_failures = 0;
                if let Some(ref probe) = ctx.pull_probe {
                    probe.clear_error();
                }
                if count > 0 {
                    info!("Pull source '{}' enqueued {} events", source.label, count);
                }
            }
            Err(e) => {
                consecutive_failures += 1;
                let backoff = backoff_secs(consecutive_failures);
                error!(
                    "Pull source '{}' cycle failed (attempt {}, backoff {}s): {}",
                    source.label, consecutive_failures, backoff, e
                );
                if let Some(ref probe) = ctx.pull_probe {
                    probe.record_error(e.to_string());
                }
                cycle_done();
                interruptible_sleep(&ctx, backoff).await;
                continue;
            }
        }

        cycle_done();

        interruptible_sleep(&ctx, interval).await;
    }

    info!("Pull source '{}' stopped", source.label);
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
        config.event_key = Some("il1api123".to_string());
        config.map_key_alert_key = Some("id".to_string());
        config
    }

    fn validate_err(config: &ILConfig) -> String {
        let result = std::panic::catch_unwind(|| validate_pull_config(config));
        let err = result.expect_err("validation should have failed");
        if let Some(s) = err.downcast_ref::<String>() {
            s.clone()
        } else if let Some(s) = err.downcast_ref::<&str>() {
            s.to_string()
        } else {
            String::new()
        }
    }

    // --- extract_items ---

    #[test]
    fn extract_items_from_nested_path() {
        let body = json!({"data": {"records": [{"id": 1}, {"id": 2}]}});
        let items = extract_items(&body, Some("data.records")).unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0]["id"], 1);
    }

    #[test]
    fn extract_items_from_bare_array() {
        let body = json!([{"id": 1}, {"id": 2}]);
        assert_eq!(extract_items(&body, None).unwrap().len(), 2);
    }

    #[test]
    fn extract_items_wraps_single_object() {
        let body = json!({"result": {"id": 1}});
        let items = extract_items(&body, Some("result")).unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0]["id"], 1);
    }

    #[test]
    fn extract_items_null_is_an_empty_page() {
        let body = json!({"items": null});
        assert!(extract_items(&body, Some("items")).unwrap().is_empty());
    }

    #[test]
    fn extract_items_missing_path_lists_keys_not_values() {
        // JSON-RPC style error body must not pass as an empty poll
        let body = json!({"jsonrpc": "2.0", "error": {"message": "secret-detail"}});
        let err = extract_items(&body, Some("items")).unwrap_err();
        assert!(err.contains("'items' not found"), "got: {}", err);
        assert!(err.contains("jsonrpc, error") || err.contains("error, jsonrpc"));
        assert!(
            !err.contains("secret-detail"),
            "values must not leak: {}",
            err
        );
    }

    #[test]
    fn extract_items_missing_path_in_non_object_body() {
        let err = extract_items(&json!([1, 2]), Some("items")).unwrap_err();
        assert!(err.contains("not an object"), "got: {}", err);
    }

    #[test]
    fn extract_items_scalar_is_an_error() {
        assert!(extract_items(&json!({"items": "nope"}), Some("items")).is_err());
        assert!(extract_items(&json!(42), None).is_err());
    }

    // --- lookup_path / next_cursor ---

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

    #[test]
    fn next_cursor_accepts_strings_and_numbers_only() {
        assert_eq!(
            next_cursor(&json!({"c": "abc"}), "c").as_deref(),
            Some("abc")
        );
        assert_eq!(next_cursor(&json!({"c": 42}), "c").as_deref(), Some("42"));
        assert_eq!(next_cursor(&json!({"c": ""}), "c"), None);
        assert_eq!(next_cursor(&json!({"c": null}), "c"), None);
        assert_eq!(next_cursor(&json!({"c": {"x": 1}}), "c"), None);
        assert_eq!(next_cursor(&json!({}), "c"), None);
    }

    // --- flag parsing ---

    #[test]
    fn cursor_flag_parses_path_and_param() {
        assert_eq!(
            parse_cursor_flag("meta.next=cursor"),
            Some(("meta.next".to_string(), "cursor".to_string()))
        );
        assert_eq!(parse_cursor_flag("next_cursor"), None);
        assert_eq!(parse_cursor_flag("=cursor"), None);
        assert_eq!(parse_cursor_flag("next= "), None);
    }

    #[test]
    fn header_flag_keeps_inner_equals_in_value() {
        let (name, value) = parse_header_flag("Authorization=Basic dXM6cA==").unwrap();
        assert_eq!(name.as_str(), "authorization");
        assert_eq!(value.to_str().unwrap(), "Basic dXM6cA==");
        assert!(value.is_sensitive());
    }

    #[test]
    fn header_flag_error_names_only_the_leading_part() {
        let err = parse_header_flag("X-Api-Key:supersecret").unwrap_err();
        assert!(err.contains("'X-Api-Key'"), "got: {}", err);
        assert!(!err.contains("supersecret"), "got: {}", err);
    }

    #[test]
    fn header_flag_without_separator_is_not_echoed() {
        let err = parse_header_flag("supersecret").unwrap_err();
        assert!(!err.contains("supersecret"), "got: {}", err);
    }

    #[test]
    fn header_flag_rejects_empty_and_invalid_names() {
        assert!(parse_header_flag("=value").is_err());
        assert!(parse_header_flag("Bad Name=value").is_err());
        assert!(parse_header_flag("X-Ok=bad\nvalue").is_err());
    }

    // --- cursor key ---

    #[test]
    fn cursor_key_ignores_query_string() {
        let a = cursor_db_key("k1", "https://api.example.com/siem/v1/events?limit=100");
        let b = cursor_db_key("k1", "https://api.example.com/siem/v1/events?limit=500");
        let c = cursor_db_key("k1", "https://api.example.com/siem/v1/events");
        assert_eq!(a, b);
        assert_eq!(a, c);
        assert!(a.starts_with("pull_cursor:"));
        assert_eq!(a.len(), "pull_cursor:".len() + 16);
    }

    #[test]
    fn cursor_key_changes_with_path_or_event_key() {
        let events = cursor_db_key("k1", "https://api.example.com/siem/v1/events");
        let alerts = cursor_db_key("k1", "https://api.example.com/siem/v1/alerts");
        let other_key = cursor_db_key("k2", "https://api.example.com/siem/v1/events");
        assert_ne!(events, alerts);
        assert_ne!(events, other_key);
    }

    #[test]
    fn source_label_is_host_only() {
        assert_eq!(
            source_label("https://api.example.com/x?api_key=secret"),
            "api.example.com"
        );
        assert_eq!(source_label("not a url"), "pull");
    }

    // --- prepare_vendor_event ---

    #[test]
    fn vendor_event_custom_details_default_to_source() {
        let config = base_config();
        let event = prepare_vendor_event(&config, &json!({"id": "a"}), "api.example.com").unwrap();
        assert_eq!(
            event.customDetails.unwrap(),
            json!({"source": "api.example.com"})
        );
    }

    #[test]
    fn vendor_event_forwards_record_as_custom_details() {
        let mut config = base_config();
        config.forward_message_payload = true;
        config.severity = Some(2);
        let record = json!({"id": "a", "priority": 1});
        let event = prepare_vendor_event(&config, &record, "h").unwrap();
        assert_eq!(event.customDetails.unwrap(), record);
        assert_eq!(event.severity, Some(2), "static enrichment applies");
        assert_eq!(event.alertKey.as_deref(), Some("a"));
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
    fn validate_accepts_full_oauth_token_source() {
        let mut config = base_config();
        config.pull_cursor_path = Some("next_cursor".to_string());
        config.pull_cursor_param = Some("cursor".to_string());
        config.pull_headers = vec!["X-Tenant-ID=abc".to_string()];
        config.pull_oauth_url = Some("https://id.example.com/token".to_string());
        config.pull_oauth_client_id = Some("cid".to_string());
        config.pull_oauth_secret = Some("secret".to_string());
        config.pull_oauth_scope = Some("token".to_string());
        validate_pull_config(&config);
    }

    #[test]
    fn validate_requires_event_key_and_alert_key() {
        let mut config = base_config();
        config.event_key = None;
        assert!(validate_err(&config).contains("--event_key is required"));

        let mut config = base_config();
        config.map_key_alert_key = None;
        assert!(validate_err(&config).contains("--map_key_alert_key is required"));
    }

    #[test]
    fn validate_rejects_invalid_url_without_echoing_it() {
        let mut config = base_config();
        config.pull_url = Some("not a url?api_key=secret".to_string());
        let err = validate_err(&config);
        assert!(
            err.contains("--pull_url is not a valid URL"),
            "got: {}",
            err
        );
        assert!(!err.contains("secret"));
    }

    #[test]
    fn validate_interval_bounds() {
        for bad in [9, 3601] {
            let mut config = base_config();
            config.pull_interval = bad;
            assert!(validate_err(&config).contains("--pull_interval must be between"));
        }
        for ok in [10, 3600] {
            let mut config = base_config();
            config.pull_interval = ok;
            validate_pull_config(&config);
        }
    }

    #[test]
    fn validate_rejects_half_a_cursor() {
        let mut config = base_config();
        config.pull_cursor_path = Some("next".to_string());
        assert!(validate_err(&config).contains("--pull_cursor must be PATH=PARAM"));

        let mut config = base_config();
        config.pull_cursor_path = Some(" ".to_string());
        config.pull_cursor_param = Some("cursor".to_string());
        assert!(validate_err(&config).contains("--pull_cursor must be PATH=PARAM"));
    }

    #[test]
    fn validate_rejects_malformed_header() {
        let mut config = base_config();
        config.pull_headers = vec!["X-Api-Key:secret".to_string()];
        let err = validate_err(&config);
        assert!(err.contains("expected KEY=VALUE"), "got: {}", err);
        assert!(!err.contains("secret"));
    }

    #[test]
    fn validate_oauth_requires_client_id_and_secret() {
        let mut config = base_config();
        config.pull_oauth_url = Some("https://id.example.com/token".to_string());
        config.pull_oauth_secret = Some("s".to_string());
        assert!(validate_err(&config).contains("--pull_oauth_client_id is required"));

        let mut config = base_config();
        config.pull_oauth_url = Some("https://id.example.com/token".to_string());
        config.pull_oauth_client_id = Some("cid".to_string());
        assert!(validate_err(&config).contains("ILERT_PULL_OAUTH_SECRET"));
    }

    #[test]
    fn validate_rejects_oauth_settings_without_url() {
        let mut config = base_config();
        config.pull_oauth_client_id = Some("cid".to_string());
        assert!(validate_err(&config).contains("require --pull_oauth_url"));

        let mut config = base_config();
        config.pull_oauth_scope = Some("token".to_string());
        assert!(validate_err(&config).contains("require --pull_oauth_url"));
    }
}
