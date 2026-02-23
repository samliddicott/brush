//! Python runtime context and execution helpers.

#[cfg(feature = "python-pyo3")]
mod imp {
    use std::cell::RefCell;
    use std::collections::BTreeMap;
    use std::collections::HashSet;
    use std::io::Write;

    use pyo3::exceptions::{PyKeyError, PyRuntimeError};
    use pyo3::prelude::*;
    use pyo3::types::{PyAny, PyBool, PyDict, PyList, PyModule, PyTuple};

    use crate::{
        ExecutionExitCode, ExecutionParameters, ExecutionResult, ShellValue, ShellVariable, error,
        extensions,
    };

    thread_local! {
        static ACTIVE_BRIDGE: RefCell<Option<*mut dyn LiveBridge>> = RefCell::new(None);
    }

    trait LiveBridge {
        fn vars_get(&mut self, py: Python<'_>, name: &str) -> PyResult<PyObject>;
        fn vars_set(&mut self, name: &str, value: &Bound<'_, PyAny>) -> PyResult<()>;
        fn vars_del(&mut self, name: &str) -> PyResult<()>;
        fn vars_contains(&mut self, name: &str) -> PyResult<bool>;
        fn vars_keys(&mut self) -> PyResult<Vec<String>>;
        fn vars_attrs(&mut self, py: Python<'_>, name: &str) -> PyResult<PyObject>;
        fn vars_set_attrs(
            &mut self,
            py: Python<'_>,
            name: &str,
            attrs: Option<&Bound<'_, PyDict>>,
        ) -> PyResult<PyObject>;
        fn vars_declare(
            &mut self,
            py: Python<'_>,
            name: &str,
            value: &Bound<'_, PyAny>,
            attrs: Option<&Bound<'_, PyDict>>,
        ) -> PyResult<PyObject>;
        fn env_get(&mut self, py: Python<'_>, name: &str) -> PyResult<PyObject>;
        fn env_set(&mut self, name: &str, value: &Bound<'_, PyAny>) -> PyResult<()>;
        fn env_del(&mut self, name: &str) -> PyResult<()>;
        fn env_contains(&mut self, name: &str) -> PyResult<bool>;
        fn env_keys(&mut self) -> PyResult<Vec<String>>;
        fn funcs_get_body(&mut self, name: &str) -> PyResult<String>;
        fn funcs_contains(&mut self, name: &str) -> PyResult<bool>;
        fn funcs_keys(&mut self) -> PyResult<Vec<String>>;
        fn funcs_set_body(&mut self, name: &str, body: &str) -> PyResult<()>;
        fn funcs_del(&mut self, name: &str) -> PyResult<()>;
        fn run_command(&mut self, py: Python<'_>, req: &RunRequest) -> PyResult<RunOutcome>;
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum StdioSpec {
        Inherit,
        Pipe,
        Stdout,
        DevNull,
    }

    struct RunRequest {
        args: Vec<String>,
        shell: bool,
        capture_output: bool,
        cwd: Option<String>,
        env: Vec<(String, String)>,
    }

    struct RunOutcome {
        returncode: u8,
        stdout: String,
        stderr: String,
        args: Vec<String>,
    }

    struct ShellLiveBridge<SE: extensions::ShellExtensions> {
        shell_ptr: *mut crate::Shell<SE>,
    }

    impl<SE: extensions::ShellExtensions> ShellLiveBridge<SE> {
        fn shell(&self) -> &crate::Shell<SE> {
            // SAFETY: shell_ptr is set from the currently executing shell and valid
            // for the duration of with_active_bridge.
            unsafe { &*self.shell_ptr }
        }

        fn shell_mut(&mut self) -> &mut crate::Shell<SE> {
            // SAFETY: shell_ptr is set from the currently executing shell and valid
            // for the duration of with_active_bridge, accessed on one thread.
            unsafe { &mut *self.shell_ptr }
        }
    }

    impl<SE: extensions::ShellExtensions> LiveBridge for ShellLiveBridge<SE> {
        fn vars_get(&mut self, py: Python<'_>, name: &str) -> PyResult<PyObject> {
            let Some(var) = self.shell().env_var(name) else {
                return Err(PyKeyError::new_err(name.to_string()));
            };
            shell_var_to_py(py, self.shell(), var).map_err(to_py_runtime_error)
        }

        fn vars_set(&mut self, name: &str, value: &Bound<'_, PyAny>) -> PyResult<()> {
            let mut var =
                ShellVariable::new(py_any_to_shell_value(value).map_err(to_py_runtime_error)?);

            if let Some(existing) = self.shell().env_var(name) {
                preserve_attrs_from_existing(existing, &mut var).map_err(to_py_runtime_error)?;
            }

            self.shell_mut()
                .env_mut()
                .set_global(name, var)
                .map_err(to_py_runtime_error)
        }

        fn vars_del(&mut self, name: &str) -> PyResult<()> {
            let _ = self
                .shell_mut()
                .env_mut()
                .unset(name)
                .map_err(to_py_runtime_error)?;
            Ok(())
        }

        fn vars_contains(&mut self, name: &str) -> PyResult<bool> {
            Ok(self.shell().env().is_set(name))
        }

        fn vars_keys(&mut self) -> PyResult<Vec<String>> {
            Ok(self
                .shell()
                .env()
                .iter()
                .map(|(name, _)| name.clone())
                .collect::<Vec<_>>())
        }

        fn vars_attrs(&mut self, py: Python<'_>, name: &str) -> PyResult<PyObject> {
            let Some(var) = self.shell().env_var(name) else {
                return Err(PyKeyError::new_err(name.to_string()));
            };
            attrs_for_var(py, var).map_err(to_py_runtime_error)
        }

        fn vars_set_attrs(
            &mut self,
            py: Python<'_>,
            name: &str,
            attrs: Option<&Bound<'_, PyDict>>,
        ) -> PyResult<PyObject> {
            let Some(existing) = self.shell().env_var(name) else {
                return Err(PyKeyError::new_err(name.to_string()));
            };

            let mut attrs_set = attrs_set_from_var(existing);
            apply_kwargs_to_attrs(&mut attrs_set, attrs).map_err(to_py_runtime_error)?;

            let mut var = existing.clone();
            apply_attrs(&mut var, &attrs_set).map_err(to_py_runtime_error)?;
            self.shell_mut()
                .env_mut()
                .set_global(name, var)
                .map_err(to_py_runtime_error)?;

            let attrs_vec = attrs_set.into_iter().collect::<Vec<_>>();
            Ok(PyList::new(py, attrs_vec)?.unbind().into())
        }

        fn vars_declare(
            &mut self,
            py: Python<'_>,
            name: &str,
            value: &Bound<'_, PyAny>,
            attrs: Option<&Bound<'_, PyDict>>,
        ) -> PyResult<PyObject> {
            let attrs_set = kwargs_to_attrs(attrs).map_err(to_py_runtime_error)?;
            let mut var =
                ShellVariable::new(py_any_to_shell_value(value).map_err(to_py_runtime_error)?);
            apply_attrs(&mut var, &attrs_set).map_err(to_py_runtime_error)?;
            self.shell_mut()
                .env_mut()
                .set_global(name, var)
                .map_err(to_py_runtime_error)?;

            let attrs_vec = attrs_set.into_iter().collect::<Vec<_>>();
            Ok(PyList::new(py, attrs_vec)?.unbind().into())
        }

        fn env_get(&mut self, py: Python<'_>, name: &str) -> PyResult<PyObject> {
            let Some(var) = self.shell().env_var(name) else {
                return Err(PyKeyError::new_err(name.to_string()));
            };
            if !var.is_exported() {
                return Err(PyKeyError::new_err(name.to_string()));
            }
            shell_var_to_py(py, self.shell(), var).map_err(to_py_runtime_error)
        }

        fn env_set(&mut self, name: &str, value: &Bound<'_, PyAny>) -> PyResult<()> {
            let mut var =
                ShellVariable::new(py_any_to_shell_value(value).map_err(to_py_runtime_error)?);
            var.export();
            self.shell_mut()
                .env_mut()
                .set_global(name, var)
                .map_err(to_py_runtime_error)
        }

        fn env_del(&mut self, name: &str) -> PyResult<()> {
            let _ = self
                .shell_mut()
                .env_mut()
                .unset(name)
                .map_err(to_py_runtime_error)?;
            Ok(())
        }

        fn env_contains(&mut self, name: &str) -> PyResult<bool> {
            Ok(self
                .shell()
                .env_var(name)
                .is_some_and(crate::variables::ShellVariable::is_exported))
        }

        fn env_keys(&mut self) -> PyResult<Vec<String>> {
            Ok(self
                .shell()
                .env()
                .iter_exported()
                .map(|(name, _)| name.clone())
                .collect::<Vec<_>>())
        }

        fn funcs_get_body(&mut self, name: &str) -> PyResult<String> {
            let Some(reg) = self.shell().funcs().get(name) else {
                return Err(PyKeyError::new_err(name.to_string()));
            };
            Ok(reg.definition().body.to_string())
        }

        fn funcs_contains(&mut self, name: &str) -> PyResult<bool> {
            Ok(self.shell().funcs().get(name).is_some())
        }

        fn funcs_keys(&mut self) -> PyResult<Vec<String>> {
            Ok(self
                .shell()
                .funcs()
                .iter()
                .map(|(name, _)| name.clone())
                .collect::<Vec<_>>())
        }

        fn funcs_set_body(&mut self, name: &str, body: &str) -> PyResult<()> {
            let body_text = if body.trim_start().starts_with("()") {
                body.to_string()
            } else {
                format!("() {{\n{body}\n}}")
            };

            self.shell_mut()
                .define_func_from_str(name, body_text.as_str())
                .map_err(to_py_runtime_error)
        }

        fn funcs_del(&mut self, name: &str) -> PyResult<()> {
            let _ = self.shell_mut().undefine_func(name);
            Ok(())
        }

        fn run_command(&mut self, _py: Python<'_>, req: &RunRequest) -> PyResult<RunOutcome> {
            let command = build_command_string(req);
            if req.capture_output {
                self.run_capture_for_call(req, command.as_str())
            } else if req.shell || req.cwd.is_some() || !req.env.is_empty() {
                self.run_in_child(req, command.as_str())
            } else {
                self.run_in_current(req, command.as_str())
            }
        }
    }

    impl<SE: extensions::ShellExtensions> ShellLiveBridge<SE> {
        fn run_in_current(&mut self, req: &RunRequest, command: &str) -> PyResult<RunOutcome> {
            let params = self.shell().default_exec_params();
            let source = crate::SourceInfo::from("python-bridge");
            let result = tokio::task::block_in_place(|| {
                let rt = tokio::runtime::Handle::current();
                rt.block_on(
                    self.shell_mut()
                        .run_string(command.to_string(), &source, &params),
                )
            })
            .map_err(to_py_runtime_error)?;

            drop(params);

            Ok(RunOutcome {
                returncode: result.exit_code.into(),
                stdout: String::new(),
                stderr: String::new(),
                args: req.args.clone(),
            })
        }

        fn run_in_child(&mut self, req: &RunRequest, command: &str) -> PyResult<RunOutcome> {
            let mut subshell = self.shell().clone();
            let mut params = subshell.default_exec_params();
            params.process_group_policy = crate::ProcessGroupPolicy::SameProcessGroup;
            let source = crate::SourceInfo::from("python-bridge-child");
            let result = tokio::task::block_in_place(|| {
                let rt = tokio::runtime::Handle::current();
                rt.block_on(subshell.run_string(command.to_string(), &source, &params))
            })
            .map_err(to_py_runtime_error)?;

            Ok(RunOutcome {
                returncode: result.exit_code.into(),
                stdout: String::new(),
                stderr: String::new(),
                args: req.args.clone(),
            })
        }

        fn run_capture_for_call(&mut self, req: &RunRequest, command: &str) -> PyResult<RunOutcome> {
            let params = self.shell().default_exec_params();
            let output = tokio::task::block_in_place(|| {
                let rt = tokio::runtime::Handle::current();
                rt.block_on(crate::commands::invoke_command_in_subshell_and_get_output(
                    self.shell_mut(),
                    &params,
                    command.to_string(),
                ))
            })
            .map_err(to_py_runtime_error)?;

            Ok(RunOutcome {
                returncode: self.shell().last_exit_status(),
                stdout: output,
                stderr: String::new(),
                args: req.args.clone(),
            })
        }
    }

    fn shell_escape_single(s: &str) -> String {
        if s.is_empty() {
            return "''".to_string();
        }
        let escaped = s.replace('\'', "'\"'\"'");
        format!("'{escaped}'")
    }

    fn build_command_string(req: &RunRequest) -> String {
        let mut prefix_parts = Vec::<String>::new();
        if let Some(cwd) = &req.cwd {
            prefix_parts.push(format!("cd {} || exit $?", shell_escape_single(cwd)));
        }
        if req.shell && !req.env.is_empty() {
            let exports = req
                .env
                .iter()
                .map(|(k, v)| format!("export {k}={}", shell_escape_single(v)))
                .collect::<Vec<_>>()
                .join("; ");
            prefix_parts.push(exports);
        }

        let base = if req.shell {
            req.args.join(" ")
        } else {
            let mut iter = req.args.iter();
            let Some(first) = iter.next() else {
                return String::new();
            };
            let mut cmd = shell_escape_single(first);
            if !req.env.is_empty() {
                let assigns = req
                    .env
                    .iter()
                    .map(|(k, v)| format!("{k}={}", shell_escape_single(v)))
                    .collect::<Vec<_>>()
                    .join(" ");
                cmd = format!("{assigns} {cmd}");
            }
            for a in iter {
                cmd.push(' ');
                cmd.push_str(shell_escape_single(a).as_str());
            }
            cmd
        };

        if prefix_parts.is_empty() {
            base
        } else {
            format!("{}; {base}", prefix_parts.join("; "))
        }
    }

    fn to_py_runtime_error(err: error::Error) -> PyErr {
        PyRuntimeError::new_err(err.to_string())
    }

    fn with_live_bridge<R>(f: impl FnOnce(&mut dyn LiveBridge) -> PyResult<R>) -> PyResult<R> {
        ACTIVE_BRIDGE.with(|slot| {
            let slot = slot.borrow_mut();
            let Some(ptr) = *slot else {
                return Err(PyRuntimeError::new_err("python bridge is not active"));
            };

            // SAFETY: pointer is set by with_active_bridge and valid during call.
            let bridge = unsafe { &mut *ptr };
            f(bridge)
        })
    }

    fn with_active_bridge<R>(bridge: &mut dyn LiveBridge, f: impl FnOnce() -> R) -> R {
        ACTIVE_BRIDGE.with(|slot| {
            let bridge_ptr: *mut dyn LiveBridge = bridge;
            // SAFETY: lifetime is constrained by this function, and pointer is restored before return.
            let bridge_ptr_static: *mut dyn LiveBridge = unsafe { std::mem::transmute(bridge_ptr) };
            let prev = slot.replace(Some(bridge_ptr_static));
            let out = f();
            let _ = slot.replace(prev);
            out
        })
    }

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
            let mut bridge = ShellLiveBridge {
                shell_ptr: shell as *mut _,
            };
            let globals = shell.python_mut().ensure_globals(py);
            let globals = globals.bind(py);
            install_bash_bridge(py, globals)?;

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

            let (stdout, run_result) =
                with_active_bridge(&mut bridge, || run_with_stdout_capture(py, run))?;

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
            let mut bridge = ShellLiveBridge {
                shell_ptr: shell as *mut _,
            };
            let globals = shell.python_mut().ensure_globals(py);
            let globals = globals.bind(py);
            install_bash_bridge(py, globals)?;

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

            let (stdout, run_result) =
                with_active_bridge(&mut bridge, || run_with_stdout_capture(py, run))?;

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

    fn install_bash_bridge(
        py: Python<'_>,
        globals: &Bound<'_, PyDict>,
    ) -> Result<(), error::Error> {
        let bridge_module = PyModule::new(py, "_brush_bridge")
            .map_err(|e| error::ErrorKind::InternalError(e.to_string()))?;
        bridge_module
            .add_function(
                pyo3::wrap_pyfunction!(brush_vars_get, &bridge_module)
                    .map_err(|e| error::ErrorKind::InternalError(e.to_string()))?,
            )
            .map_err(|e| error::ErrorKind::InternalError(e.to_string()))?;
        bridge_module
            .add_function(
                pyo3::wrap_pyfunction!(brush_vars_set, &bridge_module)
                    .map_err(|e| error::ErrorKind::InternalError(e.to_string()))?,
            )
            .map_err(|e| error::ErrorKind::InternalError(e.to_string()))?;
        bridge_module
            .add_function(
                pyo3::wrap_pyfunction!(brush_vars_del, &bridge_module)
                    .map_err(|e| error::ErrorKind::InternalError(e.to_string()))?,
            )
            .map_err(|e| error::ErrorKind::InternalError(e.to_string()))?;
        bridge_module
            .add_function(
                pyo3::wrap_pyfunction!(brush_vars_contains, &bridge_module)
                    .map_err(|e| error::ErrorKind::InternalError(e.to_string()))?,
            )
            .map_err(|e| error::ErrorKind::InternalError(e.to_string()))?;
        bridge_module
            .add_function(
                pyo3::wrap_pyfunction!(brush_vars_keys, &bridge_module)
                    .map_err(|e| error::ErrorKind::InternalError(e.to_string()))?,
            )
            .map_err(|e| error::ErrorKind::InternalError(e.to_string()))?;
        bridge_module
            .add_function(
                pyo3::wrap_pyfunction!(brush_vars_attrs, &bridge_module)
                    .map_err(|e| error::ErrorKind::InternalError(e.to_string()))?,
            )
            .map_err(|e| error::ErrorKind::InternalError(e.to_string()))?;
        bridge_module
            .add_function(
                pyo3::wrap_pyfunction!(brush_vars_set_attrs, &bridge_module)
                    .map_err(|e| error::ErrorKind::InternalError(e.to_string()))?,
            )
            .map_err(|e| error::ErrorKind::InternalError(e.to_string()))?;
        bridge_module
            .add_function(
                pyo3::wrap_pyfunction!(brush_vars_declare, &bridge_module)
                    .map_err(|e| error::ErrorKind::InternalError(e.to_string()))?,
            )
            .map_err(|e| error::ErrorKind::InternalError(e.to_string()))?;
        bridge_module
            .add_function(
                pyo3::wrap_pyfunction!(brush_env_get, &bridge_module)
                    .map_err(|e| error::ErrorKind::InternalError(e.to_string()))?,
            )
            .map_err(|e| error::ErrorKind::InternalError(e.to_string()))?;
        bridge_module
            .add_function(
                pyo3::wrap_pyfunction!(brush_env_set, &bridge_module)
                    .map_err(|e| error::ErrorKind::InternalError(e.to_string()))?,
            )
            .map_err(|e| error::ErrorKind::InternalError(e.to_string()))?;
        bridge_module
            .add_function(
                pyo3::wrap_pyfunction!(brush_env_del, &bridge_module)
                    .map_err(|e| error::ErrorKind::InternalError(e.to_string()))?,
            )
            .map_err(|e| error::ErrorKind::InternalError(e.to_string()))?;
        bridge_module
            .add_function(
                pyo3::wrap_pyfunction!(brush_env_contains, &bridge_module)
                    .map_err(|e| error::ErrorKind::InternalError(e.to_string()))?,
            )
            .map_err(|e| error::ErrorKind::InternalError(e.to_string()))?;
        bridge_module
            .add_function(
                pyo3::wrap_pyfunction!(brush_env_keys, &bridge_module)
                    .map_err(|e| error::ErrorKind::InternalError(e.to_string()))?,
            )
            .map_err(|e| error::ErrorKind::InternalError(e.to_string()))?;
        bridge_module
            .add_function(
                pyo3::wrap_pyfunction!(brush_funcs_get_body, &bridge_module)
                    .map_err(|e| error::ErrorKind::InternalError(e.to_string()))?,
            )
            .map_err(|e| error::ErrorKind::InternalError(e.to_string()))?;
        bridge_module
            .add_function(
                pyo3::wrap_pyfunction!(brush_funcs_contains, &bridge_module)
                    .map_err(|e| error::ErrorKind::InternalError(e.to_string()))?,
            )
            .map_err(|e| error::ErrorKind::InternalError(e.to_string()))?;
        bridge_module
            .add_function(
                pyo3::wrap_pyfunction!(brush_funcs_keys, &bridge_module)
                    .map_err(|e| error::ErrorKind::InternalError(e.to_string()))?,
            )
            .map_err(|e| error::ErrorKind::InternalError(e.to_string()))?;
        bridge_module
            .add_function(
                pyo3::wrap_pyfunction!(brush_funcs_set_body, &bridge_module)
                    .map_err(|e| error::ErrorKind::InternalError(e.to_string()))?,
            )
            .map_err(|e| error::ErrorKind::InternalError(e.to_string()))?;
        bridge_module
            .add_function(
                pyo3::wrap_pyfunction!(brush_funcs_del, &bridge_module)
                    .map_err(|e| error::ErrorKind::InternalError(e.to_string()))?,
            )
            .map_err(|e| error::ErrorKind::InternalError(e.to_string()))?;
        bridge_module
            .add_function(
                pyo3::wrap_pyfunction!(brush_call, &bridge_module)
                    .map_err(|e| error::ErrorKind::InternalError(e.to_string()))?,
            )
            .map_err(|e| error::ErrorKind::InternalError(e.to_string()))?;
        bridge_module
            .add_function(
                pyo3::wrap_pyfunction!(brush_run, &bridge_module)
                    .map_err(|e| error::ErrorKind::InternalError(e.to_string()))?,
            )
            .map_err(|e| error::ErrorKind::InternalError(e.to_string()))?;

        globals
            .set_item("_brush_bridge", bridge_module)
            .map_err(|e| error::ErrorKind::InternalError(e.to_string()))?;

        let builtins = PyModule::import(py, "builtins")
            .map_err(|e| error::ErrorKind::InternalError(e.to_string()))?;
        let exec_fn = builtins
            .getattr("exec")
            .map_err(|e| error::ErrorKind::InternalError(e.to_string()))?;
        exec_fn
            .call1((BASH_BRIDGE_BOOTSTRAP, globals, globals))
            .map_err(|e| error::ErrorKind::InternalError(e.to_string()))?;
        Ok(())
    }

    fn shell_var_to_py<'py, SE: extensions::ShellExtensions>(
        py: Python<'py>,
        shell: &crate::Shell<SE>,
        var: &ShellVariable,
    ) -> Result<PyObject, error::Error> {
        match var.resolve_value(shell) {
            ShellValue::Unset(_) => Ok(py.None()),
            ShellValue::String(s) => Ok(s
                .into_pyobject(py)
                .unwrap_or_else(|never| match never {})
                .unbind()
                .into()),
            ShellValue::IndexedArray(values) => {
                let mut ordered = values.into_iter().collect::<Vec<(u64, String)>>();
                ordered.sort_by_key(|(k, _)| *k);
                let list_values = ordered.into_iter().map(|(_, v)| v).collect::<Vec<_>>();
                Ok(PyList::new(py, list_values)
                    .map_err(|e| error::ErrorKind::InternalError(e.to_string()))?
                    .unbind()
                    .into())
            }
            ShellValue::AssociativeArray(values) => {
                let d = PyDict::new(py);
                for (k, v) in values {
                    d.set_item(k, v)
                        .map_err(|e| error::ErrorKind::InternalError(e.to_string()))?;
                }
                Ok(d.unbind().into())
            }
            ShellValue::Dynamic { .. } => Ok(var
                .value()
                .to_cow_str(shell)
                .to_string()
                .into_pyobject(py)
                .unwrap_or_else(|never| match never {})
                .unbind()
                .into()),
        }
    }

    fn attrs_for_var(py: Python<'_>, var: &ShellVariable) -> Result<PyObject, error::Error> {
        let mut attrs = Vec::<String>::new();
        if var.is_exported() {
            attrs.push("exported".to_string());
        }
        if var.is_readonly() {
            attrs.push("readonly".to_string());
        }
        if var.is_trace_enabled() {
            attrs.push("trace".to_string());
        }
        if var.is_treated_as_integer() {
            attrs.push("integer".to_string());
        }
        if var.is_treated_as_nameref() {
            attrs.push("nameref".to_string());
        }
        match var.get_update_transform() {
            crate::variables::ShellVariableUpdateTransform::Lowercase => {
                attrs.push("lowercase".to_string());
            }
            crate::variables::ShellVariableUpdateTransform::Uppercase => {
                attrs.push("uppercase".to_string());
            }
            crate::variables::ShellVariableUpdateTransform::None
            | crate::variables::ShellVariableUpdateTransform::Capitalize => {}
        }

        Ok(PyList::new(py, attrs)
            .map_err(|e| error::ErrorKind::InternalError(e.to_string()))?
            .unbind()
            .into())
    }

    fn apply_attrs(var: &mut ShellVariable, attrs: &HashSet<String>) -> Result<(), error::Error> {
        if attrs.contains("exported") {
            var.export();
        } else {
            var.unexport();
        }

        if attrs.contains("trace") {
            var.enable_trace();
        } else {
            var.disable_trace();
        }

        if attrs.contains("integer") {
            var.treat_as_integer();
        } else {
            var.unset_treat_as_integer();
        }

        if attrs.contains("nameref") {
            var.treat_as_nameref();
        } else {
            var.unset_treat_as_nameref();
        }

        if attrs.contains("readonly") {
            var.set_readonly();
        }

        if attrs.contains("uppercase") {
            var.set_update_transform(crate::variables::ShellVariableUpdateTransform::Uppercase);
        } else if attrs.contains("lowercase") {
            var.set_update_transform(crate::variables::ShellVariableUpdateTransform::Lowercase);
        } else {
            var.set_update_transform(crate::variables::ShellVariableUpdateTransform::None);
        }

        Ok(())
    }

    fn py_any_to_shell_value(any: &Bound<'_, PyAny>) -> Result<ShellValue, error::Error> {
        if let Ok(d) = any.downcast::<PyDict>() {
            let mut map = BTreeMap::<String, String>::new();
            for (k, v) in d.iter() {
                map.insert(any_to_key_string(k)?, py_any_to_string(&v)?);
            }
            return Ok(ShellValue::AssociativeArray(map));
        }

        if let Ok(list) = any.downcast::<PyList>() {
            let mut map = BTreeMap::<u64, String>::new();
            for (idx, v) in list.iter().enumerate() {
                map.insert(idx as u64, py_any_to_string(&v)?);
            }
            return Ok(ShellValue::IndexedArray(map));
        }

        if let Ok(tuple) = any.downcast::<PyTuple>() {
            let mut map = BTreeMap::<u64, String>::new();
            for (idx, v) in tuple.iter().enumerate() {
                map.insert(idx as u64, py_any_to_string(&v)?);
            }
            return Ok(ShellValue::IndexedArray(map));
        }

        Ok(ShellValue::String(py_any_to_string(any)?))
    }

    fn py_any_to_string(any: &Bound<'_, PyAny>) -> Result<String, error::Error> {
        if any.is_none() {
            return Ok(String::new());
        }
        if let Ok(s) = any.extract::<String>() {
            return Ok(s);
        }
        if let Ok(v) = any.extract::<bool>() {
            return Ok(v.to_string());
        }
        if let Ok(v) = any.extract::<i64>() {
            return Ok(v.to_string());
        }
        if let Ok(v) = any.extract::<f64>() {
            return Ok(v.to_string());
        }
        any.str()
            .map(|s| s.to_string())
            .map_err(|e| error::ErrorKind::InternalError(e.to_string()).into())
    }

    fn any_to_key_string(any: Bound<'_, PyAny>) -> Result<String, error::Error> {
        if let Ok(s) = any.extract::<String>() {
            return Ok(s);
        }
        any.str()
            .map(|s| s.to_string())
            .map_err(|e| error::ErrorKind::InternalError(e.to_string()).into())
    }

    fn kwargs_to_attrs(
        kwargs: Option<&Bound<'_, PyDict>>,
    ) -> Result<HashSet<String>, error::Error> {
        let mut attrs_set = HashSet::new();
        apply_kwargs_to_attrs(&mut attrs_set, kwargs)?;
        Ok(attrs_set)
    }

    fn apply_kwargs_to_attrs(
        attrs_set: &mut HashSet<String>,
        kwargs: Option<&Bound<'_, PyDict>>,
    ) -> Result<(), error::Error> {
        let Some(kwargs) = kwargs else {
            return Ok(());
        };

        for (k, v) in kwargs.iter() {
            let key = any_to_key_string(k)?;
            let val = v
                .extract::<bool>()
                .map_err(|e| error::ErrorKind::InternalError(e.to_string()))?;
            if val {
                attrs_set.insert(key);
            } else {
                attrs_set.remove(key.as_str());
            }
        }
        if attrs_set.contains("uppercase") && attrs_set.contains("lowercase") {
            attrs_set.remove("lowercase");
        }

        Ok(())
    }

    fn attrs_set_from_var(var: &ShellVariable) -> HashSet<String> {
        let mut attrs_set = HashSet::new();
        if var.is_exported() {
            attrs_set.insert("exported".to_string());
        }
        if var.is_readonly() {
            attrs_set.insert("readonly".to_string());
        }
        if var.is_trace_enabled() {
            attrs_set.insert("trace".to_string());
        }
        if var.is_treated_as_integer() {
            attrs_set.insert("integer".to_string());
        }
        if var.is_treated_as_nameref() {
            attrs_set.insert("nameref".to_string());
        }
        match var.get_update_transform() {
            crate::variables::ShellVariableUpdateTransform::Lowercase => {
                attrs_set.insert("lowercase".to_string());
            }
            crate::variables::ShellVariableUpdateTransform::Uppercase => {
                attrs_set.insert("uppercase".to_string());
            }
            crate::variables::ShellVariableUpdateTransform::None
            | crate::variables::ShellVariableUpdateTransform::Capitalize => {}
        }
        attrs_set
    }

    fn preserve_attrs_from_existing(
        existing: &ShellVariable,
        var: &mut ShellVariable,
    ) -> Result<(), error::Error> {
        if existing.is_exported() {
            var.export();
        }
        if existing.is_trace_enabled() {
            var.enable_trace();
        }
        if existing.is_treated_as_integer() {
            var.treat_as_integer();
        }
        if existing.is_treated_as_nameref() {
            var.treat_as_nameref();
        }
        if existing.is_readonly() {
            var.set_readonly();
        }
        var.set_update_transform(existing.get_update_transform());
        Ok(())
    }

    fn extract_args(any: &Bound<'_, PyAny>) -> PyResult<Vec<String>> {
        if let Ok(s) = any.extract::<String>() {
            return Ok(vec![s]);
        }
        if let Ok(v) = any.extract::<Vec<String>>() {
            return Ok(v);
        }
        if let Ok(tuple) = any.downcast::<PyTuple>() {
            let mut out = Vec::with_capacity(tuple.len());
            for item in tuple.iter() {
                out.push(
                    item.extract::<String>()
                        .map_err(|_| PyRuntimeError::new_err("args must be strings"))?,
                );
            }
            return Ok(out);
        }
        Err(PyRuntimeError::new_err("args must be str or sequence[str]"))
    }

    fn parse_stdio_spec(spec: Option<String>, default: StdioSpec) -> PyResult<StdioSpec> {
        match spec.as_deref() {
            None => Ok(default),
            Some("PIPE") => Ok(StdioSpec::Pipe),
            Some("STDOUT") => Ok(StdioSpec::Stdout),
            Some("DEVNULL") => Ok(StdioSpec::DevNull),
            Some("INHERIT") => Ok(StdioSpec::Inherit),
            Some(other) => Err(PyRuntimeError::new_err(format!(
                "invalid stdio spec: {other}"
            ))),
        }
    }

    fn run_outcome_to_py_dict(py: Python<'_>, out: RunOutcome) -> PyResult<PyObject> {
        let d = PyDict::new(py);
        d.set_item("returncode", out.returncode)?;
        d.set_item("stdout", out.stdout)?;
        d.set_item("stderr", out.stderr)?;
        d.set_item("args", out.args)?;
        Ok(d.unbind().into())
    }

    #[pyfunction]
    fn brush_vars_get(py: Python<'_>, name: String) -> PyResult<PyObject> {
        with_live_bridge(|bridge| bridge.vars_get(py, name.as_str()))
    }

    #[pyfunction]
    fn brush_vars_set(name: String, value: Bound<'_, PyAny>) -> PyResult<()> {
        with_live_bridge(|bridge| bridge.vars_set(name.as_str(), &value))
    }

    #[pyfunction]
    fn brush_vars_del(name: String) -> PyResult<()> {
        with_live_bridge(|bridge| bridge.vars_del(name.as_str()))
    }

    #[pyfunction]
    fn brush_vars_contains(name: String) -> PyResult<bool> {
        with_live_bridge(|bridge| bridge.vars_contains(name.as_str()))
    }

    #[pyfunction]
    fn brush_vars_keys() -> PyResult<Vec<String>> {
        with_live_bridge(|bridge| bridge.vars_keys())
    }

    #[pyfunction]
    fn brush_vars_attrs(py: Python<'_>, name: String) -> PyResult<PyObject> {
        with_live_bridge(|bridge| bridge.vars_attrs(py, name.as_str()))
    }

    #[pyfunction]
    #[pyo3(signature = (name, attrs=None))]
    fn brush_vars_set_attrs(
        py: Python<'_>,
        name: String,
        attrs: Option<Bound<'_, PyDict>>,
    ) -> PyResult<PyObject> {
        with_live_bridge(|bridge| bridge.vars_set_attrs(py, name.as_str(), attrs.as_ref()))
    }

    #[pyfunction]
    #[pyo3(signature = (name, value, attrs=None))]
    fn brush_vars_declare(
        py: Python<'_>,
        name: String,
        value: Bound<'_, PyAny>,
        attrs: Option<Bound<'_, PyDict>>,
    ) -> PyResult<PyObject> {
        with_live_bridge(|bridge| bridge.vars_declare(py, name.as_str(), &value, attrs.as_ref()))
    }

    #[pyfunction]
    fn brush_env_get(py: Python<'_>, name: String) -> PyResult<PyObject> {
        with_live_bridge(|bridge| bridge.env_get(py, name.as_str()))
    }

    #[pyfunction]
    fn brush_env_set(name: String, value: Bound<'_, PyAny>) -> PyResult<()> {
        with_live_bridge(|bridge| bridge.env_set(name.as_str(), &value))
    }

    #[pyfunction]
    fn brush_env_del(name: String) -> PyResult<()> {
        with_live_bridge(|bridge| bridge.env_del(name.as_str()))
    }

    #[pyfunction]
    fn brush_env_contains(name: String) -> PyResult<bool> {
        with_live_bridge(|bridge| bridge.env_contains(name.as_str()))
    }

    #[pyfunction]
    fn brush_env_keys() -> PyResult<Vec<String>> {
        with_live_bridge(|bridge| bridge.env_keys())
    }

    #[pyfunction]
    fn brush_funcs_get_body(name: String) -> PyResult<String> {
        with_live_bridge(|bridge| bridge.funcs_get_body(name.as_str()))
    }

    #[pyfunction]
    fn brush_funcs_contains(name: String) -> PyResult<bool> {
        with_live_bridge(|bridge| bridge.funcs_contains(name.as_str()))
    }

    #[pyfunction]
    fn brush_funcs_keys() -> PyResult<Vec<String>> {
        with_live_bridge(|bridge| bridge.funcs_keys())
    }

    #[pyfunction]
    fn brush_funcs_set_body(name: String, body: String) -> PyResult<()> {
        with_live_bridge(|bridge| bridge.funcs_set_body(name.as_str(), body.as_str()))
    }

    #[pyfunction]
    fn brush_funcs_del(name: String) -> PyResult<()> {
        with_live_bridge(|bridge| bridge.funcs_del(name.as_str()))
    }

    #[pyfunction]
    fn brush_call(py: Python<'_>, args: Bound<'_, PyAny>) -> PyResult<PyObject> {
        let args = extract_args(&args)?;
        let req = RunRequest {
            args,
            shell: false,
            capture_output: true,
            cwd: None,
            env: Vec::new(),
        };
        let out = with_live_bridge(|bridge| bridge.run_command(py, &req))?;
        run_outcome_to_py_dict(py, out)
    }

    #[pyfunction]
    #[pyo3(signature = (args, capture_output=false, stdout=None, stderr=None, check=false, input=None, shell=false, timeout=None, cwd=None, env=None))]
    fn brush_run(
        py: Python<'_>,
        args: Bound<'_, PyAny>,
        capture_output: bool,
        stdout: Option<String>,
        stderr: Option<String>,
        check: bool,
        input: Option<Bound<'_, PyAny>>,
        shell: bool,
        timeout: Option<f64>,
        cwd: Option<String>,
        env: Option<Bound<'_, PyDict>>,
    ) -> PyResult<PyObject> {
        let args = extract_args(&args)?;
        let stdout_spec = if capture_output {
            StdioSpec::Pipe
        } else {
            parse_stdio_spec(stdout, StdioSpec::Inherit)?
        };
        let stderr_spec = if capture_output {
            StdioSpec::Pipe
        } else {
            parse_stdio_spec(stderr, StdioSpec::Inherit)?
        };

        let input_bytes = if let Some(input) = input {
            if let Ok(s) = input.extract::<String>() {
                Some(s.into_bytes())
            } else if let Ok(b) = input.extract::<Vec<u8>>() {
                Some(b)
            } else {
                return Err(PyRuntimeError::new_err("input must be str or bytes"));
            }
        } else {
            None
        };

        if capture_output
            || stdout_spec != StdioSpec::Inherit
            || stderr_spec != StdioSpec::Inherit
            || input_bytes.is_some()
            || timeout.is_some()
        {
            return Err(PyRuntimeError::new_err(
                "bash.run pipe/input/timeout features are deferred to phase 6 (bash.popen)",
            ));
        }

        let mut env_pairs = Vec::<(String, String)>::new();
        if let Some(env_map) = env {
            for (k, v) in env_map.iter() {
                let key = any_to_key_string(k).map_err(to_py_runtime_error)?;
                let val = py_any_to_string(&v).map_err(to_py_runtime_error)?;
                env_pairs.push((key, val));
            }
        }

        let req = RunRequest {
            args,
            shell,
            capture_output,
            cwd,
            env: env_pairs,
        };
        let _ = check;
        let out = with_live_bridge(|bridge| bridge.run_command(py, &req))?;
        run_outcome_to_py_dict(py, out)
    }

    const BASH_BRIDGE_BOOTSTRAP: &str = r#"
class _BrushVarsMap:
    def __getitem__(self, name):
        return _brush_bridge.brush_vars_get(name)

    def __setitem__(self, name, value):
        _brush_bridge.brush_vars_set(name, value)

    def __delitem__(self, name):
        _brush_bridge.brush_vars_del(name)

    def __contains__(self, name):
        return _brush_bridge.brush_vars_contains(name)

    def __iter__(self):
        return iter(_brush_bridge.brush_vars_keys())

    def __len__(self):
        return len(_brush_bridge.brush_vars_keys())

    def attrs(self, name):
        return set(_brush_bridge.brush_vars_attrs(name))

    def set_attrs(self, name, **kwargs):
        return set(_brush_bridge.brush_vars_set_attrs(name, dict(kwargs)))

    def declare(self, name, value="", **kwargs):
        return set(_brush_bridge.brush_vars_declare(name, value, dict(kwargs)))

class _BrushEnvMap:
    def __getitem__(self, name):
        return _brush_bridge.brush_env_get(name)

    def __setitem__(self, name, value):
        _brush_bridge.brush_env_set(name, value)

    def __delitem__(self, name):
        _brush_bridge.brush_env_del(name)

    def __contains__(self, name):
        return _brush_bridge.brush_env_contains(name)

    def __iter__(self):
        return iter(_brush_bridge.brush_env_keys())

    def __len__(self):
        return len(_brush_bridge.brush_env_keys())

def _brush_raise_bash_error(args, result):
    err = RuntimeError("bash command failed")
    err.returncode = result["returncode"]
    err.cmd = list(args)
    err.stdout = result["stdout"]
    err.stderr = result["stderr"]
    raise err

class _BrushFnCallable:
    def __init__(self, name):
        self._name = name

    def __call__(self, *args):
        cmd = [self._name]
        cmd.extend(str(a) for a in args)
        result = _brush_bridge.brush_call(cmd)
        if result["returncode"] != 0:
            _brush_raise_bash_error(cmd, result)
        return result["stdout"].rstrip("\n")

class _BrushFnMap:
    _seq = 0

    def __getitem__(self, name):
        return _brush_bridge.brush_funcs_get_body(name)

    def __setitem__(self, name, value):
        if callable(value):
            py_name = self._next_callable_name(name)
            globals()[py_name] = value
            _brush_bridge.brush_funcs_set_body(name, f'() {{ py {py_name} \"$@\"; }}')
            return
        if not isinstance(value, str):
            raise TypeError("bash.fn assignment expects str body or callable")
        _brush_bridge.brush_funcs_set_body(name, value)

    def __delitem__(self, name):
        _brush_bridge.brush_funcs_del(name)

    def __contains__(self, name):
        return _brush_bridge.brush_funcs_contains(name)

    def __iter__(self):
        return iter(_brush_bridge.brush_funcs_keys())

    def __len__(self):
        return len(_brush_bridge.brush_funcs_keys())

    def __getattr__(self, name):
        if name.startswith("__"):
            raise AttributeError(name)
        if not _brush_bridge.brush_funcs_contains(name):
            raise AttributeError(name)
        return _BrushFnCallable(name)

    def _next_callable_name(self, name):
        _BrushFnMap._seq += 1
        safe = "".join(ch if (ch.isalnum() or ch == "_") else "_" for ch in name)
        if not safe:
            safe = "fn"
        return f"_brush_fn_{safe}_{_BrushFnMap._seq}"

class _Brush:
    def __init__(self):
        self.vars = _BrushVarsMap()
        self.env = _BrushEnvMap()
        self.fn = _BrushFnMap()
        self.PIPE = "PIPE"
        self.STDOUT = "STDOUT"
        self.DEVNULL = "DEVNULL"

    def __call__(self, *args):
        result = _brush_bridge.brush_call(list(args))
        if result["returncode"] != 0:
            _brush_raise_bash_error(args, result)
        return result["stdout"].rstrip("\n")

    def run(self, *args, capture_output=False, stdout=None, stderr=None, check=False, input=None, shell=False, timeout=None, cwd=None, env=None):
        if capture_output:
            stdout = self.PIPE
            stderr = self.PIPE
        result = _brush_bridge.brush_run(
            list(args),
            capture_output,
            stdout,
            stderr,
            check,
            input,
            shell,
            timeout,
            cwd,
            env,
        )
        completed = type("BashCompletedProcess", (), {})()
        completed.args = result["args"]
        completed.returncode = int(result["returncode"])
        completed.stdout = result["stdout"]
        completed.stderr = result["stderr"]
        if check and completed.returncode != 0:
            err = RuntimeError("bash command returned non-zero exit status")
            err.returncode = completed.returncode
            err.cmd = completed.args
            err.stdout = completed.stdout
            err.stderr = completed.stderr
            raise err
        return completed

bash = _Brush()
"#;

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

    #[tokio::test]
    async fn bash_vars_round_trip_and_type_mapping() -> anyhow::Result<()> {
        let mut shell = crate::Shell::builder().build().await?;
        exec_code_with_options(
            &mut shell,
            "bash.vars['sc']='v'; bash.vars['arr']=[1,2,3]; bash.vars['assoc']={'k':'x'}",
            PyExecOptions::default(),
        )?;

        assert_eq!(shell.env_str("sc").as_deref(), Some("v"));

        let arr = shell.env_var("arr").expect("arr set");
        let crate::ShellValue::IndexedArray(arr_values) = arr.value() else {
            panic!("arr must be indexed");
        };
        assert_eq!(arr_values.get(&0).map(|s| s.as_str()), Some("1"));
        assert_eq!(arr_values.get(&1).map(|s| s.as_str()), Some("2"));
        assert_eq!(arr_values.get(&2).map(|s| s.as_str()), Some("3"));

        let assoc = shell.env_var("assoc").expect("assoc set");
        let crate::ShellValue::AssociativeArray(assoc_values) = assoc.value() else {
            panic!("assoc must be associative");
        };
        assert_eq!(assoc_values.get("k").map(|s| s.as_str()), Some("x"));
        Ok(())
    }

    #[tokio::test]
    async fn bash_env_write_marks_export_and_delete_unsets() -> anyhow::Result<()> {
        let mut shell = crate::Shell::builder().build().await?;
        exec_code_with_options(
            &mut shell,
            "bash.env['PH2_ENV']='yes'; del bash.env['PH2_ENV']",
            PyExecOptions::default(),
        )?;
        assert!(shell.env_var("PH2_ENV").is_none());

        exec_code_with_options(
            &mut shell,
            "bash.env['PH2_ENV']='yes'",
            PyExecOptions::default(),
        )?;
        let env_var = shell.env_var("PH2_ENV").expect("env var set");
        assert!(env_var.is_exported());
        assert_eq!(shell.env_str("PH2_ENV").as_deref(), Some("yes"));
        Ok(())
    }

    #[tokio::test]
    async fn bash_vars_attrs_and_declare_apply_flags() -> anyhow::Result<()> {
        let mut shell = crate::Shell::builder().build().await?;
        exec_code_with_options(
            &mut shell,
            "bash.vars.declare('ct', '42', integer=True, exported=True); bash.vars.set_attrs('ct', trace=True)",
            PyExecOptions::default(),
        )?;

        let ct = shell.env_var("ct").expect("ct set");
        assert!(ct.is_exported());
        assert!(ct.is_treated_as_integer());
        assert!(ct.is_trace_enabled());
        assert_eq!(shell.env_str("ct").as_deref(), Some("42"));
        Ok(())
    }
}
