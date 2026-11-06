# syntax=docker/dockerfile:1
FROM lukemathwalker/cargo-chef:latest-rust-1-bookworm AS chef
RUN apt-get update && apt-get install -y --no-install-recommends cmake clang libclang-dev pkg-config libopus-dev \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /app

FROM chef AS planner
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

FROM chef AS builder
COPY --from=planner /app/recipe.json recipe.json
# Dependencies (incl. whisper.cpp) are cached in this layer until Cargo.toml/lock change.
RUN cargo chef cook --release --recipe-path recipe.json
COPY . .
RUN cargo build --release

FROM debian:bookworm-slim AS model
# Pinned to a whisper.cpp HF commit and verified by sha256 (the file's LFS oid). `ADD --checksum`
# needs Dockerfile syntax >= 1.6, hence the `# syntax` line above.
ADD --checksum=sha256:a03779c86df3323075f5e796cb2ce5029f00ec8869eee3fdfb897afe36c6d002 \
    https://huggingface.co/ggerganov/whisper.cpp/resolve/5359861c739e955e79d9a303bcbc70fb988958b1/ggml-base.en.bin \
    /models/ggml-base.en.bin
RUN chmod 644 /models/ggml-base.en.bin

FROM debian:bookworm-slim
# Links the GHCR package to the repo (so it shows there and can inherit its visibility).
LABEL org.opencontainers.image.source=https://github.com/SAUCE-2/NiceNovember
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates libopus0 libgomp1 \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --uid 1000 --create-home nice \
    && mkdir -p /data /models && chown nice /data
COPY --from=model /models /models
COPY --from=builder /app/target/release/nicenovember /usr/local/bin/nicenovember
USER nice
ENV DATABASE_URL=sqlite:///data/nicenovember.db \
    WHISPER_MODEL=/models/ggml-base.en.bin
VOLUME /data
CMD ["nicenovember"]
