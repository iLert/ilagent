# ilagent CHANGELOG

## 2026-06-11, Version 0.11.0

* added support for four additional ilert event fields — `labels`, `severity`, `routingKey`, and `services` — wired end-to-end through the MQTT/Kafka consumers and the HTTP `POST /api/events` endpoint
* added `--label key=value` (repeatable) to stamp static labels on every event, and `--map_key_label name=jsonpath` (repeatable) to pull labels from arbitrary payload paths; static labels override mapped/payload labels on key conflict
* added `--severity N` to set a default severity (1–5, applied only when the event carries none) and `--map_key_severity jsonpath` to extract severity from the payload (string or number); out-of-range values are dropped without failing the event
* added `--map_key_routing_key jsonpath` to extract a routing key from the payload
* added `--service alias=NAME` / `--service id=NUM` (repeatable) to attach static service references to every event
* static enrichment (`--label`, `--severity`, `--service`) now applies to HTTP-only daemons as well as MQTT/Kafka consumers
* `POST /api/events` now validates `severity` is within 1–5, rejecting out-of-range values with HTTP 400
* these consumer arguments are rejected when combined with edge connector mode (`--edge_mode`)

## 2026-05-06, Version 0.10.2

* added optional authentication header for edge connector HTTP delivery (`--edge_http_auth_header`, `--edge_http_auth_value`) — the value can be set via `ILERT_EDGE_HTTP_AUTH_VALUE` env var to avoid exposing secrets in process listings

## 2026-05-05, Version 0.10.1

* added **stdout** edge connector delivery mode (`--edge_mode stdout`) — prints each polled event's JSON payload to stdout, useful for testing, debugging, or piping into other tools; no additional configuration required

## 2026-05-05, Version 0.10.0

* added **edge connector mode** (`--edge_mode`) — a new exclusive daemon mode that polls the ilert edge-connections API and delivers events to a local target via HTTP, Kafka, MQTT, or script execution
* edge connector supports HTTP delivery (`--edge_mode http`) with configurable target URL and method, custom headers (`X-ilert-Event-Type`, `X-ilert-Alert-Id`, `X-ilert-Edge-Item-Id`)
* edge connector supports Kafka delivery (`--edge_mode kafka`) producing to a configurable topic via `--edge_topic`
* edge connector supports MQTT delivery (`--edge_mode mqtt`) publishing to a configurable topic with QoS-aware broker acknowledgement
* edge connector supports script delivery (`--edge_mode script`) piping event JSON to stdin with metadata in environment variables, 30s execution timeout with kill on expiry
* cursor-based at-least-once delivery with per-integration-key SQLite persistence, immediate re-poll on full batches (capped at 10 consecutive re-polls to prevent starvation)
* high availability mode (`--edge_cluster_id`, `--edge_instance_id`) with server-managed leader election and standby polling
* exponential backoff on poll failures (5s base, capped at 300s), resets on success
* non-retryable HTTP delivery errors (4xx except 429) skip the item and advance the cursor instead of blocking the queue permanently
* 429 (rate limited) and 5xx responses are retryable — batch stops and retries on next cycle
* MQTT event loop supervision — if the event loop exits unexpectedly, the edge connector detects it and initiates shutdown
* readiness endpoint (`/ready`) reports edge connector health when `--port` is used alongside `--edge_mode`
* heartbeat (`--heartbeat`) is supported in edge connector mode for service-level liveness pings
* `--edge_poll_interval` and `--edge_standby_interval` default to 10s, validated to [5, 120] seconds
* eased permission requirements in the install script

## 2026-05-02, Version 0.9.0

* **BREAKING** removed the `cleanup` subcommand — this functionality has moved to the new `ilert` CLI tool (github.com/iLert/ilert-cli)
* HTTP requests to the ilert API now identify themselves with `ilagent/{version}` in the User-Agent header, enabling better version tracking and diagnostics
* upgraded ilert-rust SDK from 5.1.0 to 5.2.0

## 2026-05-01, Version 0.8.0

* **BREAKING** `--event_topic` and `--heartbeat_topic` are no longer subscribed by default in MQTT mode, at least one topic (`--event_topic`, `--heartbeat_topic`, or `--policy_topic`) must be explicitly configured
* **BREAKING** MQTT non-buffered mode now requires `--mqtt_qos 1` or `--mqtt_qos 2` — QoS 0 without `--mqtt_buffer` is rejected at startup because there is no broker acknowledgement to delay on delivery failure. Non-buffered mode delivers events and heartbeats directly to ilert inline, making it possible to run MQTT without SQLite
* **BREAKING** Kafka mode no longer implicitly starts the HTTP server — pass `--port` explicitly if you need health/readiness endpoints alongside Kafka
* readiness endpoint (`/ready`) now reflects actual consumer state: MQTT must be connected with all subscriptions acknowledged, Kafka must have a running consumer with active topic subscriptions — returns 503 with structured JSON diagnostics (`component`, `connected`, `subscriptions_ready`, `error`) until the consumer is fully operational
* health endpoint (`/health`) returns 503 during graceful shutdown instead of 204
* Kafka consumer now shuts down gracefully on SIGINT/SIGTERM instead of requiring a process kill
* if MQTT or Kafka worker exits unexpectedly while the HTTP server is running, the agent detects it and initiates shutdown instead of continuing in a half-broken state
* Kafka consumer creation and topic subscription failures are now handled gracefully with error logging instead of panicking the process
* added TLS certificate hot reload for MQTT — certificates are polled every 30s and the connection is transparently re-established when files change on disk
* moved from `TlsConfiguration::Simple` to `TlsConfiguration::Rustls` with explicit PEM parsing and validation
* fixed buffered MQTT event durability: `enqueue_event` now distinguishes insert success, filtered payload, and database failure so that `mqtt_queue` items are retained for retry on DB errors instead of being silently dropped

