# Builds the `serve` binary — the HTTP wrapper that lets the browser page run
# the real probe. The CLI is unaffected by any of this.

FROM rust:1-slim-bookworm AS build
WORKDIR /src

# Dependencies first, against a stub main, so that editing the source does not
# re-download and re-compile the whole tree on every build.
COPY Cargo.toml Cargo.lock ./
RUN mkdir -p src/bin \
    && echo 'fn main() {}' > src/main.rs \
    && echo 'fn main() {}' > src/bin/serve.rs \
    && touch src/lib.rs \
    && cargo build --release --features server --bin serve \
    && rm -rf src

COPY src ./src
# Cargo stats mtimes; the stub artifacts would otherwise be considered current.
RUN touch src/main.rs src/lib.rs src/bin/serve.rs \
    && cargo build --release --features server --bin serve

FROM debian:bookworm-slim AS runtime

# The probe speaks wss:// to public endpoints, so the image needs root
# certificates — without them every connection fails TLS verification and the
# cause is not obvious from the error.
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*

# Nothing here needs root, and a probe that dials arbitrary public addresses is
# exactly the sort of thing to run unprivileged.
RUN useradd --system --create-home --uid 10001 probe
USER probe
WORKDIR /app

COPY --from=build /src/target/release/serve /usr/local/bin/serve
COPY web /app/web

ENV WEB_ROOT=/app/web \
    RUST_LOG=info \
    PORT=8080
EXPOSE 8080

CMD ["serve"]
