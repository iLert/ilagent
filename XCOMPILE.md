# Cross compilation guide

## Setup zigbuild

Used to use `cross` for that, but with all the native static linking deps `cargo-zigbuild` was better and faster.

```sh
brew install zig
cargo install cargo-zigbuild
rustup target add x86_64-unknown-linux-gnu
rustup target add x86_64-pc-windows-gnu
rustup target add arm-unknown-linux-gnueabihf
```

> Note: that curl stub is currently needed as a workaround for an error with DWITH_CURL in librdkafka

Run these from the project root dir:

## Build Mac (Apple silicon)

```sh
cargo build --release
```

## Build Linux x86_64

```sh
CFLAGS="-I$(pwd)/stubs" \
CXXFLAGS="-I$(pwd)/stubs" \
RDKAFKA_SYS_CMAKE_ARGS="-DWITH_CURL=0" \
cargo zigbuild --target x86_64-unknown-linux-gnu --release
```

## Build Windows x86_64

```sh
CFLAGS="-I$(pwd)/stubs" \
CXXFLAGS="-I$(pwd)/stubs" \
RDKAFKA_SYS_CMAKE_ARGS="-DWITH_CURL=0" \
cargo zigbuild --target x86_64-pc-windows-gnu --release
```

## Build ARM

```sh
CFLAGS="-I$(pwd)/stubs" \
CXXFLAGS="-I$(pwd)/stubs" \
RDKAFKA_SYS_CMAKE_ARGS="-DWITH_CURL=0" \
cargo zigbuild --target arm-unknown-linux-gnueabihf --release
```

## Collect binaries

```sh
mkdir -p x_builds && cp target/release/ilagent x_builds/ilagent_mac && cp target/x86_64-unknown-linux-gnu/release/ilagent x_builds/ilagent_linux && cp target/x86_64-pc-windows-gnu/release/ilagent.exe x_builds/ilagent.exe && cp target/arm-unknown-linux-gnueabihf/release/ilagent x_builds/ilagent_arm
```