## 2026-03-23, Version 0.7.0

* **BREAKING** `--map_key_summary`, `--map_key_alert_key`, and `--map_key_etype` now interpret dots as nested path separators (e.g. `data.message` accesses `{"data": {"message": "..."}}`) — JSON keys containing literal dots can no longer be matched with these flags
* **BREAKING** `cleanup` command no longer accepts `--api_key` argument, API key must be provided via `ILERT_API_KEY` environment variable
* `event` and `heartbeat` commands now fall back to `ILERT_INTEGRATION_KEY` env var when `--integration_key` is not provided
* added escalation policy consumer mode for MQTT and Kafka with `--policy_topic`, `--policy_routing_keys`, `--map_key_email`, `--map_key_shift`
* policy mode resolves routing keys via `/escalation-policies/resolve`, users via `POST /users/resolve`, and updates levels via `PUT /escalation-policies/{id}/levels/{shift}`
* added dot-notation support for `--map_key_summary`, `--map_key_alert_key`, `--map_key_etype`, `--map_key_email`, and `--map_key_shift` (e.g. `data.message`, `status.type`)
* added `--forward_message_payload` flag to include the full original JSON payload as `customDetails` in events (MQTT and Kafka)
* added `--shift_offset` flag to adjust shift values (e.g. `-1` to convert 1-indexed to 0-indexed)
* added `--mqtt_qos` flag to configure MQTT QoS level (0, 1, or 2)
* added `--mqtt_buffer` flag to buffer all MQTT messages (events and policies) in SQLite for retry with adaptive polling and exponential backoff
* added `--mqtt_shared_group` flag for MQTT v5 shared subscriptions (load balancing across multiple agent instances)
* adaptive polling for both event and MQTT queue pollers (fast drain when active, exponential backoff on failures up to 60s)
* extracted `get_nested_value` into shared `json_util` module for reuse across event and policy mapping
* added high availability documentation covering Kafka, HTTP, and MQTT deployment strategies
* bumped Rust Docker image from 1.91 to 1.94
* upgraded ilert-rust SDK from 5.0.1 to 5.1.0

## 2026-03-19, Version 0.6.0

* **BREAKING** CLI now uses subcommands: `daemon`, `event`, `heartbeat`, `cleanup` (previously positional argument)
* **BREAKING** `--api_key` renamed to `--integration_key` for `event` and `heartbeat` commands (`-k` shorthand still works)
* added MQTT TLS support with `--mqtt_tls`, `--mqtt_ca`, `--mqtt_client_cert`, `--mqtt_client_key`
* streamlined consumer message preparation for MQTT and Kafka into shared helpers
* preserved legacy heartbeat support for `il1hbt` prefixed keys via old heartbeat endpoint
* exponential backoff for MQTT reconnects (capped at 30s), replacing linear delay
* version string now derived from Cargo.toml instead of hardcoded
* refactored codebase into library (`lib.rs`) for better testability
* added comprehensive unit tests, integration tests, and e2e tests (wiremock, testcontainers)
* upgraded dependencies: tokio 1.50, actix-web 4.13, rdkafka 0.39, uuid 1.22

## 2025-12-21, Version 0.5.2

* upgraded dependencies
* bumped the docker image to rust 1.91
* using new ilert-rust:5.0.1 to send new heartbeat keys to 2.0 architecture

## 2024-10-07, Version 0.5.1

* added option to send event message payloads directly to integration endpoint targets

## 2024-10-04, Version 0.5.0

* upgraded dependencies
* migrated from sync threads to a tokio app
* **BREAKING** --mqtt_* prefixed event mapping arguments have dropped the prefix to fit to other consumers as well
* now supporting Apache Kafka to event API proxy
* bumped the docker image to rust 1.81
* bumped SQLite version from 3.41.2 -> 3.46.0

## 2023-05-13, Version 0.4.0

* upgraded dependencies
* bumped SQLite from 3.36.0 to 3.41.2
* added new `cleanup` command
* added cleanup command resource `alerts`

## 2021-11-02, Version 0.3.0

* **BREAKING** --incident_key is now --alert_key (-i is still available)
* **BREAKING** http server is not started unless --p is provided
* **BREAKING** migrated to new API /api/v1/events -> /api/events
* if one of the threads exit, the whole program will exit
* moved to ilert-rust@2.0.0, will migrate incident_key -> alert_key in code and db
* added event mapping keys to map mqtt payloads to event api
* added event filter keys to filter mqtt payloads

## 2020-08-21, Version 0.2.2

* keep mqtt connection settings on reconnect

## 2020-08-06, Version 0.2.1

* recovery loop for mqtt connection

## 2020-07-14, Version 0.2.0

* starting the changelog