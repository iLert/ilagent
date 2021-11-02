FROM rust:1.56 AS builder
WORKDIR /usr/src/ilagent
COPY . .
RUN cargo install --path .

FROM alpine
COPY --from=builder /usr/local/cargo/bin/ilagent /usr/local/bin/ilagent
CMD ["ilagent"]