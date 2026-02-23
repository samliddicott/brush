# Build Guide (CLI)

All commands below run from the `brush/` repository root.

## 1) Rust builds (no Python bridge)

Debug build:

```bash
cargo build -p brush-shell
```

Release build:

```bash
cargo build --release -p brush-shell
```

## 2) Rust builds with Python bridge (`python-pyo3`)

Debug build:

```bash
cargo build -p brush-shell --features python-pyo3
```

Release build:

```bash
cargo build --release -p brush-shell --features python-pyo3
```

## 3) Debian package (`.deb`)

Install tool once:

```bash
cargo install cargo-deb
```

Build without Python bridge:

```bash
./scripts/package-deb.sh
```

Build with Python bridge:

```bash
./scripts/package-deb.sh --features python-pyo3
```

## 4) Source RPM (`.src.rpm`)

Requires `rpmbuild`.

Build without Python bridge:

```bash
./scripts/build-srpm.sh
```

Build with Python bridge:

```bash
BRUSH_CARGO_FEATURES=python-pyo3 ./scripts/build-srpm.sh
```

## 5) Binary RPM via SRPM

Requires `rpmbuild`.

Build without Python bridge:

```bash
./scripts/build-rpm-via-srpm.sh
```

Build with Python bridge:

```bash
BRUSH_CARGO_FEATURES=python-pyo3 ./scripts/build-rpm-via-srpm.sh
```

## Output locations

- Cargo build artifacts:
  - debug: `target/debug/`
  - release: `target/release/`
- `.deb`: typically under `target/debian/`
- `.src.rpm`: `dist/rpm/`
- `.rpm`: `dist/rpm/<arch>/`
