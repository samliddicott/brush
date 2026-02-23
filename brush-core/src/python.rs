//! Python runtime context attached to each shell context.

use std::collections::HashMap;

/// Placeholder globals store for Stage-1 wiring.
///
/// This will be replaced or wrapped with PyO3-owned globals in follow-up steps.
pub type PythonGlobals = HashMap<String, String>;

/// Python configuration and namespace state attached to a shell context.
#[derive(Clone, Debug, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct PythonContext {
    /// Per-shell Python globals namespace placeholder.
    pub globals: PythonGlobals,
    /// Enables command-not-found fallback for dotted names.
    pub implicit_dotted_dispatch: bool,
    /// Enables shell-argument auto-conversion for callable dispatch.
    pub auto_convert_args: bool,
}

impl PythonContext {
    /// Create context defaults for interactive or non-interactive mode.
    pub fn defaults_for_interactive(interactive: bool) -> Self {
        Self {
            globals: PythonGlobals::default(),
            implicit_dotted_dispatch: interactive,
            auto_convert_args: true,
        }
    }
}
