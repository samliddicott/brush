//! Phase 3 integration tests for Python->bash callback APIs.

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
fn bash_callable_returns_stdout_string() -> anyhow::Result<()> {
    let output = run_script("py \"print(bash('echo', 'hello'))\"")?;
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(String::from_utf8(output.stdout)?, "hello\n");
    Ok(())
}

#[test]
fn bash_run_capture_output_completed_shape() -> anyhow::Result<()> {
    let output = run_script(
        "py \"r = bash.run('echo', 'phase3', capture_output=True); print(r.returncode); print(r.stdout.strip()); print(r.stderr)\"",
    )?;
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(String::from_utf8(output.stdout)?, "0\nphase3\n\n");
    Ok(())
}

#[test]
fn bash_run_check_raises_on_nonzero() -> anyhow::Result<()> {
    let output = run_script(
        "py -x \"bash.run('false', check=True)\"; printf '%s|%s' \"$MCBASH_EXCEPTION\" \"$MCBASH_EXCEPTION_LANG\"",
    )?;
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(String::from_utf8(output.stdout)?, "RuntimeError|python");
    Ok(())
}

#[test]
fn bash_run_shell_true_uses_command_string() -> anyhow::Result<()> {
    let output = run_script(
        "py \"r = bash.run('echo shell_mode', shell=True, capture_output=True); print(r.stdout.strip())\"",
    )?;
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(String::from_utf8(output.stdout)?, "shell_mode\n");
    Ok(())
}
