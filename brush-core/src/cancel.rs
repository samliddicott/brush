//! Cooperative cancellation token for shell execution contexts.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::{error, results};

/// A clonable token used for cooperative cancellation checks.
#[derive(Clone, Debug, Default)]
pub struct CancellationToken {
    flag: Arc<AtomicBool>,
}

impl CancellationToken {
    /// Create a new uncancelled token.
    pub fn new() -> Self {
        Self::default()
    }

    /// Marks this token as cancelled.
    pub fn cancel(&self) {
        self.flag.store(true, Ordering::SeqCst);
    }

    /// Clears cancellation for this token.
    pub fn reset(&self) {
        self.flag.store(false, Ordering::SeqCst);
    }

    /// Returns whether cancellation was requested.
    pub fn is_cancelled(&self) -> bool {
        self.flag.load(Ordering::SeqCst)
    }

    /// Returns `Err(Interrupted)` if cancellation was requested.
    pub fn check(&self) -> Result<(), error::Error> {
        if self.is_cancelled() {
            Err(error::ErrorKind::Interrupted.into())
        } else {
            Ok(())
        }
    }

    /// Returns a standard interrupted execution result.
    pub const fn interrupted_result() -> results::ExecutionResult {
        results::ExecutionResult {
            next_control_flow: results::ExecutionControlFlow::Normal,
            exit_code: results::ExecutionExitCode::Interrupted,
        }
    }
}

