# Python bridge quickstart

This short tutorial walks through using embedded Python in `brush`.

## Prerequisite

Build brush with Python support:

```bash
cargo build -p brush-shell --features python-pyo3
```

## Step 1: run Python directly

```bash
py 'print("hello")'
```

## Step 2: expression mode

```bash
py -e '1 + 2'
```

## Step 3: capture results

```bash
py 'def double(n): return int(n) * 2'
py -r out double 21
echo "$out"
```

## Step 4: use a native `PYTHON` block

```bash
PYTHON
name = "brush"
print(f"hello from {name}")
END_PYTHON
```

## Step 5: structured exceptions

```bash
PYTHON -x
raise RuntimeError("demo")
END_PYTHON
echo "$MCBASH_EXCEPTION: $MCBASH_EXCEPTION_MSG"
```

## Step 6: shell bridge access

```bash
PYTHON
bash.vars["demo_var"] = "set from python"
END_PYTHON
echo "$demo_var"
```

Next: see [Python reference](../reference/python.md) for full command and behavior details.
