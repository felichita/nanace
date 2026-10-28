# syntax=docker/dockerfile:1
#
# nanace API server for container platforms such as Kubernetes. The
# bge-small embedding model is baked in, so containers start without network
# access to Hugging Face. nanace-mcp is not included: Claude runs it on each
# user's machine (it ships with the desktop app and the release archives).
#
# Debian 13 (trixie): the ONNX Runtime libraries used by fastembed need
# glibc 2.38+.

FROM rust:1.99-trixie AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock rustfmt.toml ./
COPY crates crates
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release --locked -p nanace-server \
    && cp target/release/nanace-server /usr/local/bin/
RUN FASTEMBED_CACHE_DIR=/models nanace-server fetch-model

FROM debian:trixie-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates libssl3t64 \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --uid 10001 --no-create-home --shell /usr/sbin/nologin nanace
COPY --from=build /usr/local/bin/nanace-server /usr/local/bin/
COPY --from=build --chown=10001:10001 /models /models
ENV FASTEMBED_CACHE_DIR=/models \
    NANACE_BIND=0.0.0.0:8080 \
    RUST_LOG=info
USER 10001
EXPOSE 8080
ENTRYPOINT ["nanace-server"]
