FROM rust:1.99-slim-bookworm AS builder
WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY src ./src
RUN cargo build --release --locked

FROM debian:bookworm-slim
RUN useradd --create-home --uid 10001 app
COPY --from=builder /app/target/release/distributed-rate-limiter /usr/local/bin/rate-limiter
USER app
EXPOSE 8080
ENTRYPOINT ["/usr/local/bin/rate-limiter"]
