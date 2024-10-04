# ilagent 

![Docker Image Size (latest by date)](https://img.shields.io/docker/image-size/ilert/ilagent?sort=date)
![Docker Image Version (latest by date)](https://img.shields.io/docker/v/ilert/ilagent?sort=date)
![GitHub Workflow Status](https://img.shields.io/github/actions/workflow/status/iLert/ilagent/docker-release.yml)

The ilert Agent 🦀 📦 is a program that lets you easily integrate your on premise system with ilert - a swiss army knife.

## ilert agent

The ilert agent comes in a single binary with a small footprint and helps you to

* Send events from the command line `ilagent event -k il1api123... -t ALERT -s 'a summary from the shell'`
* Send heartbeat pings from the command line `ilagent heartbeat -k il1hbt123...`
* Monitor a host with regular heartbeats `ilagent daemon -b il1hbt123...`
* Run a proxy server with retry-queue for HTTP events and heartbeats on premise `ilagent daemon -p 8977`
* Run a proxy server with retry-queue for MQTT events and heartbeats on premise `ilagent daemon -m 192.168.1.14`
* Run a proxy server with retry behaviour for Apache Kafka messages on premise `ilagent daemon --kafka_brokers localhost:9092`
* Map and filter your MQTT or Kafka events to alerts [see](#Mapping-MQTT-or-Apache-Kafka-messages-to-events)
* Clean-up your open alerts (mass resolve) `ilagent cleanup -k your-api-key --resource alerts`

<p align="center"><img src="/docs/misc/froggo.png?raw=true"/></p>

## Downloading / Installing

### Docker image

You can grab the latest release from [Docker hub](https://hub.docker.com/r/ilert/ilagent)

```shell script
docker run ilert/ilagent
```

### Install script

For MacOS and Linux we also provide this one-liner to automatically install the agent:

> Note: default prebuild support stopped at version 0.3.0 if you cannot use the docker image or compile yourself and need new builds please open an issue

```shell script
curl -sL https://raw.githubusercontent.com/iLert/ilagent/master/install.sh | bash -
```

### Pre-build releases

> Note: default prebuild support stopped at version 0.3.0 if you cannot use the docker image or compile yourself and need new builds please open an issue

We provide pre compiled binaries for every major OS on the [release page of this repository](https://github.com/iLert/ilagent/releases).

Grab your version

- [Linux x86_64](https://github.com/iLert/ilagent/releases/download/0.3.0/ilagent_linux)
- [Windows x86_64](https://github.com/iLert/ilagent/releases/download/0.3.0/ilagent.exe)
- [Mac x86_64](https://github.com/iLert/ilagent/releases/download/0.3.0/ilagent_mac)
- [ARM (gnueabihf)](https://github.com/iLert/ilagent/releases/download/0.3.0/ilagent_arm)
- [Others][issues]

## Quick gimmicks

- The proxy exposes the exact same API as our public `https://api.ilert.com/api` you can therefore use our [API Docs](https://api.ilert.com/api-docs/#tag/Events)
- When running in `daemon` mode, it is always possible to provide a `-b il1hbt123...` heartbeat api key
to monitor the uptime of the agent
- To adjust the log level you can provide multiple `-v -v #info` verbose flags, default is error
- The event command supports additional args to add an image `-g 'url'`, link `-l 'url'` or priority `-o 'LOW'`
- You can adjust the MQTT topics that the proxy is listening on `-e 'ilert/events'` or `-r 'ilert/heartbeats'`
- Per default the `daemon` mode will not start an HTTP server, however you can provide a port with `-p 8977` to start it
- The agent will buffer events locally (except in kafka mode) using SQLite3 it will therefore require file system access in `daemon` mode
- Running detached: `nohup sh -c 'ilagent daemon -m 192.168.1.14 -b il1hbt123... -v -v'  > ./ilagent.log 2>&1 &`

You can always run `ilagent --help` or take a look at our [documentation](https://docs.ilert.com/ilagent) for help.

# Consumers

> Note: we expect your message payloads to be in JSON format

A minimal event payload `{
  "summary": "Hey from Kafka",
  "eventType": "ALERT",
  "apiKey": "il1awx123..."
}`

A minimal heartbeat payload `{
  "apiKey": "il1awx123..."
}`

## MQTT

### Connection

After connecting to your MQTT broker or proxy using `-m 127.0.0.1 -q 1883 -n ilagent`
you may specify the MQTT topic that should be listened to for events with `-e 'ilert/events'`.

#### Providing credentials

In case you need to provide credentials to your MQTT broker you can do so by passing the two arguments `--mqtt_username 'my-user'`
and `--mqtt_password 'my-pass'`, there is no default provided otherwise.

### At least once delivery

In MQTT mode events are buffered in SQLite3 to enable re-tries and guarantee delivery. If running the agent in Kubernetes make
sure to provide it with an attached volume for the ilagent3.db file in case you need delivery guarantee.

### Subscribing to wildcards

You may subscribe to wildcard topics like `-e 'ilert/+'` or `-e '#'`.

## Apache Kafka

### Connection

After connecting to your bootstrap broker(s) using `--kafka_brokers localhost:9092 --kafka_group_id ilagent`
you may specify the topic that should be subscribed to for events with `-e 'test-topic-events'` and for heartbeats `-r 'test-topic-hbts'`.

### At least once delivery

In Kafka mode events are NOT buffered in SQLite3, as the consumer offset is used to keep track and guarantee at least once delivery.
If an event cannot be delivered the offset is not committed and the agent will exit after 5 seconds. We recommend running the agent
in such a way that it is automatically restarted on exit e.g. `docker run ilert/ilagent --restart always`

## Mapping consumer messages to events

### Filtering messages

For a certain JSON key `--filter_key 'type'` or value of the key `--filter_val 'ALERT'`.
The above example will only process message payloads that fit `{ "type": "ALERT" }`.

### Setting a fixed API key

Most likely when mapping custom events from MQTT topics, you will not be able to 1:1 map an apiKey field.
This is why the `--event_key 'il1api123...'` option allows you to set a fixed ilert alert source API key
that is automatically used for every mapped event.

### Mapping required event fields

- You may map the `alertKey` field using `--map_key_alert_key 'mCode'` e.g. `{ "mCode": "123" }` -> 123
- You may map the `summary` field using `--map_key_summary 'comment'` e.g. `{ "comment": "A comment" }` -> "A comment"
- You may map the `eventType` field using `--map_key_etype 'state'` e.g. `{ "state": "ACK" }` -> "ACK"

> Note: if you are using custom mapping and fail to set the required field summary for ALERT events, the summary will be generated using the MQTT topic name of the message
> Note: if you fail to map the required event properties such as apiKey or eventType the event call will fail

### Mapping values of the eventType field

In case the property values of your eventType field do not match to ilert's API fields `ALERT, ACCEPT and RESOLVE` you may map these values as well.

- map `ALERT` value with `--map_val_etype_alert 'SET'` e.g. SET -> ALERT
- map `ACCEPT` value with `--map_val_etype_accept 'ACK'` e.g. ACK -> ACCEPT
- map `RESOLVE` value with `--map_val_etype_resolve 'CLR'` e.g. CLR -> RESOLVE

> Note: you may increase the log verbosity to understand why and how events are mapped by providing multiple `-v` depending on the loglevel

### Complete sample command

```sh
ilagent daemon -v -v \
    -m 127.0.0.1 -q 1883 -n ilagent -e '#' \
    --mqtt_username 'my-user' --mqtt_password 'my-pass' \
    --event_key 'il1api112115xxx' \
    --map_key_alert_key 'mCode' \
    --map_key_summary 'comment' \
    --map_key_etype 'state' \
    --map_val_etype_alert 'SET' \
    --map_val_etype_accept 'ACK' \
    --map_val_etype_resolve 'CLR' \
    --filter_key 'type' \
    --filter_val 'ALARM'
```

## Getting help

We are happy to respond to [GitHub issues][issues] as well.

<br>

## Developing

### Cross-Compiling

Of course, you can also grab the source code and compile it yourself.
Requires cross (`cargo install cross`) to be installed.

- Mac (or your host): `cargo build --release`
- Linux: `cross build --release --target x86_64-unknown-linux-gnu`
- Windows: `cross build --release --target x86_64-pc-windows-gnu`
- ARM: `cross build --release --target arm-unknown-linux-gnueabihf`
- All `cargo build --release && cross build --release --target x86_64-unknown-linux-gnu && cross build --release --target x86_64-pc-windows-gnu && cross build --release --target arm-unknown-linux-gnueabihf`


#### License

<sup>
Licensed under <a href="LICENSE">Apache License, Version 2.0</a>
Note that, depending on the usage of ilagent, it bundles SQLite and other libraries which might have different licenses
you can read more about them [here](https://github.com/rusqlite/rusqlite#license)
</sup>

[issues]: https://github.com/iLert/ilagent/issues/new
