# Python embedding model

`brush` supports two user-facing Python forms:

- `py ...` for token/argument-based invocation
- `PYTHON ... END_PYTHON` for block-based invocation

## Why both forms exist

`py` is compact and shell-like for one-liners and callable dispatch.

`PYTHON ... END_PYTHON` is robust for multiline code and avoids shell alias parsing timing issues (especially around command substitution boundaries and literal `)` in payloads).

## Block model

Internally, block syntax is treated as Python source input for the same execution core used by `py`.

Behavioral contract:

- exact, zero-indented `END_PYTHON` terminator
- raw Python body (no shell expansion)
- options can be supplied on the `PYTHON` command line
- regular shell redirections and pipeline placement are supported
- dedent defaults to enabled for block mode

## Why dedent defaults on block mode

Most shell scripts indent embedded language blocks to match surrounding shell control flow. Without dedent, valid-looking block code often raises `unexpected indent` in Python.

Default dedent keeps block ergonomics predictable while preserving an explicit opt-out (`--no-dedent`) for literal indentation scenarios.

## Execution context

Python executes in the embedded runtime and can interact with shell state through the `bash` bridge (`bash.vars`, `bash.env`, callbacks, ties, shared mapping, and stack view as implemented).

Runtime status conventions remain shell-friendly:

- `0` success
- `1` Python exception
- `130` interruption
