# Dev/CI toolchain image: Rust + native deps for whisper.cpp (cmake/clang) and opus.
FROM rust:1-bookworm
RUN apt-get update && apt-get install -y --no-install-recommends cmake clang libclang-dev pkg-config libopus-dev \
    && rm -rf /var/lib/apt/lists/*
RUN rustup component add clippy rustfmt
