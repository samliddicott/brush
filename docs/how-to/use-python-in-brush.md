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

## 8. Tie variables from shell side (`py -t` / `py -u`)

```bash
py 'x = 42'
py -t x
echo "$x"
x=100
py -e 'x'
py -u x
```

## 9. Tie variables from Python side (`bash.tie`)

```bash
py '
counter = {"v": 0}
def getv():
    return counter["v"]
def setv(v):
    counter["v"] = int(v)
bash.tie("COUNT", getv, setv, type="integer")
'
echo "$COUNT"
COUNT=7
py -e 'counter["v"]'
py 'bash.untie("COUNT")'
```

## 10. Use shared variables from shell and Python

From shell:

```bash
shared total=1
echo "$total"
```

From Python:

```bash
py 'bash.shared["total"] = 99'
echo "$total"
```

## 11. Check status and return codes

Shell command status:

```bash
py 'print("ok")'
echo "$?"   # 0

py 'raise RuntimeError("bad")'
echo "$?"   # 1
```

Python callback status handling:

```bash
py 'r = bash.run("sh", "-c", "exit 7"); print(r.returncode)'
py 'bash.run("sh", "-c", "exit 7", check=True)'  # raises in Python
```
