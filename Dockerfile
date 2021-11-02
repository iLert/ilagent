FROM ekidd/rust-musl-builder AS builder
ADD . ./
RUN sudo chown -R rust:rust /home/rust/src
RUN cargo build --release
RUN strip /home/rust/src/target/x86_64-unknown-linux-musl/release/ilagent

FROM scratch AS runner
COPY --from=builder /home/rust/src/target/x86_64-unknown-linux-musl/release/ilagent \
  /ilagent

ENTRYPOINT ["./ilagent"]