//! Python runtime context and execution helpers.

#[cfg(feature = "python-pyo3")]
mod imp {
    use std::collections::BTreeMap;
    use std::io::Write;

    use pyo3::prelude::*;
    use pyo3::types::{PyAny, PyBool, PyDict, PyModule, PyTuple};

    use crate::{
        ExecutionExitCode, ExecutionParameters, ExecutionResult, ShellValue, ShellVariable, error,
        extensions,
    };

    /// Python configuration and namespace state attached to a shell context.
    pub struct PythonContext {
        globals: Option<Py<PyDict>>,
        /// Enables command-not-found fallback for dotted names.
        pub implicit_dotted_dispatch: bool,
        /// Enables shell-argument auto-conversion for callable dispatch.
        pub auto_convert_args: bool,
    }

    /// Execution options for Python invocation.
    #[derive(Clone, Copy, Debug, Default)]
    pub struct PyExecOptions {
        /// Evaluate input as a Python expression and return its value.
        pub expression_mode: bool,
        /// Do not print traceback; instead populate structured exception metadata.
        pub structured_exceptions: bool,
    }

    /// Structured Python exception information for shell-side capture.
    #[derive(Clone, Debug)]
    pub struct PyExceptionInfo {
        /// Exception type name.
        pub ty: String,
        /// Exception message.
        pub msg: String,
        /// Traceback frames as formatted text lines.
        pub traceback_frames: Vec<String>,
    }

    /// Complete execution outcome used by `py` builtin option handling.
    pub struct PyExecOutcome {
        /// Shell execution result status.
        pub result: ExecutionResult,
        /// Captured Python stdout.
        pub stdout: String,
        /// String/representation value produced by callable/expression mode.
        pub value: Option<String>,
        /// Structured exception, if one occurred.
        pub exception: Option<PyExceptionInfo>,
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
        let outcome = exec_unified_with_options(shell, params, tokens, PyExecOptions::default())?;

        if !outcome.stdout.is_empty() {
            write!(params.stdout(shell), "{}", outcome.stdout)?;
        }

        if let Some(value) = &outcome.value {
            writeln!(params.stdout(shell), "{value}")?;
        }

        Ok(outcome.result)
    }

    /// Executes Python and returns a structured outcome used by option-aware callers.
    pub fn exec_unified_with_options<SE: extensions::ShellExtensions>(
        shell: &mut crate::Shell<SE>,
        _params: &ExecutionParameters,
        tokens: &[String],
        options: PyExecOptions,
    ) -> Result<PyExecOutcome, error::Error> {
        if let Some(outcome) = try_callable_with_options(shell, tokens, options)? {
            return Ok(outcome);
        }

        let code = tokens.join(" ");
        exec_code_with_options(shell, &code, options)
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
        let attempted =
            try_callable_with_options(shell, tokens, PyExecOptions::default())?.map(|outcome| {
                if !outcome.stdout.is_empty() {
                    let _ = write!(params.stdout(shell), "{}", outcome.stdout);
                }
                if let Some(value) = &outcome.value {
                    let _ = writeln!(params.stdout(shell), "{value}");
                }
                outcome.result
            });

        Ok(attempted)
    }

    /// Tries callable invocation and returns structured execution outcome.
    pub fn try_callable_with_options<SE: extensions::ShellExtensions>(
        shell: &mut crate::Shell<SE>,
        tokens: &[String],
        options: PyExecOptions,
    ) -> Result<Option<PyExecOutcome>, error::Error> {
        if tokens.is_empty() {
            return Ok(Some(PyExecOutcome {
                result: ExecutionResult::success(),
                stdout: String::new(),
                value: None,
                exception: None,
            }));
        }

        let first = &tokens[0];
        if !looks_callable(tokens) || is_python_keyword(first) {
            return Ok(None);
        }

        if shell.cancel().is_cancelled() {
            return Ok(Some(interrupted_outcome()));
        }

        let attempted = Python::with_gil(|py| -> Result<Option<PyExecOutcome>, error::Error> {
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

            let run = || -> Result<Option<String>, PyErr> {
                let result_obj = callable.call1(args)?;
                if result_obj.is_none() {
                    Ok(None)
                } else if options.expression_mode {
                    Ok(Some(result_obj.repr()?.to_string()))
                } else {
                    Ok(Some(result_obj.str()?.to_string()))
                }
            };

            let (stdout, run_result) = run_with_stdout_capture(py, run)?;

            match run_result {
                Ok(value) => Ok(Some(PyExecOutcome {
                    result: ExecutionResult::success(),
                    stdout,
                    value,
                    exception: None,
                })),
                Err(py_err) => Ok(Some(pyerr_to_outcome(py, py_err, options)?)),
            }
        })?;

        if shell.cancel().is_cancelled() {
            return Ok(Some(interrupted_outcome()));
        }

        Ok(attempted)
    }

