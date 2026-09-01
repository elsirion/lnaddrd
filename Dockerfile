# ---- Build Stage ----
FROM rust:1.86 AS builder
WORKDIR /app

# Install build dependencies
RUN apt-get update && apt-get install -y pkg-config libssl-dev build-essential

# Cache dependencies
COPY Cargo.toml Cargo.lock ./
RUN mkdir src && echo "fn main() {}" > src/main.rs
RUN cargo build --release || true

# Build actual source
COPY . .
RUN cargo build --release

# ---- Runtime Stage ----
FROM debian:bookworm-slim
WORKDIR /app

# Install runtime dependencies
RUN apt-get update && apt-get install -y ca-certificates && rm -rf /var/lib/apt/lists/*
RUN useradd --system --home-dir /var/lib/lnaddrd --shell /usr/sbin/nologin lnaddrd \
    && install -d -o lnaddrd -g lnaddrd -m 0700 /var/lib/lnaddrd

# Copy the binary from the builder
COPY --from=builder /app/target/release/lnaddrd /usr/local/bin/lnaddrd

# Expose the default port
EXPOSE 8080

# Set environment variables for configuration (can be overridden)
ENV LNADDRD_BIND=0.0.0.0:8080
ENV LNADDRD_DATABASE_PATH=/var/lib/lnaddrd/lnaddrd.sqlite3
ENV LNADDRD_ROOT_SECRET_FILE=/var/lib/lnaddrd/root-secret
ENV LNADDRD_ADMIN_PASSWORD_FILE=/var/lib/lnaddrd/admin-password

VOLUME ["/var/lib/lnaddrd"]

USER lnaddrd

# Entrypoint
ENTRYPOINT ["/usr/local/bin/lnaddrd"]
