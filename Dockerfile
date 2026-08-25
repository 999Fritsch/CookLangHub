# Build stage.
FROM rust:1.97-slim-bookworm AS builder

WORKDIR /build

# Cache the dependency build. The dummy main lets cargo compile every
# dependency before the real sources arrive.
COPY Cargo.toml Cargo.lock ./
RUN mkdir src \
 && echo "fn main() {}" > src/main.rs \
 && echo "" > src/lib.rs \
 && cargo build --release \
 && rm -rf src

COPY src ./src
COPY templates ./templates
COPY migrations ./migrations

# Touch the sources so cargo rebuilds them after the cached dependency layer.
RUN touch src/main.rs src/lib.rs && cargo build --release

# Runtime stage.
FROM debian:bookworm-slim AS runtime

# git is the Git adapter of the application. curl serves the health check.
RUN apt-get update \
 && apt-get install --no-install-recommends --yes ca-certificates git curl \
 && rm -rf /var/lib/apt/lists/*

RUN useradd --create-home --uid 1000 cooklanghub \
 && mkdir -p /data \
 && chown cooklanghub:cooklanghub /data

WORKDIR /app
COPY --from=builder /build/target/release/cooklanghub /usr/local/bin/cooklanghub
COPY static ./static

USER cooklanghub

ENV COOKLANGHUB_BIND=0.0.0.0:8080 \
    COOKLANGHUB_DATABASE_URL=sqlite:///data/cooklanghub.db?mode=rwc \
    COOKLANGHUB_STATIC_DIR=/app/static \
    COOKLANGHUB_LOG_FORMAT=json

EXPOSE 8080
VOLUME ["/data"]

HEALTHCHECK --interval=15s --timeout=5s --start-period=10s --retries=5 \
  CMD curl --fail --silent http://127.0.0.1:8080/health > /dev/null || exit 1

CMD ["cooklanghub"]
