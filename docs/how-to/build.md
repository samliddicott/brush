# How to build and run

1. Install Rust toolchain. We recommend using [rustup](https://rustup.rs/).
1. Build `brush`: `cargo build`
1. Run `brush`: `cargo run`

## Optional Python (`python-pyo3`)

Embedded Python support (the `py` builtin and Python dispatch paths) is feature-gated.

1. Build with Python support: `cargo build -p brush-shell --features python-pyo3`
1. Run with Python support: `cargo run -p brush-shell --features python-pyo3`
