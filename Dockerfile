# syntax=docker/dockerfile:1

FROM rust:1.95-slim-trixie AS builder

WORKDIR /app

ARG CARGO_FEATURES=production
ARG TARGETARCH
ARG DEBIAN_FRONTEND=noninteractive
ENV CARGO_NET_GIT_FETCH_WITH_CLI=true

RUN apt-get update && apt-get install -y --no-install-recommends \
    git \
    pkg-config \
    libssl-dev \
    && rm -rf /var/lib/apt/lists/*

COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY migrations ./migrations

RUN --mount=type=cache,id=seren-router-registry-${TARGETARCH},target=/usr/local/cargo/registry \
    --mount=type=cache,id=seren-router-git-${TARGETARCH},target=/usr/local/cargo/git \
    --mount=type=cache,id=seren-router-target-${TARGETARCH},target=/app/target \
    --mount=type=secret,id=github_token,required=false \
    TOKEN="$(cat /run/secrets/github_token 2>/dev/null || true)" \
    && if [ -n "$TOKEN" ]; then \
        git config --global url."https://x-access-token:${TOKEN}@github.com/serenorg/".insteadOf "https://github.com/serenorg/"; \
    fi \
    && cargo build --release --locked ${CARGO_FEATURES:+--features "$CARGO_FEATURES"} --bin seren-router \
    && cp /app/target/release/seren-router /usr/local/bin/seren-router \
    && rm -f /root/.gitconfig

FROM debian:trixie-slim AS runtime

WORKDIR /app
ARG DEBIAN_FRONTEND=noninteractive

RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    curl \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /usr/local/bin/seren-router /app/seren-router

RUN useradd -r -s /bin/false appuser
USER appuser

EXPOSE 8000

HEALTHCHECK --interval=30s --timeout=3s --start-period=5s --retries=3 \
    CMD curl -f http://localhost:8000/readyz || exit 1

ENV RUST_LOG=seren_router=info,tower_http=info
CMD ["/app/seren-router"]
