# Build from the repository root: docker build -f deploy/Dockerfile .
# (compose.yaml in this directory does that for you.)

FROM rust:1-slim-bookworm AS build
# tokscale-core (price estimates, D042) builds its bundled OpenSSL on Linux.
RUN apt-get update \
 && apt-get install -y --no-install-recommends perl make \
 && rm -rf /var/lib/apt/lists/*
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY schema schema
COPY config/pricing-overrides.json config/pricing-overrides.json
COPY crates/usage-event crates/usage-event
COPY server server
# Only the server and its local crate are in the image (tokscale-core comes
# from git), so the workspace is narrowed to them; versions still come from
# the committed Cargo.lock.
# Small servers: `docker compose build --build-arg BUILD_JOBS=1` caps parallel
# compile jobs so the build does not starve other services of memory.
ARG BUILD_JOBS
RUN sed -i 's|^members = .*|members = ["crates/usage-event", "server"]|' Cargo.toml \
 && cargo build --release -p tokenlens-server ${BUILD_JOBS:+--jobs "$BUILD_JOBS"} \
 && install -s -m 0755 target/release/tokenlens-server /tokenlens-server

FROM debian:bookworm-slim
# Root certificates for downloading the public price catalogs over HTTPS.
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates \
 && rm -rf /var/lib/apt/lists/*
RUN useradd --system --uid 10001 --user-group --home-dir /app --shell /usr/sbin/nologin tokenlens \
 && mkdir -p /app/data /app/backups \
 && chown tokenlens:tokenlens /app/data /app/backups
COPY --from=build /tokenlens-server /usr/local/bin/tokenlens-server
USER tokenlens
WORKDIR /app
ENV DATABASE_PATH=/app/data/tokenlens.sqlite \
    LISTEN_ADDR=0.0.0.0:8787 \
    LISTEN_ALLOW_NON_LOOPBACK=1
EXPOSE 8787
HEALTHCHECK --interval=30s --timeout=5s --start-period=10s --retries=3 \
  CMD ["tokenlens-server", "healthcheck"]
CMD ["tokenlens-server", "serve"]
