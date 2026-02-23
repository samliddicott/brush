# How to build Linux packages

This repo now has two packaging targets:

* Debian package (`.deb`) via `cargo-deb`.
* RPM package via SRPM (`.src.rpm`) and rebuild.

## Build a `.deb`

Prerequisite:

```bash
cargo install cargo-deb
```

Build:

```bash
./scripts/package-deb.sh
```

Output is produced by `cargo-deb` (typically under `target/debian/`).

## Build an SRPM (`.src.rpm`)

Prerequisite:

```bash
# distro-specific package name may vary
sudo dnf install rpm-build
```

Build:

```bash
./scripts/build-srpm.sh
```

Outputs:

* source tarball: `dist/rpm/brush-shell-<version>.tar.gz`
* SRPM: `dist/rpm/brush-shell-<version>-1*.src.rpm`

## Build binary RPM via SRPM

```bash
./scripts/build-rpm-via-srpm.sh
```

This intentionally routes through SRPM first:

1. generate source tarball
2. build SRPM from `packaging/rpm/brush-shell.spec`
3. rebuild SRPM into binary RPM(s)

RPM outputs are written under `dist/rpm/`.
