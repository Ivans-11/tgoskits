<h1 align="center">riscv_vcpu</h1>

<p align="center">ArceOS-Hypervisor RISC-V vCPU module</p>

<div align="center">

[![Crates.io](https://img.shields.io/crates/v/riscv_vcpu.svg)](https://crates.io/crates/riscv_vcpu)
[![Docs.rs](https://docs.rs/riscv_vcpu/badge.svg)](https://docs.rs/riscv_vcpu)
[![Rust](https://img.shields.io/badge/edition-2024-orange.svg)](https://www.rust-lang.org/)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](./LICENSE)

</div>

English | [中文](README_CN.md)

# Introduction

`riscv_vcpu` provides ArceOS-Hypervisor RISC-V vCPU module. It is maintained as part of the TGOSKits component set and is intended for Rust projects that integrate with ArceOS, AxVisor, or related low-level systems software.

## Quick Start

### Installation

Add this crate to your `Cargo.toml`:

```toml
[dependencies]
riscv_vcpu = "0.5.0"
```

### Run Check and Test

```bash
# Enter the crate directory
cd virtualization/riscv_vcpu

# Format code
cargo fmt --all

# Run clippy
cargo clippy --all-targets --all-features

# Run tests
cargo test --all-features

# Build documentation
cargo doc --no-deps
```

## Integration

### Host privilege mode

The default backend runs in S/HS-mode under SBI firmware. The optional
`m-mode` feature selects an M-mode host at build time: machine trap CSRs,
`mret` guest entry, machine interrupt forwarding and a virtual SBI implementation
that does not forward calls to firmware. Guest execution remains in VS/VU-mode.

When using `axvisor_core`, enable its `m-mode` feature, which also selects
M-mode interrupt guards. Integrations using `riscv_vcpu` directly must likewise
enable `ax-kernel-guard/m-mode` if they use that crate for interrupt locks.
All callers of a single build must use the selected host privilege mode.

The M-mode host owns PMP configuration and must grant access to the memory
used for guest RAM and G-stage page tables. The backend saves/restores machine
delegation, counter access and environment configuration around vCPU binding.
Physical timer, IPI delivery and synchronous remote fences remain host services
through the existing adapter interfaces. The `sstc` feature requires hardware
support for virtual supervisor timer comparators.

### Example

```rust
use riscv_vcpu as _;

fn main() {
    // Integrate `riscv_vcpu` into your project here.
}
```

### Documentation

Generate and view API documentation:

```bash
cargo doc --no-deps --open
```

Online documentation: [docs.rs/riscv_vcpu](https://docs.rs/riscv_vcpu)

# Contributing

1. Fork the repository and create a branch
2. Run local format and checks
3. Run local tests relevant to this crate
4. Submit a PR and ensure CI passes

# License

Licensed under the Apache License, Version 2.0. See [LICENSE](./LICENSE) for details.
