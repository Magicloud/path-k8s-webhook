FROM ghcr.io/magicloud/rust-stable:latest AS builder

WORKDIR /usr/src/myapp
COPY . .

ENV SCCACHE_DIR=/sccache
ENV XDG_RUNTIME_DIR=/tmp/sccache-runtime
ENV SCCACHE_SERVER_UDS=/tmp/sccache-runtime/sccache.socket

ARG IS_CI=false

RUN --mount=type=cache,target=/sccache,id=rust \
    if [ "${IS_CI}" = "true" ]; then \
        sed -i '/\[source\.crates-io\]/,/registry =/d' .cargo/config.toml && \
        sed -i '/rustc-wrapper/d' .cargo/config.toml && \
        cargo install --path .; \
    else \
        mkdir -p /tmp/sccache-runtime && \
        sccache --start-server && \
        cargo install --path . && \
        sccache --stop-server; \
    fi


# FROM alpine:latest
FROM debian:latest

# RUN apk add --no-cache gcompat libgcc && adduser -D worker -u 1000
RUN useradd -u 1000 worker
USER 1000

EXPOSE 443/TCP

COPY --from=builder /usr/local/cargo/bin/path-k8s-webhook /usr/local/bin/path-k8s-webhook

ENTRYPOINT ["path-k8s-webhook"]
