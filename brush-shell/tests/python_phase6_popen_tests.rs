//! Phase 6 integration tests for `bash.popen` and `bash.run` pipe features.

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
fn bash_run_capture_output_and_stderr_pipe() -> anyhow::Result<()> {
    let output = run_script(
        "py \"r = bash.run('sh', '-c', 'echo out; echo err >&2', capture_output=True); print(r.stdout.strip()); print(r.stderr.strip())\"",
    )?;
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(String::from_utf8(output.stdout)?, "out\nerr\n");
    Ok(())
}

#[test]
fn bash_run_input_pipe() -> anyhow::Result<()> {
    let output = run_script(
        "py \"r = bash.run('cat', capture_output=True, input='abc'); print(r.stdout)\"",
    )?;
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(String::from_utf8(output.stdout)?, "abc\n");
    Ok(())
}

#[test]
fn bash_popen_wait_and_communicate() -> anyhow::Result<()> {
    let output = run_script(
        "py \"p = bash.popen('cat', stdin=bash.PIPE, stdout=bash.PIPE); out, err = p.communicate(input='xyz'); print(out); print(err); print(p.returncode)\"",
    )?;
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(String::from_utf8(output.stdout)?, "xyz\n\n0\n");
    Ok(())
}
