//! Python runtime context and execution helpers.

use std::io::Write;

use pyo3::prelude::*;
use pyo3::types::{PyAny, PyDict, PyModule, PyTuple};

use crate::{ExecutionExitCode, ExecutionParameters, ExecutionResult, error, extensions};

/// Python configuration and namespace state attached to a shell context.
pub struct PythonContext {
    globals: Option<Py<PyDict>>,
    /// Enables command-not-found fallback for dotted names.
    pub implicit_dotted_dispatch: bool,
    /// Enables shell-argument auto-conversion for callable dispatch.
    pub auto_convert_args: bool,
}

impl Clone for PythonContext {
    fn clone(&self) -> Self {
        self.snapshot_for_isolated()
    }
}

impl Default for PythonContext {
    fn default() -> Self {
        Self {
            globals: None,
            implicit_dotted_dispatch: false,
            auto_convert_args: true,
        }
    }
}

impl PythonContext {
    /// Create context defaults for interactive or non-interactive mode.
    pub fn defaults_for_interactive(interactive: bool) -> Self {
        Self {
            globals: None,
            implicit_dotted_dispatch: interactive,
            auto_convert_args: true,
        }
    }

    /// Returns a shallow-copied context suitable for isolated execution.
    pub fn snapshot_for_isolated(&self) -> Self {
        Python::with_gil(|py| {
            let globals = self.globals.as_ref().map(|g| {
                let src = g.bind(py);
                let copied = PyDict::new(py);
                let _ = copied.update(src.as_mapping());
                copied.unbind()
            });

            Self {
                globals,
                implicit_dotted_dispatch: self.implicit_dotted_dispatch,
                auto_convert_args: self.auto_convert_args,
            }
        })
    }

    fn ensure_globals(&mut self, py: Python<'_>) -> Py<PyDict> {
        if let Some(existing) = &self.globals {
            existing.clone_ref(py)
        } else {
            let d = PyDict::new(py).unbind();
            self.globals = Some(d.clone_ref(py));
            d
        }
    }
}

/// Executes Python in callable-first mode, with statement fallback.
pub fn exec_unified<SE: extensions::ShellExtensions>(
    shell: &mut crate::Shell<SE>,
    params: &ExecutionParameters,
    tokens: &[String],
) -> Result<ExecutionResult, error::Error> {
    if let Some(result) = try_callable(shell, params, tokens)? {
        return Ok(result);
    }

    let code = tokens.join(" ");
    exec_code(shell, params, &code)
}

/// Tries to treat the token sequence as a Python callable invocation.
///
/// Returns `Ok(Some(result))` if callable dispatch was attempted.
/// Returns `Ok(None)` when this does not look like a callable dispatch.
pub fn try_callable<SE: extensions::ShellExtensions>(
    shell: &mut crate::Shell<SE>,
    params: &ExecutionParameters,
    tokens: &[String],
) -> Result<Option<ExecutionResult>, error::Error> {
    if tokens.is_empty() {
        return Ok(Some(ExecutionResult::success()));
    }

    let first = &tokens[0];
    if !looks_callable(tokens) {
        return Ok(None);
    }

    if is_python_keyword(first) {
        return Ok(None);
    }

    if shell.cancel().is_cancelled() {
        return Ok(Some(ExecutionExitCode::Interrupted.into()));
    }

    let attempted = Python::with_gil(|py| -> Result<Option<ExecutionResult>, error::Error> {
        let globals = shell.python_mut().ensure_globals(py);
        let globals = globals.bind(py);

        let Some(callable) = resolve_callable(py, globals, first)? else {
            return Ok(None);
        };

        if !callable.is_callable() {
            return Ok(None);
        }

        let py_args = tokens[1..]
            .iter()
            .map(|arg| {
                if shell.python().auto_convert_args {
                    auto_convert(py, arg)
                } else {
                    Ok(arg
                        .into_pyobject(py)
                        .unwrap_or_else(|never| match never {})
                        .unbind()
                        .into())
                }
            })
            .collect::<Result<Vec<PyObject>, error::Error>>()?;

        let args = PyTuple::new(py, py_args)
            .map_err(|e| error::ErrorKind::InternalError(e.to_string()))?;

        match callable.call1(args) {
            Ok(result_obj) => {
                if !result_obj.is_none() {
                    let rendered = result_obj
                        .str()
                        .map_err(|e| error::ErrorKind::InternalError(e.to_string()))?;
                    writeln!(params.stdout(shell), "{}", rendered).map_err(error::Error::from)?;
                }
                Ok(Some(ExecutionResult::success()))
            }
            Err(py_err) => {
                py_err.print(py);
                Ok(Some(ExecutionExitCode::GeneralError.into()))
            }
        }
    })?;

    if shell.cancel().is_cancelled() {
        return Ok(Some(ExecutionExitCode::Interrupted.into()));
    }

    Ok(attempted)
}

