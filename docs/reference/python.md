# Python Reference

This page documents Python support in `brush`.

## Availability

Python integration is feature-gated behind `python-pyo3`.

Build with Python support:

```bash
cargo build -p brush-shell --features python-pyo3
```

## Implementation parity matrix

Status key:

- `implemented`: available now
- `partial`: available with limits noted below
- `planned`: not implemented yet

| Area | Status | Notes |
|---|---|---|
| `py` command (`-e`, `-x`, `-v`, `-r`) | implemented | Core execution and capture modes are available. |
| `PYTHON ... END_PYTHON` block | implemented | Dedent defaults on; same-line options and redirection/pipeline placement supported. |
| `bash.vars` / `bash.env` mappings | implemented | Live reads/writes into shell variable store. |
| Variable attrs (`attrs`, `set_attrs`, `declare`) | implemented | Mirrors shell attribute model exposed by bridge. |
| Ties (`py -t/-u`, `bash.tie/untie`) | implemented | Two-way variable tie hooks available. |
| `bash.fn` map/call namespace | implemented | Function map operations and callable wrapper assignment are available. |
| `bash()` convenience callback | implemented | Raises on non-zero shell status. |
| `bash.run(...)` | partial | `timeout` not implemented. |
| `bash.popen(...)` | partial | Pipe-heavy/streaming parity still incomplete; see caveats below. |
| `shared` builtin + `bash.shared` | implemented | Shared typed variables and Python mapping are available. |
| `bash.stack` | implemented | Read-only stack frame inspection. |

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

Execution behavior:

- callable-first dispatch, then statement execution fallback
- expression mode (`-e`) evaluates and emits result text
- `-v` / `-r` controls where stdout/result values are written

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

- The `PYTHON` command supports normal `py` options on the same line (`-x`, `-v`, `-r`, etc.).
- `END_PYTHON` terminator must be exact and zero-indented.

## Structured Exceptions (`-x`)

When `-x` is used and Python raises, `brush` sets:

- `MCBASH_EXCEPTION`
- `MCBASH_EXCEPTION_MSG`
- `MCBASH_EXCEPTION_TB` (array)
- `MCBASH_EXCEPTION_LANG` (`python`)

## `bash` bridge object

The embedded runtime injects `bash` with the following implemented surface.

Constants:

- `bash.PIPE`
- `bash.STDOUT`
- `bash.DEVNULL`

### `bash.vars`

Live mapping over shell variables.

- `bash.vars[name]`
- `bash.vars[name] = value`
- `del bash.vars[name]`
- `name in bash.vars`
- `for name in bash.vars`
- `len(bash.vars)`
- `bash.vars.attrs(name)`
- `bash.vars.set_attrs(name, **flags)`
- `bash.vars.declare(name, value="", **flags)`

Value mapping:

- scalar Python values -> scalar shell vars
- `list` / `tuple` -> indexed arrays
- `dict` -> associative arrays

Supported attribute flags:

- `exported`, `integer`, `readonly`, `uppercase`, `lowercase`, `nameref`, `trace`

### `bash.env`

Live mapping over exported shell environment.

- `bash.env[name]`
- `bash.env[name] = value` (sets + exports)
- `del bash.env[name]`
- `name in bash.env`
- `for name in bash.env`
- `len(bash.env)`

### `bash.shared`

Live mapping over shared variable storage.

- `bash.shared[name]`
- `bash.shared[name] = value`
- `del bash.shared[name]`
- `name in bash.shared`
- `for name in bash.shared`
- `len(bash.shared)`

### `bash.fn`

Function map and callable namespace.

- `bash.fn[name]` -> function body
- `bash.fn[name] = "..."` -> set shell function body
- `bash.fn[name] = callable` -> generate wrapper via `py ...`
- `del bash.fn[name]`
- `name in bash.fn`
- `for name in bash.fn`
- attribute call form: `bash.fn.some_func(arg1, arg2)`

### `bash()` convenience call

Runs a shell command and returns stdout (trailing newline stripped) on success.
Raises on non-zero.

Error contract:

- non-zero command status raises `BashCalledProcessError`
- attributes: `.returncode`, `.cmd`, `.stdout`, `.stderr`

### `bash.run(...)`

Subprocess-style call returning a completed object with:

- `.args`
- `.returncode`
- `.stdout`
- `.stderr`

Supports implemented options:

- `capture_output`
- `stdout` / `stderr` (`bash.PIPE`, `bash.STDOUT`, `bash.DEVNULL`)
- `check`
- `input`
- `shell`
- `cwd`
- `env`

Current limitation:

- `timeout` is not implemented yet.

Error contract:

- `check=False` (default): always returns a completed object
- `check=True`: non-zero command status raises `BashCalledProcessError`
- unsupported options (including current `timeout`) raise a Python exception from argument validation/runtime checks

### `bash.popen(...)`

Long-running process handle with:

- `.args`
- `.stdin` / `.stdout` / `.stderr` (pipe markers when configured)
- `.returncode`
- `.poll()`
- `.wait()`
- `.communicate(input=None)`
- context-manager support

Current caveats:

- designed for subprocess-style execution semantics first; full parity with Python `subprocess.Popen` stream behavior is still in progress
- very large streaming workloads should be validated carefully in your workload before relying on strict CPython parity

### `bash.tie(...)` and `bash.untie(...)`

Tie shell variable access to Python getter/setter callbacks:

- `bash.tie(name, getter, setter=None, type=None)`
- `bash.untie(name)`

Supported tie `type` values:

- `scalar`
- `integer`
- `array`
- `assoc`

### `bash.stack`

Read-only stack view of shell frames.
Each frame provides:

- `source`
- `lineno`
- `funcname`

## Shell-side ties (`py -t` / `py -u`)

Shell-initiated tie control:

- `py -t varname` ties shell reads/writes for `varname` to Python-side value/update flow.
- `py -u varname` removes the tie.

The two directions are complementary:

- shell-initiated: `py -t` / `py -u`
- python-initiated: `bash.tie(...)` / `bash.untie(...)`

## `shared` shell builtin (related)

The shell `shared` builtin exposes shared backing directly from shell scripts:

- scalar: `shared name=value`
- indexed array: `shared -a name`
- associative array: `shared -A name`
- integer: `shared -i name=0`
- delete: `shared -d name`

`bash.shared` accesses the same shared backend from Python.

Concurrency notes:

- shared storage is process-visible across shell forks/subshells
- updates are serialized through shared-backend locking
- visibility is immediate after writer completion
- there is no multi-key transaction API; treat each assignment as an independent update

## Exit and return code semantics

### `py` and `PYTHON` command status

- `0` success
- `1` Python exception
- `130` interruption/cancellation

With structured exceptions (`-x`), traceback output is replaced by `MCBASH_EXCEPTION*` variable population.

### `bash()` behavior in Python

- On success: returns stdout text with trailing newline stripped.
- On non-zero: raises an exception-like runtime error carrying command outcome metadata.

### `bash.run(...)` behavior in Python

- Always returns completed object unless:
  - `check=True` and command returns non-zero (raises), or
  - unsupported `timeout` is provided (raises).
- Command exit code is available as `completed.returncode`.

### `bash.popen(...)` behavior in Python

- `poll()` returns `None` while running, or integer exit code when finished.
- `wait()` returns integer exit code.
- `communicate()` returns `(stdout, stderr)` and sets `.returncode`.

### Shell pipeline/command-substitution impact

`PYTHON` is a normal shell command for status propagation purposes.
Its status participates in shell pipeline/list semantics exactly like other commands.