    /// Executes raw Python code against the current shell Python namespace.
    pub fn exec_code<SE: extensions::ShellExtensions>(
        shell: &mut crate::Shell<SE>,
        params: &ExecutionParameters,
        code: &str,
    ) -> Result<ExecutionResult, error::Error> {
        let outcome = exec_code_with_options(shell, code, PyExecOptions::default())?;
        if !outcome.stdout.is_empty() {
            write!(params.stdout(shell), "{}", outcome.stdout)?;
        }
        if let Some(value) = &outcome.value {
            writeln!(params.stdout(shell), "{value}")?;
        }
        Ok(outcome.result)
    }

    /// Executes raw Python code and returns structured outcome.
    pub fn exec_code_with_options<SE: extensions::ShellExtensions>(
        shell: &mut crate::Shell<SE>,
        code: &str,
        options: PyExecOptions,
    ) -> Result<PyExecOutcome, error::Error> {
        if shell.cancel().is_cancelled() {
            return Ok(interrupted_outcome());
        }

        let outcome = Python::with_gil(|py| -> Result<PyExecOutcome, error::Error> {
            let globals = shell.python_mut().ensure_globals(py);
            let globals = globals.bind(py);

            let run = || -> Result<Option<String>, PyErr> {
                if options.expression_mode {
                    let builtins = PyModule::import(py, "builtins")?;
                    let eval_fn = builtins.getattr("eval")?;
                    let result_obj = eval_fn.call1((code, globals, globals))?;
                    Ok(Some(result_obj.repr()?.to_string()))
                } else {
                    let builtins = PyModule::import(py, "builtins")?;
                    let exec_fn = builtins.getattr("exec")?;
                    exec_fn.call1((code, globals, globals))?;
                    Ok(None)
                }
            };

            let (stdout, run_result) = run_with_stdout_capture(py, run)?;

            match run_result {
                Ok(value) => Ok(PyExecOutcome {
                    result: ExecutionResult::success(),
                    stdout,
                    value,
                    exception: None,
                }),
                Err(py_err) => pyerr_to_outcome(py, py_err, options),
            }
        })?;

        if shell.cancel().is_cancelled() {
            return Ok(interrupted_outcome());
        }

        Ok(outcome)
    }

    /// Applies structured exception metadata to shell variables.
    pub fn apply_structured_exception<SE: extensions::ShellExtensions>(
        shell: &mut crate::Shell<SE>,
        exception: &PyExceptionInfo,
    ) -> Result<(), error::Error> {
        shell
            .env_mut()
            .set_global("MCBASH_EXCEPTION", ShellVariable::new(exception.ty.clone()))?;
        shell.env_mut().set_global(
            "MCBASH_EXCEPTION_MSG",
            ShellVariable::new(exception.msg.clone()),
        )?;
        shell
            .env_mut()
            .set_global("MCBASH_EXCEPTION_LANG", ShellVariable::new("python"))?;

        let tb = exception
            .traceback_frames
            .iter()
            .enumerate()
            .map(|(i, v)| (i as u64, v.clone()))
            .collect::<BTreeMap<_, _>>();
        shell.env_mut().set_global(
            "MCBASH_EXCEPTION_TB",
            ShellVariable::new(ShellValue::IndexedArray(tb)),
        )?;

        Ok(())
    }

