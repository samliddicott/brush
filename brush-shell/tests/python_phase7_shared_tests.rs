//! Phase 7 integration tests for shared-memory-backed shell variables.

#![cfg(all(target_os = "linux", feature = "python-pyo3"))]
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
fn shared_scalar_mutation_in_subshell_is_visible_in_parent() -> anyhow::Result<()> {
    let output = run_script("shared x=0; (x=42); echo \"$x\"")?;
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(String::from_utf8(output.stdout)?, "42\n");
    Ok(())
}

#[test]
fn shared_bind_existing_name_uses_current_scalar_value() -> anyhow::Result<()> {
    let output = run_script("y=abc; shared y; (y=def); echo \"$y\"")?;
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(String::from_utf8(output.stdout)?, "def\n");
    Ok(())
}

#[test]
fn shared_delete_unbinds_and_unsets_name() -> anyhow::Result<()> {
    let output = run_script("shared z=1; shared -d z; test -z \"${z+x}\" && echo gone")?;
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(String::from_utf8(output.stdout)?, "gone\n");
    Ok(())
}

#[test]
fn shared_indexed_array_visible_across_subshell() -> anyhow::Result<()> {
    let output = run_script(
        "shared -a arr; arr[0]=a; (arr[1]=b); printf '%s|%s' \"${arr[0]}\" \"${arr[1]}\"",
    )?;
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(String::from_utf8(output.stdout)?, "a|b");
    Ok(())
}

#[test]
fn shared_assoc_array_visible_across_subshell() -> anyhow::Result<()> {
    let output = run_script(
        "shared -A cfg; cfg[k]=v; (cfg[p]=q); printf '%s|%s' \"${cfg[k]}\" \"${cfg[p]}\"",
    )?;
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(String::from_utf8(output.stdout)?, "v|q");
    Ok(())
}

#[test]
fn shared_integer_preserves_integer_behavior() -> anyhow::Result<()> {
    let output = run_script("shared -i n=1; (n=41; n+=1); echo \"$n\"")?;
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(String::from_utf8(output.stdout)?, "42\n");
    Ok(())
}

#[test]
fn shared_concurrent_background_writes_distinct_names() -> anyhow::Result<()> {
    let output = run_script(
        r#"for i in $(seq 1 20); do eval "shared v$i=0"; done
for i in $(seq 1 20); do (eval "v$i=$i") & done
wait
ok=1
for i in $(seq 1 20); do eval "test \"\$v$i\" = \"$i\"" || ok=0; done
echo "$ok""#,
    )?;
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(String::from_utf8(output.stdout)?, "1\n");
    Ok(())
}

#[test]
fn python_bash_shared_roundtrip_scalar() -> anyhow::Result<()> {
    let output = run_script(
        "py \"bash.shared['sx']='1'\"; (sx=9); py -r out -e \"int(bash.shared['sx'])\"; printf '%s|%s' \"$sx\" \"$out\"",
    )?;
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(String::from_utf8(output.stdout)?, "9|9");
    Ok(())
}

#[test]
fn python_bash_shared_sets_array_assoc_integer() -> anyhow::Result<()> {
    let output = run_script(
        "py \"bash.shared['arr']=[1,2]; bash.shared['cfg']={'k':'v'}; bash.shared['n']=5\"; (n+=1); printf '%s|%s|%s' \"${arr[1]}\" \"${cfg[k]}\" \"$n\"",
    )?;
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(String::from_utf8(output.stdout)?, "2|v|6");
    Ok(())
}
