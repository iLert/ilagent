# ilagent CHANGELOG

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