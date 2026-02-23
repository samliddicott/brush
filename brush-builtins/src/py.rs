use std::io::{Read, Write};

use brush_core::{ExecutionExitCode, ExecutionResult, ShellVariable, builtins};
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
        let options = match parse_options(&self.args) {
            Ok(o) => o,
            Err(()) => {
                writeln!(context.stderr(), "py: invalid option usage")?;
                return Ok(ExecutionExitCode::InvalidUsage.into());
            }
        };
        let remaining = options.remaining;

        if let Some(name) = &options.tie_var {
            if !remaining.is_empty() {
                writeln!(context.stderr(), "py: -t expects only a variable name")?;
                return Ok(ExecutionExitCode::InvalidUsage.into());
            }

            let q = python_quote_single(name);
            let code = format!(
                "bash.tie({q}, lambda: globals().get({q}, ''), lambda v: globals().__setitem__({q}, v))"
            );
            let outcome = brush_core::python::exec_code_with_options(
                context.shell,
                &code,
                brush_core::python::PyExecOptions::default(),
            )?;
            return finalize_outcome(&mut context, outcome, &options);
        }

        if let Some(name) = &options.untie_var {
            if !remaining.is_empty() {
                writeln!(context.stderr(), "py: -u expects only a variable name")?;
                return Ok(ExecutionExitCode::InvalidUsage.into());
            }

            let code = format!("bash.untie({})", python_quote_single(name));
            let outcome = brush_core::python::exec_code_with_options(
                context.shell,
                &code,
                brush_core::python::PyExecOptions::default(),
            )?;
            return finalize_outcome(&mut context, outcome, &options);
        }

        let Some(first) = remaining.first() else {
            writeln!(context.stderr(), "py: expected arguments")?;
            return Ok(ExecutionExitCode::InvalidUsage.into());
        };

        if let Some((fd, close_after)) = parse_fd_spec(first) {
            if remaining.len() != 1 {
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
            let py_options = brush_core::python::PyExecOptions {
                expression_mode: options.expression_mode,
                structured_exceptions: options.structured_exceptions,
            };
            let outcome =
                brush_core::python::exec_code_with_options(context.shell, &dedented, py_options)?;
            return finalize_outcome(&mut context, outcome, &options);
        }

        let py_options = brush_core::python::PyExecOptions {
            expression_mode: options.expression_mode,
            structured_exceptions: options.structured_exceptions,
        };
        let outcome = brush_core::python::exec_unified_with_options(
            context.shell,
            &context.params,
            remaining,
            py_options,
        )?;
        finalize_outcome(&mut context, outcome, &options)
    }
}

struct ParsedOptions<'a> {
    expression_mode: bool,
    structured_exceptions: bool,
    capture_stdout_var: Option<String>,
    capture_result_var: Option<String>,
    tie_var: Option<String>,
    untie_var: Option<String>,
    remaining: &'a [String],
}

fn parse_options(args: &[String]) -> Result<ParsedOptions<'_>, ()> {
    let mut expression_mode = false;
    let mut structured_exceptions = false;
    let mut capture_stdout_var: Option<String> = None;
    let mut capture_result_var: Option<String> = None;
    let mut tie_var: Option<String> = None;
    let mut untie_var: Option<String> = None;

    let mut i = 0usize;
    while i < args.len() {
        let arg = &args[i];

        if arg == "--" {
            i += 1;
            break;
        }

        match arg.as_str() {
            "-e" => {
                expression_mode = true;
                i += 1;
            }
            "-x" => {
                structured_exceptions = true;
                i += 1;
            }
            "-v" => {
                let Some(var) = args.get(i + 1) else {
                    return Err(());
                };
                capture_stdout_var = Some(var.clone());
                i += 2;
            }
            "-r" => {
                let Some(var) = args.get(i + 1) else {
                    return Err(());
                };
                capture_result_var = Some(var.clone());
                i += 2;
            }
            "-t" => {
                let Some(var) = args.get(i + 1) else {
                    return Err(());
                };
                tie_var = Some(var.clone());
                i += 2;
            }
            "-u" => {
                let Some(var) = args.get(i + 1) else {
                    return Err(());
                };
                untie_var = Some(var.clone());
                i += 2;
            }
            _ if arg.starts_with('-') => break,
            _ => break,
        }
    }

    if tie_var.is_some() && untie_var.is_some() {
        return Err(());
    }

    Ok(ParsedOptions {
        expression_mode,
        structured_exceptions,
        capture_stdout_var,
        capture_result_var,
        tie_var,
        untie_var,
        remaining: &args[i..],
    })
}

fn python_quote_single(s: &str) -> String {
    format!("'{}'", s.replace('\\', "\\\\").replace('\'', "\\'"))
}

fn finalize_outcome<SE: brush_core::ShellExtensions>(
    context: &mut brush_core::ExecutionContext<'_, SE>,
    outcome: brush_core::python::PyExecOutcome,
    options: &ParsedOptions<'_>,
) -> Result<ExecutionResult, brush_core::Error> {
    if let Some(var) = &options.capture_stdout_var {
        context
            .shell
            .env_mut()
            .set_global(var, ShellVariable::new(outcome.stdout.clone()))?;
    } else if !outcome.stdout.is_empty() {
        write!(context.stdout(), "{}", outcome.stdout)?;
    }

    if let Some(var) = &options.capture_result_var {
        if let Some(value) = &outcome.value {
            context
                .shell
                .env_mut()
                .set_global(var, ShellVariable::new(value.clone()))?;
        } else {
            context
                .shell
                .env_mut()
                .set_global(var, ShellVariable::new(String::new()))?;
        }
    } else if let Some(value) = &outcome.value {
        writeln!(context.stdout(), "{value}")?;
    }

    if options.structured_exceptions
        && let Some(exc) = &outcome.exception
    {
        brush_core::python::apply_structured_exception(context.shell, exc)?;
    }

    Ok(outcome.result)
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
    use super::{dedent_text, parse_fd_spec, parse_options};

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

    #[test]
    fn parses_options_and_rest() {
        let args = vec![
            "-x".to_string(),
            "-e".to_string(),
            "-v".to_string(),
            "OUT".to_string(),
            "-r".to_string(),
            "RET".to_string(),
            "expr".to_string(),
        ];
        let parsed = parse_options(&args).expect("options should parse");
        assert!(parsed.expression_mode);
        assert!(parsed.structured_exceptions);
        assert_eq!(parsed.capture_stdout_var.as_deref(), Some("OUT"));
        assert_eq!(parsed.capture_result_var.as_deref(), Some("RET"));
        assert_eq!(parsed.tie_var, None);
        assert_eq!(parsed.untie_var, None);
        assert_eq!(parsed.remaining, ["expr"]);
    }

    #[test]
    fn parses_tie_and_untie_options() {
        let t = vec!["-t".to_string(), "x".to_string()];
        let parsed_t = parse_options(&t).expect("tie options should parse");
        assert_eq!(parsed_t.tie_var.as_deref(), Some("x"));
        assert_eq!(parsed_t.untie_var, None);
        assert!(parsed_t.remaining.is_empty());

        let u = vec!["-u".to_string(), "x".to_string()];
        let parsed_u = parse_options(&u).expect("untie options should parse");
        assert_eq!(parsed_u.tie_var, None);
        assert_eq!(parsed_u.untie_var.as_deref(), Some("x"));
        assert!(parsed_u.remaining.is_empty());
    }
}
