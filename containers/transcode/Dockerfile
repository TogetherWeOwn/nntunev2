# transcode: batch-only ffmpeg/opus transcode Container. No credentials, no
# outbound network at runtime; the transcode Worker is the only caller.
# Build context is the repo root (like containers/repair).
#
# The encoder is the Debian ffmpeg (libopus-enabled) from the
# digest-pinned slim base below: the base digest pins the snapshot the
# encoder comes from, so the binary only changes on a deliberate base
# bump. The encode contract itself (argv + Ogg serial normalisation) is
# pinned in `crates/nntune-core/src/transcode.rs` and shared by every
# producer; the image self-test (`tests/determinism_self_test.sh`, run
# in CI) asserts the encoder carries libopus, encodes one fixture twice
# to identical bytes, and emits ffprobe-valid 48 kHz stereo Opus. A base
# bump that changes encoder bytes only mints new content hashes — old
# artifact rows orphan and sweep, never corrupt.

FROM rust:1-bookworm@sha256:48b7f435b8fc6378ce43b44f15e3a47fd8919b2825f835c39bd4bd3da50af5b8 AS server
WORKDIR /src
# The root Cargo.lock is tracked, proven fresh in CI (`cargo metadata
# --locked`) and audited by cargo-deny. Copy the full (dockerignored)
# workspace so Cargo can resolve all member manifests on a clean checkout,
# then build the single server package --locked so the shipped binary only
# ever links the audited dependency set.
COPY . .
RUN cargo build --locked --release -p nntune-transcode-server \
 && install -m0755 target/release/nntune-transcode-server /usr/local/bin/nntune-transcode-server

FROM debian:bookworm-slim@sha256:7c7b2c966bc9ee8cedfeef67e0e279108992c77681fa595db4a9d65c06ccc587 AS runtime
# ffmpeg ships the encoder and ffprobe (the self-test's validity oracle).
RUN apt-get update && apt-get install -y --no-install-recommends ffmpeg \
 && rm -rf /var/lib/apt/lists/* \
 && useradd --system --home-dir /scratch transcode \
 && mkdir -p /scratch && chown transcode:transcode /scratch
COPY --from=server /usr/local/bin/nntune-transcode-server /usr/local/bin/nntune-transcode-server
ENV TRANSCODE_SCRATCH_DIR=/scratch TRANSCODE_FFMPEG_BIN=/usr/bin/ffmpeg
USER transcode
EXPOSE 8080
CMD ["/usr/local/bin/nntune-transcode-server"]