    fn run_with_stdout_capture<'py, F>(
        py: Python<'py>,
        run: F,
    ) -> Result<(String, Result<Option<String>, PyErr>), error::Error>
    where
        F: FnOnce() -> Result<Option<String>, PyErr>,
    {
        let sys = PyModule::import(py, "sys")
            .map_err(|e| error::ErrorKind::InternalError(e.to_string()))?;
        let io = PyModule::import(py, "io")
            .map_err(|e| error::ErrorKind::InternalError(e.to_string()))?;

        let old_stdout = sys
            .getattr("stdout")
            .map_err(|e| error::ErrorKind::InternalError(e.to_string()))?
            .unbind();
        let string_io = io
            .call_method0("StringIO")
            .map_err(|e| error::ErrorKind::InternalError(e.to_string()))?;

        sys.setattr("stdout", &string_io)
            .map_err(|e| error::ErrorKind::InternalError(e.to_string()))?;

        let run_result = run();

        let captured_result = string_io
            .call_method0("getvalue")
            .and_then(|v| v.extract::<String>())
            .map_err(|e| error::ErrorKind::InternalError(e.to_string()));

        let restore_result = sys
            .setattr("stdout", old_stdout.bind(py))
            .map_err(|e| error::ErrorKind::InternalError(e.to_string()));

        restore_result?;
        let captured = captured_result?;

        Ok((captured, run_result))
    }

    fn pyerr_to_outcome(
        py: Python<'_>,
        py_err: PyErr,
        options: PyExecOptions,
    ) -> Result<PyExecOutcome, error::Error> {
        let exc = build_exception_info(py, &py_err)?;

        if !options.structured_exceptions {
            py_err.print(py);
        }

        Ok(PyExecOutcome {
            result: ExecutionExitCode::GeneralError.into(),
            stdout: String::new(),
            value: None,
            exception: Some(exc),
        })
    }

    fn build_exception_info(
        py: Python<'_>,
        py_err: &PyErr,
    ) -> Result<PyExceptionInfo, error::Error> {
        let ty = py_err
            .get_type(py)
            .name()
            .map(|n| n.to_string())
            .unwrap_or_else(|_| "Exception".to_string());

        let msg = py_err
            .value(py)
            .str()
            .map(|s| s.to_string())
            .unwrap_or_else(|_| String::new());

        let traceback_mod = PyModule::import(py, "traceback")
            .map_err(|e| error::ErrorKind::InternalError(e.to_string()))?;

        let frames = traceback_mod
            .call_method1(
                "format_exception",
                (py_err.get_type(py), py_err.value(py), py_err.traceback(py)),
            )
            .and_then(|seq| seq.extract::<Vec<String>>())
            .map_err(|e| error::ErrorKind::InternalError(e.to_string()))?;

        Ok(PyExceptionInfo {
            ty,
            msg,
            traceback_frames: frames,
        })
    }

    fn interrupted_outcome() -> PyExecOutcome {
        PyExecOutcome {
            result: ExecutionExitCode::Interrupted.into(),
            stdout: String::new(),
            value: None,
            exception: None,
        }
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
            return Ok(PyBool::new(py, true).to_owned().unbind().into());
        }
        if lowered == "false" {
            return Ok(PyBool::new(py, false).to_owned().unbind().into());
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
}

#[cfg(not(feature = "python-pyo3"))]
#[allow(missing_docs, reason = "stub implementations for non-python builds")]
mod imp {
    use crate::{ExecutionExitCode, ExecutionParameters, ExecutionResult, error, extensions};

    /// Python configuration and namespace state attached to a shell context.
    #[derive(Clone, Debug, Default)]
    pub struct PythonContext {
        /// Enables command-not-found fallback for dotted names.
        pub implicit_dotted_dispatch: bool,
        /// Enables shell-argument auto-conversion for callable dispatch.
        pub auto_convert_args: bool,
    }

    /// Execution options for Python invocation.
    #[derive(Clone, Copy, Debug, Default)]
    pub struct PyExecOptions {
        /// Evaluate input as expression.
        pub expression_mode: bool,
        /// Structured exception mode.
        pub structured_exceptions: bool,
    }

    /// Structured Python exception information for shell-side capture.
    #[derive(Clone, Debug)]
    pub struct PyExceptionInfo {
        /// Exception type name.
        pub ty: String,
        /// Exception message.
        pub msg: String,
        /// Traceback frames.
        pub traceback_frames: Vec<String>,
    }

    /// Complete execution outcome used by `py` builtin option handling.
    pub struct PyExecOutcome {
        /// Shell execution result status.
        pub result: ExecutionResult,
        /// Captured Python stdout.
        pub stdout: String,
        /// String/representation value produced by callable/expression mode.
        pub value: Option<String>,
        /// Structured exception, if one occurred.
        pub exception: Option<PyExceptionInfo>,
    }

    impl PythonContext {
        /// Create context defaults for interactive or non-interactive mode.
        pub fn defaults_for_interactive(interactive: bool) -> Self {
            Self {
                implicit_dotted_dispatch: interactive,
                auto_convert_args: true,
            }
        }

        /// Returns a cloned context suitable for isolated execution.
        pub fn snapshot_for_isolated(&self) -> Self {
            self.clone()
        }
    }

    pub fn exec_unified<SE: extensions::ShellExtensions>(
        shell: &mut crate::Shell<SE>,
        params: &ExecutionParameters,
        tokens: &[String],
    ) -> Result<ExecutionResult, error::Error> {
        Ok(exec_unified_with_options(shell, params, tokens, PyExecOptions::default())?.result)
    }

