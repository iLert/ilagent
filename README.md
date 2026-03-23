# ilagent

![Docker Image Size (latest by date)](https://img.shields.io/docker/image-size/ilert/ilagent?sort=date)
![Docker Image Version (latest by date)](https://img.shields.io/docker/v/ilert/ilagent?sort=date)
![GitHub Workflow Status](https://img.shields.io/github/actions/workflow/status/iLert/ilagent/docker-release.yml)

The ilert Agent 🦀 📦 is a lightweight program that lets you easily integrate your on-premise systems with ilert.

<p align="center"><img src="/docs/misc/froggo.png?raw=true"/></p>

## What it does

* Send events and heartbeats from the command line
* Run a local HTTP proxy server with a retry queue
* Consume MQTT messages and forward them to ilert
* Consume Apache Kafka messages and forward them to ilert
* Map and filter consumer messages to ilert events
* Sync escalation policy levels from external systems

## Quick start

### Docker

```sh
docker run ilert/ilagent --help
```

### From source

```sh
git clone git@github.com:iLert/ilagent.git
cd ilagent
cargo build --release
./target/release/ilagent --help
```

### Install script (macOS / Linux)

```sh
curl -sL https://raw.githubusercontent.com/iLert/ilagent/master/install.sh | bash -
```

## Documentation

Full documentation is available at [docs.ilert.com](https://docs.ilert.com/developer-docs/rest-api/client-libraries/ilagent).

## Examples

See the [`examples/`](examples/) directory for ready-to-run sample commands.

## Cross-compiling

Requires [cross](https://github.com/cross-rs/cross) (`cargo install cross`, for Apple Silicon: `cargo install cross --git https://github.com/cross-rs/cross`).

```sh
cargo build --release                                          # Mac / host
cross build --release --target x86_64-unknown-linux-gnu        # Linux
cross build --release --target x86_64-pc-windows-gnu           # Windows
cross build --release --target arm-unknown-linux-gnueabihf     # ARM
```

## Getting help

We are happy to respond to [GitHub issues](https://github.com/iLert/ilagent/issues/new).

## License

Licensed under [Apache License, Version 2.0](LICENSE).
Note that, depending on the usage of ilagent, it bundles SQLite and other libraries which might have different licenses — you can read more about them [here](https://github.com/rusqlite/rusqlite#license).
