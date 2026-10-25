//! Fixed action-to-script dispatch for the harness-owned helper inventory.
//!
//! TASK-010 provides the action mapping and argv-only child launch foundation.
//! TASK-011 adds the versioned one-request/one-response JSON protocol and
//! bounded stream collection. TASK-012 adds the minimal child environment,
//! hard timeout, cancellation, explicit process cleanup, duration measurement,
//! and bounded audit metadata. Script behavior, action dispatch, timeout/cancel
//! UX, and real helper scripts arrive in later tasks.

mod actions;
pub mod protocol;
mod runner;

pub use actions::ActionId;
pub use protocol::{
    MAX_ERROR_CODE_BYTES, MAX_ERROR_MESSAGE_BYTES, MAX_REQUEST_BYTES, MAX_RESPONSE_BYTES,
    MAX_STDERR_BYTES, ProtocolErrorCode, SCRIPT_PROTOCOL_VERSION, ScriptError, ScriptOutcome,
    ScriptOutcomeError, ScriptRequest, ScriptResponse,
};
pub use runner::{
    DEFAULT_SCRIPT_TIMEOUT, MAX_SCRIPT_TIMEOUT, RunningScript, SCRIPT_ENV_PATH, ScriptInvocation,
    ScriptRunner,
};
