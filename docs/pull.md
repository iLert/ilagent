# Pull connector

Polls an HTTP endpoint on a timer, maps each returned record to an ilert event, and
queues it for delivery. Use it for systems that offer no webhooks — Sophos Central,
Zabbix, Icinga, or any REST API that hands back a list of alerts.

Records go through the same mapping, filtering and enrichment as the MQTT and Kafka
consumers, and land in the same SQLite queue, so delivery keeps its retry, backoff and
offline buffering.

Setting `--pull_url` enables the connector, exactly as `--mqtt_host` enables MQTT.

## Minimal example

```sh
ilagent daemon -v -v \
    --pull_url https://alerts.internal/api/open \
    --pull_name internal-alerts \
    --pull_dedup_key id \
    --event_key 'il1api...' \
    --map_key_summary title
```

## Request

| flag | default | purpose |
|---|---|---|
| `--pull_url` | — | endpoint to poll; enables the connector. Static query params belong in the URL |
| `--pull_name` | **required** | names the source; keys its cursor and dedup state |
| `--pull_interval` | `60` | seconds between the end of one cycle and the start of the next (10–3600) |
| `--pull_body` | — | request body, implies POST. For JSON-RPC APIs |
| `--pull_items_path` | whole response | dot-path to the record array |
| `--pull_header` | — | static header, repeatable: `--pull_header 'X-Tenant-ID=abc'` |

`--pull_name` is required rather than derived, because it identifies persisted state.
If it were computed from the URL, editing `?limit=1000` to `?limit=500` would orphan the
stored cursor and every dedup row, silently replaying the whole lookback window.

## Finding the records

`--pull_items_path` points at the array inside the response:

```
{"items":  [...]}   →  --pull_items_path items      (Sophos)
{"value":  [...]}   →  --pull_items_path value      (Microsoft Graph)
{"result": [...]}   →  --pull_items_path result     (Zabbix)
[...]               →  omit the flag
```

A path resolving to a single object is treated as one record.

## Authentication

| mode | flags |
|---|---|
| `none` | — |
| `header` | `--pull_auth_header`, `--pull_auth_value` (or `ILERT_PULL_AUTH_VALUE`) |
| `basic` | `--pull_username`, `--pull_password` (or `ILERT_PULL_PASSWORD`) |
| `oauth2` | `--pull_token_url`, `--pull_client_id`, `--pull_client_secret` (or `ILERT_PULL_CLIENT_SECRET`), `--pull_scope` |

OAuth2 is client-credentials only. The token is cached, refreshed two minutes before it
expires, and re-fetched once if the API returns a 401 in the meantime.

Every secret has an environment variable fallback. Prefer it — a value passed as a flag
is visible in the process list.

## Resuming between polls

`--pull_cursor_mode` decides how the connector picks up where it left off.

### `none` (default)

No cursor. Every poll returns the current snapshot. This is what most on-prem tools do,
and dedup is what stops the same problem being sent repeatedly:

```sh
--pull_cursor_mode none --pull_dedup_key eventid
```

Without `--pull_dedup_key` every record is re-sent on every poll and you are relying
entirely on ilert's server-side `alertKey` deduplication. The agent warns at startup.

### `token`

The response carries an opaque cursor to send back next time:

```sh
--pull_cursor_mode token --pull_cursor_path next_cursor --pull_cursor_param cursor
```

Keys containing dots work — an exact key match is tried before the path is treated as
nesting — so a vendor field like `@odata.count` resolves as one key rather than as
nesting.

### `timestamp`

The high-water mark is computed from the records themselves:

```sh
--pull_cursor_mode timestamp \
--pull_cursor_path updated_at \
--pull_cursor_param since \
--pull_cursor_format iso8601 \
--pull_initial_lookback 7200 \
--pull_dedup_key id
```

`--pull_cursor_format` is `epoch_s`, `epoch_ms` or `iso8601`, and covers both reading the
field and writing it back. `--pull_initial_lookback` seeds the first run, since there is
nothing stored to resume from.

**`--pull_dedup_key` is required in this mode.** Most `since=` bounds are inclusive, so
the newest records are returned again on the following poll. Dedup absorbs that.

One limitation to know about: if more records share the maximum timestamp than fit in a
single page, the cursor cannot advance past them and the remainder are never fetched.
The agent detects this — a page whose records all carry one timestamp logs an error —
but it cannot fix it. Use a different cursor mode for such a source.

## Deduplication

`--pull_dedup_key` is a dot-path to a stable unique id per record. Keys are remembered
for seven days, then swept; a record reappearing after that is delivered again.

The dedup marker and the queued event are written in a single transaction. A marker
without its event would suppress that record forever while it was never delivered.

## Paging

Within one cycle the connector keeps requesting pages while records come back and the
cursor keeps changing, up to ten pages, then waits for the next interval. There is no
flag for this — a server that repeats a cursor or returns nothing terminates the loop on
its own.

The cursor is only persisted once every record on a page has been committed, so a
failure part-way through re-fetches that page rather than skipping it.

## Backpressure

If the delivery queue already holds 5000 events, the connector skips its cycle and logs
a warning. The queue drains 50 at a time with one HTTP POST each, so an unchecked fetch
loop against a large backfill can enqueue far faster than delivery keeps up.

## Mapping

All the shared consumer mapping flags apply — `--map_key_summary`, `--map_key_alert_key`,
`--map_key_etype`, `--map_val_etype_*`, `--map_key_severity`, `--map_val_severity`,
`--map_key_routing_key`, `--map_key_label`, `--filter_key`/`--filter_val`, `--label`,
`--service`, `--severity`, `--forward_message_payload`. See the MQTT and Kafka docs;
behaviour is identical.

`--event_key` is required. Vendor payloads carry no ilert integration key, and events
without one are rejected.

### Severity as a word

Most vendors label severity `high` / `medium` / `low` rather than 1-5. `--map_key_severity`
finds the field; `--map_val_severity` says what its values mean:

```sh
--map_key_severity severity \
--map_val_severity 'high=2' --map_val_severity 'medium=3' --map_val_severity 'low=4'
```

Matching is case-insensitive, several words can map to the same level, and a value the
map does not cover falls through to the plain numeric parse — so a source already
sending 1-5 is unaffected. Malformed mappings fail at startup rather than silently
dropping severities later.

Anything ilagent cannot model — a vendor event type, a hostname, a file path — is best
carried across with `--forward_message_payload`, which puts the whole original record
into `customDetails` for ilert's own event templating to work with.

## Health

When an HTTP port is configured, `/ready` returns 503 with
`{"component": "pull", ...}` while the connector has not started or its last cycle
failed.

## Limitations

- One pull source per agent. Run one agent per source; `--pull_name` keeps their state
  separate even when they share a database file.
- No leader election. Two agents polling the same source both emit.
- `--pull_body` cannot be combined with `token` or `timestamp` cursor modes — the cursor
  travels as a query parameter, which a body-bearing endpoint would ignore. Rejected at
  startup rather than failing silently.
- Offset and `Link`-header pagination are not supported.
