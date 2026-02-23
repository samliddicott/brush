//! Phase 5 integration tests for Python<->shell variable ties.

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
fn bash_tie_getter_and_setter_roundtrip() -> anyhow::Result<()> {
    let output = run_script(
        "py \"x=42; bash.tie('x', lambda: x, lambda v: globals().__setitem__('x', int(v))); print(bash.vars['x']); bash.vars['x']=100; print(x)\"",
    )?;
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(String::from_utf8(output.stdout)?, "42\n100\n");
    Ok(())
}

#[test]
fn bash_tie_type_assoc_is_coerced() -> anyhow::Result<()> {
    let output = run_script(
        "py \"cfg={'host':'localhost'}; bash.tie('CFG', lambda: cfg, type='assoc'); print(bash.vars['CFG']['host'])\"",
    )?;
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(String::from_utf8(output.stdout)?, "localhost\n");
    Ok(())
}

#[test]
fn bash_untie_stops_setter_updates() -> anyhow::Result<()> {
    let output = run_script(
        "py \"x=7; bash.tie('x', lambda: x, lambda v: globals().__setitem__('x', int(v))); bash.vars['x']=9; bash.untie('x'); bash.vars['x']=123; print(x)\"",
    )?;
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(String::from_utf8(output.stdout)?, "9\n");
    Ok(())
}

#[test]
fn py_t_and_u_flags_register_and_remove_tie() -> anyhow::Result<()> {
    let output = run_script(
        "py \"x=10\"; py -t x; py \"print(bash.vars['x']); bash.vars['x']=31; print(x)\"; py -u x; py \"bash.vars['x']=99; print(x)\"",
    )?;
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(String::from_utf8(output.stdout)?, "10\n31\n31\n");
    Ok(())
}

#[test]
fn tied_var_expands_live_in_shell_parameter_expansion() -> anyhow::Result<()> {
    let output = run_script(
        "py \"x=5; bash.tie('x', lambda: x, lambda v: globals().__setitem__('x', int(v)))\"; echo \"$x\"; py \"x=8\"; echo \"$x\"",
    )?;
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(String::from_utf8(output.stdout)?, "5\n8\n");
    Ok(())
}

#[test]
fn shell_assignment_pushes_back_to_python_tie() -> anyhow::Result<()> {
    let output = run_script(
        "py \"x=1; bash.tie('x', lambda: x, lambda v: globals().__setitem__('x', int(v)))\"; x=44; py \"print(x)\"",
    )?;
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(String::from_utf8(output.stdout)?, "44\n");
    Ok(())
}
