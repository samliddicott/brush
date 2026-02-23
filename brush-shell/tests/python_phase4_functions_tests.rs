//! Phase 4 integration tests for Python<->shell function bridge.

#![cfg(all(unix, feature = "python-pyo3"))]
#![cfg(test)]
#![allow(clippy::panic_in_result_fn)]

use anyhow::Context;

fn brush_bin() -> std::path::PathBuf {
    let p = assert_cmd::cargo::cargo_bin!("brush");
    if std::fs::metadata(&p).is_ok_and(|m| m.len() > 0) {
        return p.to_path_buf();
    }

    let deps_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../target/debug/deps");
    if let Ok(entries) = std::fs::read_dir(&deps_dir) {
        let mut best: Option<(std::time::SystemTime, std::path::PathBuf)> = None;
        for entry in entries.flatten() {
            let path = entry.path();
            let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            if !name.starts_with("brush-") || name.ends_with(".d") {
                continue;
            }
            if let Ok(meta) = std::fs::metadata(&path)
                && meta.len() > 0
                && meta.is_file()
            {
                let mtime = meta.modified().unwrap_or(std::time::SystemTime::UNIX_EPOCH);
                if best.as_ref().is_none_or(|(t, _)| mtime > *t) {
                    best = Some((mtime, path));
                }
            }
        }
        if let Some((_, path)) = best {
            return path;
        }
    }

    p.to_path_buf()
}

fn run_script(script: &str) -> anyhow::Result<std::process::Output> {
    let shell_path = brush_bin();
    let output = std::process::Command::new(shell_path)
        .arg("--norc")
        .arg("--noprofile")
        .arg("--no-config")
        .arg("--input-backend=basic")
        .arg("--disable-bracketed-paste")
        .arg("--disable-color")
        .arg("-c")
        .arg(script)
        .output()
        .with_context(|| format!("failed to run script: {script}"))?;
    Ok(output)
}

#[test]
fn bash_fn_contains_and_iterates() -> anyhow::Result<()> {
    let output = run_script("demo_fn() { :; }; py \"print('demo_fn' in bash.fn); print('demo_fn' in list(bash.fn))\"")?;
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(String::from_utf8(output.stdout)?, "True\nTrue\n");
    Ok(())
}

#[test]
fn bash_fn_getitem_returns_function_body() -> anyhow::Result<()> {
    let output = run_script("demo_body() { echo hi; }; py \"print('echo hi' in bash.fn['demo_body'])\"")?;
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(String::from_utf8(output.stdout)?, "True\n");
    Ok(())
}

#[test]
fn bash_fn_attribute_call_invokes_shell_function() -> anyhow::Result<()> {
    let output = run_script("greet_fn() { echo hello:$1; }; py \"print(bash.fn.greet_fn('sam'))\"")?;
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(String::from_utf8(output.stdout)?, "hello:sam\n");
    Ok(())
}

#[test]
fn bash_fn_missing_attribute_raises_attribute_error() -> anyhow::Result<()> {
    let output = run_script("py \"\ntry:\n    bash.fn.no_such_fn()\nexcept Exception as e:\n    print(type(e).__name__)\n\"")?;
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(String::from_utf8(output.stdout)?, "AttributeError\n");
    Ok(())
}

#[test]
fn bash_fn_setitem_string_defines_shell_function() -> anyhow::Result<()> {
    let output = run_script("py \"bash.fn['mk_from_py'] = 'echo made'\"; mk_from_py ok")?;
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(String::from_utf8(output.stdout)?, "made\n");
    Ok(())
}

#[test]
fn bash_fn_setitem_callable_defines_shell_wrapper() -> anyhow::Result<()> {
    let output = run_script("py \"bash.fn['py_add'] = lambda a, b: int(a) + int(b)\"; py_add 20 22")?;
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(String::from_utf8(output.stdout)?, "42\n");
    Ok(())
}

#[test]
fn bash_fn_delitem_removes_shell_function() -> anyhow::Result<()> {
    let output = run_script(
        "py \"bash.fn['tmp_drop'] = 'echo alive'; del bash.fn['tmp_drop']; print('tmp_drop' in bash.fn)\"",
    )?;
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(String::from_utf8(output.stdout)?, "False\n");
    Ok(())
}
