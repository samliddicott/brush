# Python Reference

This page documents Python support in `brush`.

## Availability

Python integration is feature-gated behind `python-pyo3`.

Build with Python support:

```bash
cargo build -p brush-shell --features python-pyo3
```

## Commands

### `py`

`py` executes Python in the embedded runtime.

Common forms:

```bash
py 'print("hello")'
py -e '1 + 2'
py -v out -r value some_callable arg1 arg2
```

Supported options:

- `-e`: expression mode
- `-x`: structured exception mode (`MCBASH_EXCEPTION*` variables)
- `-v VAR`: capture Python stdout into shell variable `VAR`
- `-r VAR`: capture expression/call result into shell variable `VAR`
- `-N` / `-N-`: read Python source from fd `N` (close after read for `-N-`)
- `-t VAR`: tie shell variable `VAR` to Python side
- `-u VAR`: untie shell variable `VAR`
- `--no-dedent`: disable dedent when source is provided via block/fd paths

### `PYTHON ... END_PYTHON`

Native block form:

```bash
PYTHON
x = 42
print(x)
END_PYTHON
```

Rules:

- `END_PYTHON` must be a standalone zero-indented line.
- Block source is raw Python text (no shell expansion inside the block).
- Block mode defaults to dedent enabled; use `--no-dedent` to disable.
- Same-line options are allowed:

```bash
PYTHON -x
raise ValueError("bad")
END_PYTHON
```

- Redirections are allowed:

```bash
PYTHON 2>err.log
print("ok")
END_PYTHON
```

- Pipeline placement is allowed:

```bash
echo ignored | PYTHON | cat
print("ok")
END_PYTHON
```

## Structured Exceptions (`-x`)

When `-x` is used and Python raises, `brush` sets:

- `MCBASH_EXCEPTION`
- `MCBASH_EXCEPTION_MSG`
- `MCBASH_EXCEPTION_TB` (array)
- `MCBASH_EXCEPTION_LANG` (`python`)

## Bridge APIs (implemented surface)

From Python, the `bash` bridge includes the currently implemented subsets used by tests/phases:

- `bash.vars` mapping (shell variable access)
- `bash.env` mapping (exported env view)
- `bash.run(...)`
- `bash.popen(...)` with `poll()`, `wait()`, `communicate()`
- `bash.tie(...)` / `bash.untie(...)`
- `bash.shared` mapping (shared variable backend)
- `bash.stack` read-only stack view

## Exit status

Python execution conventions in brush:

- `0` success
- `1` Python exception
- `130` interruption/cancellation
