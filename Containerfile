FROM ghcr.io/magicloud/rust-stable:latest AS builder

WORKDIR /usr/src/myapp
COPY . .

ENV SCCACHE_DIR=/sccache
ENV XDG_RUNTIME_DIR=/tmp/sccache-runtime
ENV SCCACHE_SERVER_UDS=/tmp/sccache-runtime/sccache.socket

RUN --mount=type=cache,target=/sccache,id=rust mkdir -p /tmp/sccache-runtime && sccache --start-server && cargo install --path . --target x86_64-unknown-linux-musl && sccache --stop-server


FROM alpine:latest

RUN adduser -D worker -u 1000
USER 1000

EXPOSE 443/TCP

COPY --from=builder /usr/local/cargo/bin/path-k8s-webhook /usr/local/bin/path-k8s-webhook

ENTRYPOINT ["path-k8s-webhook"]
