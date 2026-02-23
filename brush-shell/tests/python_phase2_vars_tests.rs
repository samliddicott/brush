//! Phase 2 integration tests for `bash.vars` / `bash.env` bridge behavior.

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
fn bash_vars_scalar_array_assoc_mapping() -> anyhow::Result<()> {
    let output = run_script(
        "py \"bash.vars['a']='x'; bash.vars['arr']=[1,2]; bash.vars['cfg']={'k':'v'}\"; printf '%s|%s|%s|%s' \"$a\" \"${arr[0]}\" \"${arr[1]}\" \"${cfg[k]}\"",
    )?;

    assert_eq!(output.status.code(), Some(0));
    assert_eq!(String::from_utf8(output.stdout)?, "x|1|2|v");
    Ok(())
}

#[test]
fn bash_env_write_exports_and_delete_unsets() -> anyhow::Result<()> {
    let output = run_script(
        "py \"bash.env['P2E']='y'\"; export -p | grep -q 'declare -x P2E=\"y\"' && echo exported; py \"del bash.env['P2E']\"; test -z \"${P2E+x}\" && echo unset",
    )?;

    assert_eq!(output.status.code(), Some(0));
    assert_eq!(String::from_utf8(output.stdout)?, "exported\nunset\n");
    Ok(())
}

#[test]
fn bash_vars_attrs_and_declare_visible_shell_side() -> anyhow::Result<()> {
    let output = run_script(
        "py \"bash.vars.declare('count', '7', integer=True, exported=True); bash.vars.set_attrs('count', trace=True)\"; declare -p count",
    )?;

    assert_eq!(output.status.code(), Some(0));
    let stdout = String::from_utf8(output.stdout)?;
    assert!(stdout.contains("declare -"));
    assert!(stdout.contains(" i") || stdout.contains("-i"));
    assert!(stdout.contains("x"));
    assert!(stdout.contains("count=\"7\""));
    Ok(())
}

#[test]
fn bash_vars_iteration_and_membership() -> anyhow::Result<()> {
    let output = run_script(
        "v=$(py -r out -e \"('HOME' in bash.vars, len(list(bash.vars)) >= 1)\"; printf '%s' \"$out\"); printf '%s' \"$v\"",
    )?;

    assert_eq!(output.status.code(), Some(0));
    assert_eq!(String::from_utf8(output.stdout)?, "(True, True)");
    Ok(())
}
