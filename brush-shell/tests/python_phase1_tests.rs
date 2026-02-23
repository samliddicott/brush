//! Phase 1 integration tests for `py` builtin options.
//!
//! These tests run only when `python-pyo3` is enabled.

#![cfg(all(unix, feature = "python-pyo3"))]
#![cfg(test)]
#![allow(clippy::panic_in_result_fn)]

use anyhow::Context;

fn run_script(script: &str) -> anyhow::Result<std::process::Output> {
    let shell_path = assert_cmd::cargo::cargo_bin!("brush");
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
fn py_expression_mode_prints_repr() -> anyhow::Result<()> {
    let output = run_script("py -e '1 + 2'")?;
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(String::from_utf8(output.stdout)?, "3\n");
    assert_eq!(String::from_utf8(output.stderr)?, "");
    Ok(())
}

#[test]
fn py_capture_stdout_and_return_value() -> anyhow::Result<()> {
    let output = run_script(
        "py 'def process(n):\n    print(f\"processing {n} items...\")\n    return int(n) * 2'; py -v logs -r result process 21; printf '%s|%s' \"$logs\" \"$result\"",
    )?;

    assert_eq!(output.status.code(), Some(0));
    assert_eq!(
        String::from_utf8(output.stdout)?,
        "processing 21 items...\n|42"
    );
    assert_eq!(String::from_utf8(output.stderr)?, "");
    Ok(())
}

#[test]
fn py_structured_exception_sets_shell_vars() -> anyhow::Result<()> {
    let output = run_script(
        "unset MCBASH_EXCEPTION MCBASH_EXCEPTION_MSG MCBASH_EXCEPTION_LANG MCBASH_EXCEPTION_TB; py -x 'raise ValueError(\"bad\")'; rc=$?; printf '%s|%s|%s|%s|%s' \"$rc\" \"$MCBASH_EXCEPTION\" \"$MCBASH_EXCEPTION_MSG\" \"$MCBASH_EXCEPTION_LANG\" \"${MCBASH_EXCEPTION_TB[0]}\"",
    )?;

    assert_eq!(output.status.code(), Some(0));

    let stdout = String::from_utf8(output.stdout)?;
    assert!(stdout.starts_with("1|ValueError|bad|python|"));
    assert!(stdout.contains("Traceback"));

    let stderr = String::from_utf8(output.stderr)?;
    assert!(!stderr.contains("Traceback"));

    Ok(())
}

#[test]
fn py_result_capture_suppresses_terminal_output() -> anyhow::Result<()> {
    let output = run_script("py -r out -e '6 * 7'; printf '%s' \"$out\"")?;

    assert_eq!(output.status.code(), Some(0));
    assert_eq!(String::from_utf8(output.stdout)?, "42");
    assert_eq!(String::from_utf8(output.stderr)?, "");

    Ok(())
}
