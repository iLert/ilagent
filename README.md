# ilagent

The iLert Agent 🦀 📦 is a program that lets you easily integrate your monitoring system with iLert.

## iLert agent

The iLert agent comes in a single binary and helps you to

* Send events from the command line `ilagent event -k il1insada3872867c63 -t ALERT -s 'a summary from the shell'`
* Send heartbeat pings from the command line `ilagent heartbeat -k il1insada3872867c63`
* Monitor a host with regular heartbeats `ilagent daemon -b il1insada3872867c63`
* Run a proxy server with retry-queue for HTTP events and heartbeats on premise `ilagent daemon -p 8977`
* Run a proxy server with retry-queue for MQTT events and heartbeats on premise `ilagent daemon -m 192.168.1.14`

<p align="center"><img src="/demo.gif?raw=true"/></p>

## Downloading / Installing

For MacOS and Linux we also provide this one-liner to automatically install the agent:

```shell script
curl -sL https://raw.githubusercontent.com/iLert/ilagent/master/install.sh | bash -
```

We provide pre compiled binaries for every major OS on the [release page of this repository](https://github.com/iLert/ilagent/releases).

Grab your version

- [Linux x86_64](https://github.com/iLert/ilagent/releases/download/0.2.1/ilagent_linux)
- [Windows x86_64](https://github.com/iLert/ilagent/releases/download/0.2.1/ilagent.exe)
- [Mac x86_64](https://github.com/iLert/ilagent/releases/download/0.2.1/ilagent_mac)
- [ARM (gnueabihf)](https://github.com/iLert/ilagent/releases/download/0.2.1/ilagent_arm)
- [Others][issues]

### Cross compiling

Of course you can also grab the source code and compile it yourself.
Requires cross (`cargo install cross`) to be installed.

- Mac: `cargo build --release`
- Linux: `cross build --release --target x86_64-unknown-linux-gnu`
- Windows: `cross build --release --target x86_64-pc-windows-gnu`
- ARM: `cross build --release --target arm-unknown-linux-gnueabihf`

## Quick gimmicks

- The proxy exposes the exact same API as our public `https://api.ilert.com/api/v1` you can therefore use our [API Docs](https://api.ilert.com/api-docs/#tag/Events)
- When running in `daemon` mode, it is always possible to provide a `-b il1insada3872867c63` heartbeat api key
to monitor the uptime of the agent.
- To adjust the log level you can provide multiple `-v -v #info` verbose flags, default is error.
- The event command supports additional args to add an image `-g 'url'`, link `-l 'url'` or priority `-o 'LOW'`.
- You can adjust the MQTT topics that the proxy is listening on `-e 'ilert/events'` or `-r 'ilert/heartbeats'`
- Per default the `daemon` mode will start an HTTP server that listens on port `8977`.
- The agent will buffer events locally using SQLite3 it will therefore require file system access in `daemon` mode.
- Running detached: `nohup sh -c 'ilagent daemon -m 192.168.1.14 -b il1hbt... -v -v'  > ./ilagent.log 2>&1 &`

You can always run `ilagent --help` or take a look at our [documentation](https://docs.ilert.com/ilagent) for help.

## Getting help

We are happy to respond to [GitHub issues][issues] as well.

<br>

#### License

<sup>
Licensed under <a href="LICENSE">Apache License, Version 2.0</a>
</sup>

[issues]: https://github.com/iLert/ilagent/issues/new
