# ferberus development image. Used locally (devcontainer / docker run) and in CI.
# Build context is the repository root:
#   docker build -f .devcontainer/Dockerfile -t ferberus-dev .
#
# Every tool is pinned. To add or bump something: edit this file (or the
# requirements files next to it), rebuild, and log the change in NOTES.md.

FROM ghcr.io/astral-sh/uv:0.13.8 AS uv

FROM debian:trixie-20261104-slim

ARG USERNAME=dev
ARG USER_UID=1000
ARG USER_GID=1000

# Pinned upstream reference implementation (branch 1.3.x).
ARG CERBERUS_SHA=83f5dada76aa3179ef21b677a7f4235bf20c1566

# Pinned tool versions.
ARG RUSTUP_VERSION=1.29.3
ARG NIGHTLY=nightly-2026-11-10
ARG CARGO_BINSTALL_VERSION=1.27.0
ARG CARGO_NEXTEST_VERSION=0.9.152
ARG CARGO_DENY_VERSION=0.20.2
ARG CARGO_FUZZ_VERSION=0.13.2
ARG SAMPLY_VERSION=0.13.1
ARG FLAMEGRAPH_VERSION=0.6.15

ENV DEBIAN_FRONTEND=noninteractive \
    LANG=C.UTF-8 \
    RUSTUP_HOME=/usr/local/rustup \
    CARGO_HOME=/usr/local/cargo \
    CARGO_TARGET_DIR=/var/cache/ferberus/target \
    UV_CACHE_DIR=/var/cache/ferberus/uv \
    UV_PYTHON_INSTALL_DIR=/opt/python \
    UV_PYTHON_PREFERENCE=only-managed \
    UV_LINK_MODE=copy \
    PATH=/usr/local/cargo/bin:/opt/venvs/3.14/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin

# System packages: toolchain prerequisites, perf, debugging helpers.
RUN apt-get update && apt-get install -y --no-install-recommends \
        build-essential ca-certificates curl git pkg-config \
        clang lld \
        linux-perf libcap2-bin \
        gdb less procps jq xz-utils \
    && rm -rf /var/lib/apt/lists/*

# perf as non-root: the container is started with --cap-add=PERFMON and
# --cap-add=SYS_PTRACE; file capabilities let the unprivileged user use them.
RUN setcap cap_perfmon,cap_sys_ptrace+ep "$(readlink -f "$(command -v perf)")"

RUN groupadd --gid ${USER_GID} ${USERNAME} \
    && useradd --uid ${USER_UID} --gid ${USER_GID} -m -s /bin/bash ${USERNAME} \
    && mkdir -p ${RUSTUP_HOME} ${CARGO_HOME}/registry ${CARGO_HOME}/git \
        ${CARGO_TARGET_DIR} ${UV_CACHE_DIR} /opt/python /opt/venvs /opt/cerberus \
        /workspaces/ferberus \
    && chown -R ${USERNAME}:${USERNAME} ${RUSTUP_HOME} ${CARGO_HOME} \
        /var/cache/ferberus /opt/python /opt/venvs /opt/cerberus /workspaces

COPY --from=uv /uv /uvx /usr/local/bin/

USER ${USERNAME}

# Rust: stable pinned by rust-toolchain.toml, plus a pinned nightly for cargo-fuzz.
COPY --chown=${USERNAME}:${USERNAME} rust-toolchain.toml /tmp/toolchain/rust-toolchain.toml
RUN curl --proto '=https' --tlsv1.2 -sSf \
        "https://static.rust-lang.org/rustup/archive/${RUSTUP_VERSION}/$(uname -m)-unknown-linux-gnu/rustup-init" \
        -o /tmp/rustup-init \
    && chmod +x /tmp/rustup-init \
    && /tmp/rustup-init -y --no-modify-path --profile minimal --default-toolchain none \
    && rm /tmp/rustup-init \
    && cd /tmp/toolchain && rustup toolchain install \
    && rustup default "$(sed -n 's/^channel = "\(.*\)"/\1/p' rust-toolchain.toml)" \
    && rustup toolchain install ${NIGHTLY} --profile minimal --component rust-src,llvm-tools \
    && rustc --version && cargo --version

# Cargo tools (prebuilt binaries where upstream publishes them).
RUN curl -sSfL "https://github.com/cargo-bins/cargo-binstall/releases/download/v${CARGO_BINSTALL_VERSION}/cargo-binstall-$(uname -m)-unknown-linux-musl.tgz" \
        | tar -xz -C ${CARGO_HOME}/bin \
    && cargo binstall -y --locked \
        cargo-nextest@${CARGO_NEXTEST_VERSION} \
        cargo-deny@${CARGO_DENY_VERSION} \
        cargo-fuzz@${CARGO_FUZZ_VERSION} \
        samply@${SAMPLY_VERSION} \
        flamegraph@${FLAMEGRAPH_VERSION} \
    && rm -rf ${CARGO_HOME}/registry/* ${CARGO_HOME}/git/* /tmp/cargo-*

# CPython interpreters (GIL and free-threaded builds), managed by uv.
# Free-threaded builds from 3.14: PyO3 does not support 3.13t (NOTES.md, D1).
ARG PYTHON_VERSIONS="3.9 3.10 3.11 3.12 3.13 3.14 3.15 3.14t 3.15t"
RUN uv python install ${PYTHON_VERSIONS}

# Reference implementation + upstream test suite, at the pinned commit.
RUN git clone https://github.com/pyeve/cerberus.git /opt/cerberus \
    && git -C /opt/cerberus checkout --detach ${CERBERUS_SHA} \
    && test "$(git -C /opt/cerberus rev-parse HEAD)" = "${CERBERUS_SHA}"

# One venv per interpreter with the pinned tooling. Real Cerberus is
# deliberately NOT installed into them: it is put on sys.path explicitly
# (/opt/cerberus) where the reference is wanted, so that `import cerberus`
# can be pointed at ferberus when running the upstream suite.
COPY --chown=${USERNAME}:${USERNAME} .devcontainer/requirements-*.txt /opt/venvs/
RUN set -eux; for v in ${PYTHON_VERSIONS}; do \
        uv venv --python "$v" "/opt/venvs/$v"; \
        case "$v" in \
            3.9|3.10) req=legacy ;; \
            *) req=dev ;; \
        esac; \
        uv pip install --python "/opt/venvs/$v/bin/python" -r "/opt/venvs/requirements-$req.txt"; \
    done; \
    uv pip install --python /opt/venvs/3.14/bin/python -r /opt/venvs/requirements-docs.txt; \
    rm -rf ${UV_CACHE_DIR}/*

WORKDIR /workspaces/ferberus
CMD ["sleep", "infinity"]
