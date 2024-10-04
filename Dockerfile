FROM rust:1.81-bookworm as builder
RUN apt-get update && apt-get install -y cmake
WORKDIR /usr/src/ilagent
COPY . .
RUN cargo install --path .

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y openssl ca-certificates && rm -rf /var/lib/apt/lists/*
RUN update-ca-certificates
COPY --from=builder /usr/local/cargo/bin/ilagent /usr/local/bin/ilagent
ENTRYPOINT ["ilagent"]