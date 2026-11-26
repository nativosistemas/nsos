# Entorno de desarrollo de nsos: Rust (versión de rust-toolchain.toml),
# QEMU x86_64 y ARM64, xorriso, OVMF, Lean (elan) y Kani.
FROM ubuntu:24.04

ARG DEBIAN_FRONTEND=noninteractive
RUN apt-get update && apt-get install -y --no-install-recommends \
        build-essential ca-certificates curl git make python3 \
        qemu-system-x86 qemu-system-arm qemu-efi-aarch64 ovmf \
        xorriso mtools dosfstools \
    && rm -rf /var/lib/apt/lists/*

ARG USERNAME=dev
RUN useradd -m -s /bin/bash "$USERNAME"
USER $USERNAME
ENV PATH=/home/$USERNAME/.cargo/bin:/home/$USERNAME/.elan/bin:$PATH

# Rust: rustup sin toolchain; la del repositorio (rust-toolchain.toml) se
# instala sola en el primer `cargo`.
RUN curl -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain none --profile minimal

# Lean: elan en una versión fija; la versión de Lean sale de
# verification/lean/lean-toolchain.
ARG ELAN_VERSION=v4.4.0
RUN curl -sSfL "https://raw.githubusercontent.com/leanprover/elan/${ELAN_VERSION}/elan-init.sh" \
    | sh -s -- -y --default-toolchain none --no-modify-path

# Kani: la misma versión que la CI (.github/workflows/ci.yml).
ARG KANI_VERSION=0.68.0
RUN rustup toolchain install stable --profile minimal \
    && cargo +stable install --locked kani-verifier --version "${KANI_VERSION}" \
    && cargo kani setup
