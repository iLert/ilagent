FROM rust:1.69-bullseye as builder
WORKDIR /usr/src/ilagent
COPY . .
RUN cargo install --path .

FROM debian:bullseye-slim
RUN apt-get update && apt-get install -y openssl && rm -rf /var/lib/apt/lists/*
COPY --from=builder /usr/local/cargo/bin/ilagent /usr/local/bin/ilagent
ENTRYPOINT ["ilagent"]