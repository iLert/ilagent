# ilagent

The iLert Agent 🦀 📦 is a program that lets you easily integrate your on premise system with iLert.

<sup>Super small footprint (20MB container, that consumes about 5MB! of RAM)</sup>

## iLert agent

The iLert agent comes in a single binary and helps you to

* Send events from the command line `ilagent event -k il1insada3872867c63 -t ALERT -s 'a summary from the shell'`
* Send heartbeat pings from the command line `ilagent heartbeat -k il1insada3872867c63`
* Monitor a host with regular heartbeats `ilagent daemon -b il1insada3872867c63`
* Run a proxy server with retry-queue for HTTP events and heartbeats on premise `ilagent daemon -p 8977`
* Run a proxy server with retry-queue for MQTT events and heartbeats on premise `ilagent daemon -m 192.168.1.14`
* Map and filter your MQTT events to alerts [see](#mapping-mqtt-events)

<p align="center"><img src="/demo.gif?raw=true"/></p>

## Downloading / Installing

### Docker image

You can grab the latest release from [Docker hub](https://hub.docker.com/r/ilert/ilagent)

```shell script
docker run ilert/ilagent
```

### Install script

For MacOS and Linux we also provide this one-liner to automatically install the agent:

```shell script
curl -sL https://raw.githubusercontent.com/iLert/ilagent/master/install.sh | bash -
```

### Pre-build releases

We provide pre compiled binaries for every major OS on the [release page of this repository](https://github.com/iLert/ilagent/releases).

Grab your version

- [Linux x86_64](https://github.com/iLert/ilagent/releases/download/0.3.0/ilagent_linux)
- [Windows x86_64](https://github.com/iLert/ilagent/releases/download/0.3.0/ilagent.exe)
- [Mac x86_64](https://github.com/iLert/ilagent/releases/download/0.3.0/ilagent_mac)
- [ARM (gnueabihf)](https://github.com/iLert/ilagent/releases/download/0.3.0/ilagent_arm)
- [Others][issues]

### Compiling

Of course you can also grab the source code and compile it yourself.
Requires cross (`cargo install cross`) to be installed.

- Mac (Host): `cargo build --release`
- Linux: `cross build --release --target x86_64-unknown-linux-gnu`
- Windows: `cross build --release --target x86_64-pc-windows-gnu`
- ARM: `cross build --release --target arm-unknown-linux-gnueabihf`
- All `cargo build --release && cross build --release --target x86_64-unknown-linux-gnu && cross build --release --target x86_64-pc-windows-gnu && cross build --release --target arm-unknown-linux-gnueabihf`

## Quick gimmicks

- The proxy exposes the exact same API as our public `https://api.ilert.com/api` you can therefore use our [API Docs](https://api.ilert.com/api-docs/#tag/Events)
- When running in `daemon` mode, it is always possible to provide a `-b il1insada3872867c63` heartbeat api key
to monitor the uptime of the agent.
- To adjust the log level you can provide multiple `-v -v #info` verbose flags, default is error.
- The event command supports additional args to add an image `-g 'url'`, link `-l 'url'` or priority `-o 'LOW'`.
- You can adjust the MQTT topics that the proxy is listening on `-e 'ilert/events'` or `-r 'ilert/heartbeats'`
- Per default the `daemon` mode will not start an HTTP server, however you can provide a port with `-p 8977` to start it.
- The agent will buffer events locally using SQLite3 it will therefore require file system access in `daemon` mode.
- Running detached: `nohup sh -c 'ilagent daemon -m 192.168.1.14 -b il1hbt... -v -v'  > ./ilagent.log 2>&1 &`

You can always run `ilagent --help` or take a look at our [documentation](https://docs.ilert.com/ilagent) for help.

## Mapping MQTT events

> Note: we expect your message payloads to be in JSON format

### MQTT connection

After connecting to your MQTT broker or proxy using `-m 127.0.0.1 -q 1883 -n ilagent`
you may specify the MQTT topic that should be listen to for events with `-e 'ilert/events'`.

### Subscribing to wildcards and filtering events

You may subscribe to wildcard topics like `-e 'ilert/+'` or `-e '#'` and filter
for a certain JSON key `--mqtt_filter_key 'type'` or value of the key `--mqtt_filter_val 'ALERT'`.
The above example will listen for every message on every topic with a payload that fits `{ "type": "ALERT" }`.

### Setting a fixed API key

Most likely when mapping custom events from MQTT topics, you will not be able to 1:1 map an apiKey field.
This is why the `--mqtt_event_key 'il1api112115xxx'` option allows you to set a fixed iLert alert source API key
that is automatically used for every mapped MQTT event.

### Mapping required event fields

- You may map the `alertKey` field using `--mqtt_map_key_alert_key 'mCode'` e.g. `{ "mCode": "123" }` -> 123
- You may map the `summary` field using `--mqtt_map_key_summary 'comment'` e.g. `{ "comment": "A comment" }` -> "A comment"
- You may map the `eventType` field using `--mqtt_map_key_etype 'state'` e.g. `{ "state": "ACK" }` -> "ACK"

> Note: if you are using custom mapping and fail to set the required field summary for ALERT events, the summary will be generated using the MQTT topic name of the message
> Note: if you fail to map the required event properties such as apiKey or eventType the event will be dropped

### Mapping values of the eventType field

In case the property values of your eventType field do not match to iLert's API fields `ALERT, ACCEPT and RESOLVE` you may map these values as well.

- map `ALERT` value with `--mqtt_map_val_etype_alert 'SET'` e.g. SET -> ALERT
- map `ACCEPT` value with `--mqtt_map_val_etype_accept 'ACK'` e.g. ACK -> ACCEPT
- map `RESOLVE` value with `--mqtt_map_val_etype_resolve 'CLR'` e.g. CLR -> RESOLVE

> Note: you may increase the log verbosity to understand why and how events are mapped by providing multiple `-v` depending on the loglevel

### Complete sample command

```sh
ilagent daemon -v -v \
    -m 127.0.0.1 -q 1883 -n ilagent -e '#' \
    --mqtt_event_key 'il1api112115xxx' \
    --mqtt_map_key_alert_key 'mCode' \
    --mqtt_map_key_summary 'comment' \
    --mqtt_map_key_etype 'state' \
    --mqtt_map_val_etype_alert 'SET' \
    --mqtt_map_val_etype_accept 'ACK' \
    --mqtt_map_val_etype_resolve 'CLR' \
    --mqtt_filter_key 'type' \
    --mqtt_filter_val 'ALARM'
```

## Getting help

We are happy to respond to [GitHub issues][issues] as well.

<br>

#### License

<sup>
Licensed under <a href="LICENSE">Apache License, Version 2.0</a>
Note that, depending on the usage of ilagent, it bundles SQLite and other libraries which might have different licenses
you can read more about them [here](https://github.com/rusqlite/rusqlite#license)
</sup>

[issues]: https://github.com/iLert/ilagent/issues/new
