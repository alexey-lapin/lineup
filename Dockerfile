# ---- web UI ----------------------------------------------------------------
FROM node:24-alpine AS web
WORKDIR /src/web
RUN corepack enable
COPY web/package.json web/pnpm-lock.yaml web/pnpm-workspace.yaml ./
RUN pnpm install --frozen-lockfile
COPY web/ ./
RUN pnpm build

# ---- server + CLI ----------------------------------------------------------
FROM rust:1.97-alpine AS rust
RUN apk add --no-cache musl-dev build-base cmake perl
WORKDIR /src
COPY Cargo.toml Cargo.lock rustfmt.toml ./
COPY crates ./crates
COPY --from=web /src/web/dist ./web/dist
# Drop target/ in the same layer: it is gigabytes and only the binary is needed.
RUN cargo build --release --locked -p lineup && cp target/release/lineup /lineup && rm -rf target

# ---- runtime ---------------------------------------------------------------
FROM alpine:3.22
RUN apk add --no-cache ca-certificates tzdata \
    && adduser -D -H -u 10001 lineup \
    && mkdir -p /data && chown lineup /data
COPY --from=rust /lineup /usr/local/bin/lineup
USER lineup
ENV LINEUP_BIND=0.0.0.0:9300 \
    LINEUP_DATA_DIR=/data \
    LINEUP_SERVER=http://127.0.0.1:9300
VOLUME /data
EXPOSE 9300
HEALTHCHECK --interval=30s --timeout=3s --start-period=10s \
    CMD wget -qO- http://127.0.0.1:9300/healthz >/dev/null || exit 1
ENTRYPOINT ["lineup"]
CMD ["serve"]
