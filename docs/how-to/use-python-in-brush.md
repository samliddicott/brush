# How to use Python in brush

## 1. Build with Python enabled

```bash
cargo build -p brush-shell --features python-pyo3
```

## 2. Run simple Python code

```bash
py 'print("hello from python")'
```

## 3. Evaluate an expression

```bash
py -e '6 * 7'
```

## 4. Capture output and return values

```bash
py 'def process(n):\n    print(f"processing {n}")\n    return int(n) * 2'
py -v logs -r result process 21
echo "$logs"
echo "$result"
```

## 5. Use structured exceptions

```bash
py -x 'raise ValueError("bad")'
echo "$MCBASH_EXCEPTION"
echo "$MCBASH_EXCEPTION_MSG"
```

## 6. Use native `PYTHON` blocks

```bash
PYTHON
x = 40
print(x + 2)
END_PYTHON
```

With options:

```bash
PYTHON -x
raise ValueError("bad")
END_PYTHON
```

With redirection:

```bash
PYTHON 2>err.log
print("ok")
END_PYTHON
```

In a pipeline:

```bash
echo ignored | PYTHON | cat
print("ok")
END_PYTHON
```

## 7. Disable block dedent when needed

```bash
PYTHON --no-dedent
print("literal indentation mode")
END_PYTHON
```
