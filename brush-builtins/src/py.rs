use std::io::{Read, Write};

use brush_core::{ExecutionExitCode, ExecutionResult, builtins};
use clap::Parser;

/// Execute Python code in the shell-embedded Python runtime.
///
/// Callable dispatch is attempted first; if it does not apply, the input is
/// executed as Python statements.
#[derive(Parser)]
pub(crate) struct PyCommand {
    /// Python tokens, or `-N` / `-N-` to read script text from file descriptor `N`.
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    args: Vec<String>,
}

impl builtins::Command for PyCommand {
    type Error = brush_core::Error;

    async fn execute<SE: brush_core::ShellExtensions>(
        &self,
        mut context: brush_core::ExecutionContext<'_, SE>,
    ) -> Result<ExecutionResult, Self::Error> {
        let Some(first) = self.args.first() else {
            writeln!(context.stderr(), "py: expected arguments")?;
            return Ok(ExecutionExitCode::InvalidUsage.into());
        };

        if let Some((fd, close_after)) = parse_fd_spec(first) {
            if self.args.len() != 1 {
                writeln!(
                    context.stderr(),
                    "py: fd mode expects exactly one argument (e.g. -5 or -5-)"
                )?;
                return Ok(ExecutionExitCode::InvalidUsage.into());
            }

            let Some(mut fd_file) = context.try_fd(fd) else {
                writeln!(context.stderr(), "py: bad file descriptor: {fd}")?;
                return Ok(ExecutionExitCode::GeneralError.into());
            };

            let mut script = String::new();
            fd_file.read_to_string(&mut script)?;

            if close_after {
                context.params.remove_fd(fd);
            }

            let dedented = dedent_text(&script);
            return brush_core::python::exec_code(context.shell, &context.params, &dedented);
        }

        brush_core::python::exec_unified(context.shell, &context.params, &self.args)
    }
}

fn parse_fd_spec(s: &str) -> Option<(i32, bool)> {
    if !s.starts_with('-') || s == "-" {
        return None;
    }

    let (digits, close_after) = if let Some(stripped) = s.strip_suffix('-') {
        (&stripped[1..], true)
    } else {
        (&s[1..], false)
    };

    if digits.is_empty() || !digits.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }

    let fd = digits.parse::<i32>().ok()?;
    Some((fd, close_after))
}

fn dedent_text(input: &str) -> String {
    let lines = input.lines().collect::<Vec<_>>();
    let min_indent = lines
        .iter()
        .filter_map(|line| {
            if line.trim().is_empty() {
                None
            } else {
                Some(line.chars().take_while(|c| *c == ' ' || *c == '\t').count())
            }
        })
        .min()
        .unwrap_or(0);

    let mut out = String::with_capacity(input.len());
    for (i, line) in lines.iter().enumerate() {
        let mut to_strip = min_indent;
        let mut split_at = 0usize;
        for (byte_idx, ch) in line.char_indices() {
            if to_strip == 0 {
                break;
            }
            if ch == ' ' || ch == '\t' {
                to_strip -= 1;
                split_at = byte_idx + ch.len_utf8();
            } else {
                break;
            }
        }

        out.push_str(&line[split_at..]);
        if i + 1 < lines.len() || input.ends_with('\n') {
            out.push('\n');
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{dedent_text, parse_fd_spec};

    #[test]
    fn parses_fd_specs() {
        assert_eq!(parse_fd_spec("-5"), Some((5, false)));
        assert_eq!(parse_fd_spec("-5-"), Some((5, true)));
        assert_eq!(parse_fd_spec("-0012-"), Some((12, true)));
        assert_eq!(parse_fd_spec("5"), None);
        assert_eq!(parse_fd_spec("-"), None);
        assert_eq!(parse_fd_spec("-x"), None);
    }

    #[test]
    fn dedents_common_indent() {
        let input = "    a\n      b\n    c\n";
        let output = dedent_text(input);
        assert_eq!(output, "a\n  b\nc\n");
    }
}
