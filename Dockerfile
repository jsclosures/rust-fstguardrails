# Stage 1: Build environment
FROM rust:1.75-slim-bookworm AS builder

# Install standard compilation tools
RUN apt-get update && apt-get install -y --no-install-recommends \
    build-essential \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /usr/src/lume

# Copy compilation manifests and source code
COPY Cargo.toml Cargo.lock ./
COPY src/ ./src/
COPY examples/ ./examples/

# Compile optimized release binary using modern BuildKit cargo cache mounts
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/src/lume/target \
    cargo build --release && \
    cp target/release/lume /usr/local/bin/lume

# Stage 2: Minimal runtime environment
FROM debian:bookworm-slim AS runner

# Install ca-certificates for secure HTTPS requests (required by ureq)
RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    && rm -rf /var/lib/apt/lists/*

# Create a non-privileged system user for production-grade security
RUN groupadd -g 10001 lume && \
    useradd -u 10001 -g lume -s /bin/sh -m lume

WORKDIR /app

# Copy compiled unibinary from builder stage
COPY --from=builder /usr/local/bin/lume /usr/local/bin/lume

# Copy default FST dictionary data
COPY --from=builder /usr/src/lume/examples/data /app/data

# Secure runtime directory ownership
RUN chown -R lume:lume /app

# Switch to non-privileged user execution
USER lume:lume

# Predefine default environment variables
ENV DATA=/app/data
ENV PORT=8080
ENV RUST_BACKTRACE=1

# Expose port for HTTP tag-server mode
EXPOSE 8080

# Configure the entrypoint to route directly to our unibinary
ENTRYPOINT ["/usr/local/bin/lume"]

# Default subcommand boots the tag-server API
CMD ["tag-server"]
