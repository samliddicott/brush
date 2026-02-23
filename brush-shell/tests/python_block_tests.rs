//! Integration tests for native `PYTHON ... END_PYTHON` blocks.

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
fn python_block_executes() -> anyhow::Result<()> {
    let output = run_script("PYTHON\nprint(42)\nEND_PYTHON")?;
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(String::from_utf8(output.stdout)?, "42\n");
    Ok(())
}

#[test]
fn python_block_dedents_by_default() -> anyhow::Result<()> {
    let script = "if true; then\nPYTHON\n    x = 40\n    print(x + 2)\nEND_PYTHON\nfi";
    let output = run_script(script)?;
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(String::from_utf8(output.stdout)?, "42\n");
    Ok(())
}

#[test]
fn python_block_structured_exception_option() -> anyhow::Result<()> {
    let script = "PYTHON -x\nraise ValueError('bad')\nEND_PYTHON\nprintf '%s|%s' \"$MCBASH_EXCEPTION\" \"$MCBASH_EXCEPTION_MSG\"";
    let output = run_script(script)?;
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(String::from_utf8(output.stdout)?, "ValueError|bad");
    Ok(())
}

#[test]
fn pipeline_into_python_block_parses_and_runs() -> anyhow::Result<()> {
    let script = "echo ignored | PYTHON\nprint('ok')\nEND_PYTHON";
    let output = run_script(script)?;
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(String::from_utf8(output.stdout)?, "ok\n");
    Ok(())
}

#[test]
fn python_block_in_middle_of_pipeline_runs() -> anyhow::Result<()> {
    let script = "echo ignored | PYTHON | cat\nprint('ok')\nEND_PYTHON";
    let output = run_script(script)?;
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(String::from_utf8(output.stdout)?, "ok\n");
    Ok(())
}
