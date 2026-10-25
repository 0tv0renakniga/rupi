//! Fixed action-to-script dispatch for the harness-owned helper inventory.
//!
//! TASK-010 provides the action mapping and argv-only child launch foundation.
//! TASK-011 adds the versioned one-request/one-response JSON protocol, bounded
//! stream collection, and structured action outcomes. Script behavior, timeout
//! and cancellation policy, environment policy, and audit integration arrive in
//! later tasks.

mod actions;
pub mod protocol;
mod runner;

pub use actions::ActionId;
pub use protocol::{
    MAX_ERROR_CODE_BYTES, MAX_ERROR_MESSAGE_BYTES, MAX_REQUEST_BYTES, MAX_RESPONSE_BYTES,
    MAX_STDERR_BYTES, ProtocolErrorCode, SCRIPT_PROTOCOL_VERSION, ScriptError, ScriptOutcome,
    ScriptOutcomeError, ScriptRequest, ScriptResponse,
};
pub use runner::{RunningScript, ScriptInvocation, ScriptRunner};
