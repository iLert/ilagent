# Pull connector

Polls an HTTP endpoint on a timer, maps each returned record to an ilert event, and
queues it for delivery. Use it for APIs that offer no webhooks, such as Sophos Central or
any REST API that hands back a list of alerts.

Records are mapped with the same `--map_key_*` flags as the MQTT and Kafka consumers and
land in the same SQLite queue, so delivery keeps its retry, backoff and offline
buffering.

Setting `--pull_url` enables the connector, like `--mqtt_host` enables MQTT.

## Minimal example

```sh
ilagent daemon -v \
    --pull_url https://alerts.internal/api/open \
    --pull_items_path items \
    --event_key 'il1api...' \
    --map_key_alert_key id \
    --map_key_summary title
```

`--event_key` and `--map_key_alert_key` are required. Vendor records carry no ilert
integration key, and ilert uses the alert key to fold records that are delivered more
than once (see [Re-delivery](#re-delivery)).

## Request

| flag / env | default | purpose |
|---|---|---|
| `--pull_url` | — | endpoint to poll (GET); enables the connector. Static query params belong in the URL |
| `--pull_interval` | `60` | seconds between the end of one cycle and the start of the next (10–3600) |
| `--pull_items_path` | whole response | dot-path to the record array |
| `--pull_header KEY=VALUE` | — | static header, repeatable: `--pull_header 'X-Tenant-ID=abc'` |
| `--pull_cursor PATH=PARAM` | — | token paging, see below |
| `--pull_oauth_url` | — | oauth2 token endpoint; setting it enables oauth |
| `--pull_oauth_client_id` | — | required with `--pull_oauth_url` |
| `--pull_oauth_scope` | — | optional |
| `ILERT_PULL_OAUTH_SECRET` | — | oauth2 client secret, environment only; required with `--pull_oauth_url` |

A static API key goes in a header: `--pull_header 'X-API-Key=...'` or
`--pull_header 'Authorization=Bearer ...'`. A value passed as a flag is visible in the
process list, which is why the oauth secret is read from the environment only. Use
`https://`: the agent warns at startup when credentials or headers would go out over
plain `http://`.

The pull url is never logged, since it often carries a key in its query string. That
includes HTTP error messages, which are also served as the `error` field of `/ready`.
Log lines name the source by the host of the url.

Redirects are followed only while they stay on the same origin (scheme, host and port).
A redirect to anywhere else fails the cycle with the redirect status: a `--pull_header`
credential would otherwise go along with it. Point `--pull_url` at the final location.

Responses larger than 10 MB fail the cycle.

## Finding the records

`--pull_items_path` points at the array inside the response:

```
{"items":  [...]}   →  --pull_items_path items      (Sophos)
{"value":  [...]}   →  --pull_items_path value      (Microsoft Graph)
[...]               →  omit the flag
```

- A path that resolves to a single object is treated as one record.
- A path that resolves to `null` is an empty page.
- A path that is **missing** from the response fails the cycle, and `/ready` turns 503.
  That way an API error body (`{"error": {...}}`) can't pass as an empty poll. The error
  lists the response's top-level keys, never their values.
- A record that is not a JSON object is skipped with a warning.

An exact key match is tried before dots are read as nesting, so vendor keys like
`@odata.count` resolve as one key. This applies to `--pull_items_path` and to the path in
`--pull_cursor`. The `--map_key_*` flags keep their usual nested lookup.

## Token paging

`--pull_cursor PATH=PARAM` reads `PATH` from each response and sends it back as
`?PARAM=` on the next request:

```sh
--pull_cursor next_cursor=cursor
```

Within one cycle the connector keeps fetching pages while the cursor changes, for at
most ten pages, then waits for the next interval. A cycle ends when:

- the response has no cursor (or an empty one),
- the cursor is the same as the one just sent,
- the page holds no records (its new cursor is still stored first; Sophos returns
  `items: []` together with a fresh `next_cursor`),
- ten pages were fetched,
- or the delivery queue reached 5000 events (see [Backpressure](#backpressure)).

The cursor is stored only after every record on its page is queued, so a failure part
way through fetches that page again instead of skipping it. It is stored in the
database under a key derived from `--event_key` and the url **without its query
string**. Editing `?limit=` keeps the stored cursor. Pointing `--pull_url` at a different
endpoint starts fresh instead of sending it a cursor from another endpoint.

On the very first run there is no stored cursor, so the request goes out without the
parameter and the API returns whatever it returns by default (for Sophos, the last 24
hours).

### Snapshot polling (no cursor)

Without `--pull_cursor`, every poll fetches one page and sends every record in it. This
suits on-prem tools that return the current set of open problems. ilert folds the
repeats by alert key. The agent warns at startup that it is running in this mode.

## OAuth2

`--pull_oauth_url` enables the client-credentials flow:

```sh
ILERT_PULL_OAUTH_SECRET='<secret>' ilagent daemon \
    --pull_oauth_url https://id.example.com/oauth2/token \
    --pull_oauth_client_id '<client id>' \
    --pull_oauth_scope token \
    ...
```

The token is sent as `Authorization: Bearer ...`. It is cached and refreshed two
minutes before it expires. If the API answers 401 or 403 anyway (the token was revoked
early), the token is fetched again and the cycle retried once.

## Mapping

Records are mapped **only** through the shared consumer flags: `--map_key_alert_key`,
`--map_key_summary`, `--map_key_etype` with `--map_val_etype_*`, `--map_key_severity`,
`--map_key_routing_key`, `--map_key_label`, `--filter_key`/`--filter_val`, plus the
static `--label`, `--service` and `--severity`. See [mqtt.md](mqtt.md); behaviour is the
same.

Unlike MQTT and Kafka payloads, a pull record is not read as an ilert event. Vendor
fields that happen to share a name with ilert fields (`eventType`, `priority`,
`details`, `labels`, `severity`, ...) are ignored unless a `--map_key_*` flag points at
them. Without `--map_key_etype` every record is an `ALERT`. Without `--map_key_summary`
the summary is `New alert from <host>`.

`--forward_message_payload` puts the whole record into `customDetails`, so ilert's event
templating can use fields ilagent has no mapping for. Without it, `customDetails` is
`{"source": "<host>"}`.

## Re-delivery

The agent keeps no local record of what it has sent. A record is sent again when:

- a snapshot source returns it on the next poll,
- a cycle fails after some pages were stored and the failed page is fetched again,
- or the API itself returns overlapping pages.

ilert deduplicates these by alert key, which is why `--map_key_alert_key` is required.
Point it at a stable unique id of the record.

## Backpressure

When the delivery queue holds 5000 events, the connector skips its cycle, and a cycle
already in progress stops fetching further pages. Both log a warning, but neither counts
as an error for `/ready`. The queue drains 50 events at a time, so an unchecked fetch
loop against a large backlog would enqueue far faster than delivery keeps up.

## Health

When `--port` is set, `/ready` returns 503 with `{"component": "pull", ...}` while the
connector has not started yet or its last cycle failed, and 204 otherwise. Failed cycles
back off exponentially (5 s, doubling, capped at 300 s).

## Running alongside other modes

- `--pull_url` with `--mqtt_host` or `--kafka_brokers` is rejected at startup. The pull
  connector needs `--event_key` and the `--map_key_*` flags, which would silently change
  how the broker's events are mapped. Run one agent per source.
- `--pull_url` with `--port` is allowed. The HTTP proxy does not use `--event_key` or
  `--map_key_*`, but the static `--label`, `--service` and `--severity` apply to events
  from both.
- `--pull_url` with `--edge_mode` is rejected.

## Limitations

- One pull source per agent. Give every agent its own `--file`.
- No leader election: two agents polling the same source both send.
- A record that disappears from a snapshot source does not resolve its alert in ilert.
- GET only, no request body (JSON-RPC APIs such as Zabbix are not supported).
- Token paging only: no timestamp, offset or `Link` header paging.
- The first run without a stored cursor gets whatever the API returns by default.