    pub fn exec_unified_with_options<SE: extensions::ShellExtensions>(
        _shell: &mut crate::Shell<SE>,
        _params: &ExecutionParameters,
        _tokens: &[String],
        options: PyExecOptions,
    ) -> Result<PyExecOutcome, error::Error> {
        let _ = (options.expression_mode, options.structured_exceptions);
        Ok(PyExecOutcome {
            result: ExecutionExitCode::GeneralError.into(),
            stdout: String::new(),
            value: None,
            exception: None,
        })
    }

    pub fn try_callable<SE: extensions::ShellExtensions>(
        _shell: &mut crate::Shell<SE>,
        _params: &ExecutionParameters,
        _tokens: &[String],
    ) -> Result<Option<ExecutionResult>, error::Error> {
        Ok(None)
    }

    pub fn try_callable_with_options<SE: extensions::ShellExtensions>(
        _shell: &mut crate::Shell<SE>,
        _tokens: &[String],
        _options: PyExecOptions,
    ) -> Result<Option<PyExecOutcome>, error::Error> {
        Ok(None)
    }

    pub fn exec_code<SE: extensions::ShellExtensions>(
        shell: &mut crate::Shell<SE>,
        _params: &ExecutionParameters,
        code: &str,
    ) -> Result<ExecutionResult, error::Error> {
        Ok(exec_code_with_options(shell, code, PyExecOptions::default())?.result)
    }

    pub fn exec_code_with_options<SE: extensions::ShellExtensions>(
        _shell: &mut crate::Shell<SE>,
        _code: &str,
        options: PyExecOptions,
    ) -> Result<PyExecOutcome, error::Error> {
        let _ = (options.expression_mode, options.structured_exceptions);
        Ok(PyExecOutcome {
            result: ExecutionExitCode::GeneralError.into(),
            stdout: String::new(),
            value: None,
            exception: None,
        })
    }

    pub fn apply_structured_exception<SE: extensions::ShellExtensions>(
        _shell: &mut crate::Shell<SE>,
        _exception: &PyExceptionInfo,
    ) -> Result<(), error::Error> {
        Ok(())
    }
}

pub use imp::*;

#[cfg(all(test, feature = "python-pyo3"))]
mod tests {
    use super::*;
    use crate::{ExecutionExitCode, ShellValue};

    #[tokio::test]
    async fn expression_mode_returns_repr_value() -> anyhow::Result<()> {
        let mut shell = crate::Shell::builder().build().await?;
        let outcome = exec_code_with_options(
            &mut shell,
            "1 + 2",
            PyExecOptions {
                expression_mode: true,
                structured_exceptions: false,
            },
        )?;

        assert!(outcome.result.is_success());
        assert_eq!(outcome.value.as_deref(), Some("3"));
        assert_eq!(outcome.stdout, "");
        assert!(outcome.exception.is_none());
        Ok(())
    }

    #[tokio::test]
    async fn exec_mode_captures_stdout() -> anyhow::Result<()> {
        let mut shell = crate::Shell::builder().build().await?;
        let outcome = exec_code_with_options(
            &mut shell,
            "print('hello from py')",
            PyExecOptions::default(),
        )?;

        assert!(outcome.result.is_success());
        assert_eq!(outcome.stdout, "hello from py\n");
        assert!(outcome.value.is_none());
        assert!(outcome.exception.is_none());
        Ok(())
    }

    #[tokio::test]
    async fn structured_exception_populates_shell_vars() -> anyhow::Result<()> {
        let mut shell = crate::Shell::builder().build().await?;
        let outcome = exec_code_with_options(
            &mut shell,
            "raise ValueError('bad input')",
            PyExecOptions {
                expression_mode: false,
                structured_exceptions: true,
            },
        )?;

        assert!(matches!(
            outcome.result.exit_code,
            ExecutionExitCode::GeneralError
        ));
        assert!(outcome.stdout.is_empty());

        let exception = outcome.exception.expect("expected structured exception");
        apply_structured_exception(&mut shell, &exception)?;

        assert_eq!(
            shell.env_str("MCBASH_EXCEPTION").as_deref(),
            Some("ValueError")
        );
        assert_eq!(
            shell.env_str("MCBASH_EXCEPTION_MSG").as_deref(),
            Some("bad input")
        );
        assert_eq!(
            shell.env_str("MCBASH_EXCEPTION_LANG").as_deref(),
            Some("python")
        );

        let tb_var = shell
            .env_var("MCBASH_EXCEPTION_TB")
            .expect("traceback variable must be set");
        let ShellValue::IndexedArray(tb) = tb_var.value() else {
            panic!("MCBASH_EXCEPTION_TB must be an indexed array");
        };
        assert!(!tb.is_empty());

        Ok(())
    }
}
