# repair: batch-only par2 repair Container. No credentials, no outbound
# network at runtime; the repair Worker is the only caller (ADR 0001/0009).
# Build context is the repo root (like containers/mb-import).

# Pinned par2cmdline-turbo, built from the release tarball and verified
# by sha256. Re-pin deliberately; the digest is the release asset digest.
FROM debian:bookworm-slim@sha256:7ff6ba31527b9e336162fa061746c366c4b2032f4d2cff2078bea43f78efe3f8 AS par2
ARG PAR2_VERSION=1.5.0
ARG PAR2_SHA256=035584144d3ac3b08fb5d7d4b844f60cf97404b6469f6aed7d9b9430a25bef0e
RUN apt-get update && apt-get install -y --no-install-recommends \
    build-essential ca-certificates curl xz-utils \
 && rm -rf /var/lib/apt/lists/*
WORKDIR /build
RUN curl -fsSL -o par2.tar.xz \
      "https://github.com/animetosho/par2cmdline-turbo/releases/download/v${PAR2_VERSION}/par2cmdline-turbo-${PAR2_VERSION}.tar.xz" \
 && echo "${PAR2_SHA256}  par2.tar.xz" | sha256sum -c - \
 && tar -xJf par2.tar.xz --strip-components=1 \
 && ./configure --quiet \
 && make -j"$(nproc)" --quiet \
 && install -m0755 par2 /usr/local/bin/par2

FROM rust:1-bookworm@sha256:48b7f435b8fc6378ce43b44f15e3a47fd8919b2825f835c39bd4bd3da50af5b8 AS server
WORKDIR /src
# The root Cargo.lock is tracked, proven fresh in CI (`cargo metadata
# --locked`) and audited by cargo-deny. Copy the full (dockerignored)
# workspace so Cargo can resolve all member manifests on a clean checkout,
# then build the single server package --locked so the shipped binary only
# ever links the audited dependency set.
COPY . .
RUN cargo build --locked --release -p nntune-repair-server \
 && install -m0755 target/release/nntune-repair-server /usr/local/bin/nntune-repair-server

FROM debian:bookworm-slim@sha256:7ff6ba31527b9e336162fa061746c366c4b2032f4d2cff2078bea43f78efe3f8 AS runtime
# util-linux provides /usr/bin/prlimit, the kernel resource limiter the
# bounded repair runner always wraps the binary in (fail-closed without).
RUN apt-get update && apt-get install -y --no-install-recommends util-linux \
 && rm -rf /var/lib/apt/lists/* \
 && useradd --system --home-dir /scratch repair \
 && mkdir -p /scratch && chown repair:repair /scratch
COPY --from=par2 /usr/local/bin/par2 /usr/local/bin/par2
COPY --from=server /usr/local/bin/nntune-repair-server /usr/local/bin/nntune-repair-server
ENV REPAIR_SCRATCH_DIR=/scratch REPAIR_PAR2_BIN=/usr/local/bin/par2
USER repair
EXPOSE 8080
CMD ["/usr/local/bin/nntune-repair-server"]
