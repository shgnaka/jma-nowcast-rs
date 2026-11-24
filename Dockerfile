FROM rust:1.100-slim-bookworm AS build
WORKDIR /build
COPY Cargo.toml Cargo.lock ./
COPY src/ src/
RUN cargo build --release --locked

FROM debian:bookworm-slim AS runtime
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl \
    && rm -rf /var/lib/apt/lists/*
COPY --from=build /build/target/release/jma-nowcast-rs /usr/local/bin/jma-nowcast-rs

ENV JMA_PRODUCTION=1
USER 10001:10001
EXPOSE 8082
ENTRYPOINT ["/usr/local/bin/jma-nowcast-rs"]
CMD ["serve", "--bind", "0.0.0.0:8082"]