/// Executes raw Python code against the current shell Python namespace.
pub fn exec_code<SE: extensions::ShellExtensions>(
    shell: &mut crate::Shell<SE>,
    _params: &ExecutionParameters,
    code: &str,
) -> Result<ExecutionResult, error::Error> {
    if shell.cancel().is_cancelled() {
        return Ok(ExecutionExitCode::Interrupted.into());
    }

    let result = Python::with_gil(|py| -> Result<ExecutionResult, error::Error> {
        let globals = shell.python_mut().ensure_globals(py);
        let globals = globals.bind(py);

        let run_result = PyModule::import(py, "builtins")
            .and_then(|builtins| builtins.getattr("exec"))
            .and_then(|exec_fn| exec_fn.call1((code, globals, globals)));
        match run_result {
            Ok(_) => Ok(ExecutionResult::success()),
            Err(py_err) => {
                py_err.print(py);
                Ok(ExecutionExitCode::GeneralError.into())
            }
        }
    })?;

    if shell.cancel().is_cancelled() {
        return Ok(ExecutionExitCode::Interrupted.into());
    }

    Ok(result)
}

fn looks_callable(tokens: &[String]) -> bool {
    let Some(first) = tokens.first() else {
        return false;
    };

    if let Some(second) = tokens.get(1)
        && second == "="
    {
        return false;
    }

    if tokens.len() == 1 {
        return is_identifier(first) || is_dotted_identifier(first);
    }

    true
}

fn resolve_callable<'py>(
    py: Python<'py>,
    globals: &Bound<'py, PyDict>,
    first: &str,
) -> Result<Option<Bound<'py, PyAny>>, error::Error> {
    if is_identifier(first)
        && let Ok(item) = globals.get_item(first)
        && let Some(obj) = item
    {
        return Ok(Some(obj));
    }

    if is_dotted_identifier(first) {
        let mut parts = first.split('.').collect::<Vec<_>>();
        let importlib = PyModule::import(py, "importlib")
            .map_err(|e| error::ErrorKind::InternalError(e.to_string()))?;

        while !parts.is_empty() {
            let module_name = parts.join(".");
            if let Ok(module) = importlib.call_method1("import_module", (module_name,)) {
                let mut obj = module;
                for attr in first.split('.').skip(parts.len()) {
                    obj = obj
                        .getattr(attr)
                        .map_err(|e| error::ErrorKind::InternalError(e.to_string()))?;
                }
                return Ok(Some(obj));
            }
            parts.pop();
        }
    }

    Ok(None)
}

fn auto_convert(py: Python<'_>, arg: &str) -> Result<PyObject, error::Error> {
    if let Ok(v) = arg.parse::<i64>() {
        return Ok(v
            .into_pyobject(py)
            .unwrap_or_else(|never| match never {})
            .unbind()
            .into());
    }
    if let Ok(v) = arg.parse::<f64>() {
        return Ok(v
            .into_pyobject(py)
            .unwrap_or_else(|never| match never {})
            .unbind()
            .into());
    }

    let lowered = arg.to_ascii_lowercase();
    if lowered == "true" {
        return Ok(pyo3::types::PyBool::new(py, true).to_owned().unbind().into());
    }
    if lowered == "false" {
        return Ok(pyo3::types::PyBool::new(py, false)
            .to_owned()
            .unbind()
            .into());
    }
    if lowered == "none" || lowered == "null" {
        return Ok(py.None());
    }

    if arg.starts_with('{') || arg.starts_with('[') || arg.starts_with('"') {
        let json = PyModule::import(py, "json")
            .map_err(|e| error::ErrorKind::InternalError(e.to_string()))?;
        if let Ok(loaded) = json.call_method1("loads", (arg,)) {
            return Ok(loaded.unbind().into());
        }
    }

    Ok(arg
        .into_pyobject(py)
        .unwrap_or_else(|never| match never {})
        .unbind()
        .into())
}

fn is_identifier(s: &str) -> bool {
    let mut chars = s.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !(first == '_' || first.is_ascii_alphabetic()) {
        return false;
    }
    chars.all(|c| c == '_' || c.is_ascii_alphanumeric())
}

fn is_dotted_identifier(s: &str) -> bool {
    let mut saw_dot = false;
    for part in s.split('.') {
        if part.is_empty() || !is_identifier(part) {
            return false;
        }
        saw_dot = true;
    }
    saw_dot
}

fn is_python_keyword(s: &str) -> bool {
    matches!(
        s,
        "False"
            | "None"
            | "True"
            | "and"
            | "as"
            | "assert"
            | "async"
            | "await"
            | "break"
            | "class"
            | "continue"
            | "def"
            | "del"
            | "elif"
            | "else"
            | "except"
            | "finally"
            | "for"
            | "from"
            | "global"
            | "if"
            | "import"
            | "in"
            | "is"
            | "lambda"
            | "nonlocal"
            | "not"
            | "or"
            | "pass"
            | "raise"
            | "return"
            | "try"
            | "while"
            | "with"
            | "yield"
            | "match"
            | "case"
    )
}
