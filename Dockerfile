# syntax=docker/dockerfile:1
FROM lukemathwalker/cargo-chef:latest-rust-1-trixie AS chef
RUN apt-get update && apt-get install -y --no-install-recommends cmake clang libclang-dev pkg-config libopus-dev \
    libvulkan-dev glslc \
    && rm -rf /var/lib/apt/lists/*
# Build whisper.cpp for any x86-64 CPU with AVX2, not for the build machine's: by default it's tuned
# with -march=native, so an image built on a CI runner with AVX-512/AMX dies with "Illegal
# instruction" while loading the model on the Steam Machine's AMD CPU. whisper-rs-sys passes GGML_*
# variables to whisper.cpp's CMake. scripts/check-portable.sh guards this in CI.
ENV GGML_NATIVE=OFF
WORKDIR /app

FROM chef AS planner
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

FROM chef AS builder
COPY --from=planner /app/recipe.json recipe.json
# Dependencies (incl. whisper.cpp) are cached in this layer until Cargo.toml/lock change.
RUN cargo chef cook --release --features gpu --recipe-path recipe.json
COPY . .
# Shown by /status; .git is dockerignored, so CI passes the commit and its version in.
ARG GIT_SHA VERSION
RUN cargo build --release --features gpu

FROM debian:trixie-slim AS model
# Pinned to a whisper.cpp HF commit and verified by sha256 (the file's LFS oid). `ADD --checksum`
# needs Dockerfile syntax >= 1.6, hence the `# syntax` line above.
ADD --checksum=sha256:394221709cd5ad1f40c46e6031ca61bce88931e6e088c188294c6d5a55ffa7e2 \
    https://huggingface.co/ggerganov/whisper.cpp/resolve/5359861c739e955e79d9a303bcbc70fb988958b1/ggml-large-v3-turbo-q5_0.bin \
    /models/ggml-large-v3-turbo-q5_0.bin
RUN chmod 644 /models/ggml-large-v3-turbo-q5_0.bin

# Trixie rather than bookworm for a Mesa (25.x) whose RADV drives the Steam Machine's RDNA3 GPU well.
FROM debian:trixie-slim
# Links the GHCR package to the repo (so it shows there and can inherit its visibility).
LABEL org.opencontainers.image.source=https://github.com/SAUCE-2/NiceNovember
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates libopus0 libgomp1 \
    libvulkan1 mesa-vulkan-drivers \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --uid 1000 --create-home nice \
    && mkdir -p /data /models && chown nice /data
COPY --from=model /models /models
COPY --from=builder /app/target/release/nicenovember /usr/local/bin/nicenovember
USER nice
ENV DATABASE_URL=sqlite:///data/nicenovember.db \
    WHISPER_MODEL=/models/ggml-large-v3-turbo-q5_0.bin
VOLUME /data
# The dashboard (`/log dashboard`).
EXPOSE 8080
CMD ["nicenovember"]
