use std::io::Write;

use brush_core::{ExecutionExitCode, ExecutionResult, builtins};
use clap::Parser;

/// Manage shared-memory-backed shell variables.
#[derive(Parser)]
pub(crate) struct SharedCommand {
    /// Delete a shared variable binding and remove its shared value.
    #[arg(short = 'd')]
    delete: bool,

    /// Bind as indexed array.
    #[arg(short = 'a')]
    indexed_array: bool,

    /// Bind as associative array.
    #[arg(short = 'A')]
    assoc_array: bool,

    /// Bind as integer.
    #[arg(short = 'i')]
    integer: bool,

    /// Variable name, or NAME=VALUE for bind+set.
    arg: Option<String>,
}

impl builtins::Command for SharedCommand {
    type Error = brush_core::Error;

    async fn execute<SE: brush_core::ShellExtensions>(
        &self,
        context: brush_core::ExecutionContext<'_, SE>,
    ) -> Result<ExecutionResult, Self::Error> {
        let Some(arg) = &self.arg else {
            writeln!(context.stderr(), "shared: expected NAME or NAME=VALUE")?;
            return Ok(ExecutionExitCode::InvalidUsage.into());
        };

        let mode_count = usize::from(self.delete)
            + usize::from(self.indexed_array)
            + usize::from(self.assoc_array)
            + usize::from(self.integer);
        if mode_count > 1 {
            writeln!(
                context.stderr(),
                "shared: options -d/-a/-A/-i are mutually exclusive"
            )?;
            return Ok(ExecutionExitCode::InvalidUsage.into());
        }

        if self.delete {
            if arg.contains('=') {
                writeln!(context.stderr(), "shared: -d expects a variable name")?;
                return Ok(ExecutionExitCode::InvalidUsage.into());
            }
            let _ = context.shell.shared_delete(arg)?;
            return Ok(ExecutionResult::success());
        }

        if self.indexed_array {
            if arg.contains('=') {
                writeln!(context.stderr(), "shared: -a expects a variable name")?;
                return Ok(ExecutionExitCode::InvalidUsage.into());
            }
            if !brush_core::env::valid_variable_name(arg) {
                writeln!(context.stderr(), "shared: invalid variable name: {arg}")?;
                return Ok(ExecutionExitCode::InvalidUsage.into());
            }
            context.shell.shared_bind_indexed_array(arg)?;
            return Ok(ExecutionResult::success());
        }

        if self.assoc_array {
            if arg.contains('=') {
                writeln!(context.stderr(), "shared: -A expects a variable name")?;
                return Ok(ExecutionExitCode::InvalidUsage.into());
            }
            if !brush_core::env::valid_variable_name(arg) {
                writeln!(context.stderr(), "shared: invalid variable name: {arg}")?;
                return Ok(ExecutionExitCode::InvalidUsage.into());
            }
            context.shell.shared_bind_assoc_array(arg)?;
            return Ok(ExecutionResult::success());
        }

        if self.integer {
            if let Some((name, value)) = arg.split_once('=') {
                if !brush_core::env::valid_variable_name(name) {
                    writeln!(context.stderr(), "shared: invalid variable name: {name}")?;
                    return Ok(ExecutionExitCode::InvalidUsage.into());
                }
                context.shell.shared_bind_integer(name, Some(value))?;
            } else {
                if !brush_core::env::valid_variable_name(arg) {
                    writeln!(context.stderr(), "shared: invalid variable name: {arg}")?;
                    return Ok(ExecutionExitCode::InvalidUsage.into());
                }
                context.shell.shared_bind_integer(arg, None)?;
            }
            return Ok(ExecutionResult::success());
        }

        if let Some((name, value)) = arg.split_once('=') {
            if !brush_core::env::valid_variable_name(name) {
                writeln!(context.stderr(), "shared: invalid variable name: {name}")?;
                return Ok(ExecutionExitCode::InvalidUsage.into());
            }
            context.shell.shared_bind_scalar(name, Some(value))?;
        } else {
            if !brush_core::env::valid_variable_name(arg) {
                writeln!(context.stderr(), "shared: invalid variable name: {arg}")?;
                return Ok(ExecutionExitCode::InvalidUsage.into());
            }
            context.shell.shared_bind_scalar(arg, None)?;
        }

        Ok(ExecutionResult::success())
    }
}
