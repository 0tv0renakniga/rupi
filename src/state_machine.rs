use std::fmt;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::error::{AgentError, Result};

/// Operator-selected workflow and policy bundle for one task.
///
/// A mode may change only between tasks. The orchestrator derives the allowed
/// phase transitions and the model-visible action catalog from this value;
/// model output never selects or upgrades a mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum TaskMode {
    /// Existing implementation workflow with verified mutation.
    #[default]
    Change,
    /// Read-only repository investigation producing an evidence brief.
    Investigate,
}

impl TaskMode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Change => "change",
            Self::Investigate => "investigate",
        }
    }

    pub const fn is_read_only(self) -> bool {
        matches!(self, Self::Investigate)
    }
}

/// The only phases a single task may occupy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LoopState {
    Boot,
    Learn,
    Plan,
    Act,
    Verify,
    Review,
    Compile,
    Completed,
    Blocked,
    Cancelled,
    Failed,
}

impl LoopState {
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Completed | Self::Blocked | Self::Cancelled | Self::Failed
        )
    }

    /// Validate and apply a transition, returning the destination state.
    pub fn transition_to(self, to: Self) -> Result<Self> {
        if transition_allowed(self, to) {
            Ok(to)
        } else {
            Err(AgentError::InvalidTransition { from: self, to })
        }
    }
}

/// Transition table from the authoritative v2 blueprint.
///
/// Cancellation and unrecoverable failure are emergency exits available from
/// every state. No ordinary phase can skip a required gate.
pub fn transition_allowed(from: LoopState, to: LoopState) -> bool {
    use LoopState::*;

    matches!(
        (from, to),
        (Boot, Learn)
            | (Learn, Plan)
            | (Plan, Act)
            | (Plan, Blocked)
            | (Act, Act)
            | (Act, Verify)
            | (Act, Blocked)
            | (Verify, Act)
            | (Verify, Review)
            | (Verify, Blocked)
            | (Review, Act)
            | (Review, Compile)
            | (Review, Verify)
            | (Compile, Completed)
            | (_, Cancelled)
            | (_, Failed)
    )
}

/// Mode-specific transition table.
///
/// `Investigate` never enters Plan, Act, or Verify and therefore cannot reach
/// a mutation or verification gate. Emergency exits remain available from
/// every state.
pub fn transition_allowed_for_mode(mode: TaskMode, from: LoopState, to: LoopState) -> bool {
    use LoopState::*;

    match mode {
        TaskMode::Change => transition_allowed(from, to),
        TaskMode::Investigate => matches!(
            (from, to),
            (Boot, Learn)
                | (Learn, Review)
                | (Learn, Blocked)
                | (Review, Compile)
                | (Review, Blocked)
                | (Compile, Completed)
                | (Compile, Blocked)
                | (_, Cancelled)
                | (_, Failed)
        ),
    }
}

/// Hard limits for one task. Limits are values; consumption is tracked in
/// [`BudgetUsage`] so the orchestrator can persist a trace without mutating
/// configured limits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskBudget {
    pub max_act_turns: usize,
    pub max_tool_calls: usize,
    pub max_verify_retries: u8,
    pub max_system2_calls: usize,
    pub max_wall_time: Duration,
    pub max_tool_output_bytes: usize,
}

impl Default for TaskBudget {
    fn default() -> Self {
        Self {
            max_act_turns: 40,
            max_tool_calls: 120,
            max_verify_retries: 3,
            max_system2_calls: 80,
            max_wall_time: Duration::from_secs(60 * 60),
            max_tool_output_bytes: 4_000,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct BudgetUsage {
    pub act_turns: usize,
    pub tool_calls: usize,
    pub verify_retries: u8,
    pub system2_calls: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BudgetLimit {
    ActTurns,
    ToolCalls,
    VerifyRetries,
    System2Calls,
    WallTime,
    ToolOutputBytes,
}

impl fmt::Display for BudgetLimit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Self::ActTurns => "act turns",
            Self::ToolCalls => "tool calls",
            Self::VerifyRetries => "verification retries",
            Self::System2Calls => "System 2 calls",
            Self::WallTime => "wall time",
            Self::ToolOutputBytes => "tool output bytes",
        };
        f.write_str(name)
    }
}

impl std::error::Error for BudgetLimit {}

impl TaskBudget {
    /// First exhausted dimension in deterministic order.
    pub fn check(&self, usage: &BudgetUsage, elapsed: Duration) -> Option<BudgetLimit> {
        if usage.act_turns >= self.max_act_turns {
            Some(BudgetLimit::ActTurns)
        } else if usage.tool_calls >= self.max_tool_calls {
            Some(BudgetLimit::ToolCalls)
        } else if usage.verify_retries >= self.max_verify_retries {
            Some(BudgetLimit::VerifyRetries)
        } else if usage.system2_calls >= self.max_system2_calls {
            Some(BudgetLimit::System2Calls)
        } else if elapsed >= self.max_wall_time {
            Some(BudgetLimit::WallTime)
        } else if self.max_tool_output_bytes == 0 {
            Some(BudgetLimit::ToolOutputBytes)
        } else {
            None
        }
    }

    pub fn try_begin_act_turn(
        &self,
        usage: &mut BudgetUsage,
        elapsed: Duration,
    ) -> std::result::Result<(), BudgetLimit> {
        self.check_wall_time(elapsed)?;
        if usage.act_turns >= self.max_act_turns {
            return Err(BudgetLimit::ActTurns);
        }
        usage.act_turns += 1;
        Ok(())
    }

    pub fn try_record_tool_call(
        &self,
        usage: &mut BudgetUsage,
        elapsed: Duration,
    ) -> std::result::Result<(), BudgetLimit> {
        self.check_wall_time(elapsed)?;
        if usage.tool_calls >= self.max_tool_calls {
            return Err(BudgetLimit::ToolCalls);
        }
        usage.tool_calls += 1;
        Ok(())
    }

    pub fn try_record_verify_retry(
        &self,
        usage: &mut BudgetUsage,
        elapsed: Duration,
    ) -> std::result::Result<(), BudgetLimit> {
        self.check_wall_time(elapsed)?;
        if usage.verify_retries >= self.max_verify_retries {
            return Err(BudgetLimit::VerifyRetries);
        }
        usage.verify_retries += 1;
        Ok(())
    }

    pub fn try_record_system2_call(
        &self,
        usage: &mut BudgetUsage,
        elapsed: Duration,
    ) -> std::result::Result<(), BudgetLimit> {
        self.check_wall_time(elapsed)?;
        if usage.system2_calls >= self.max_system2_calls {
            return Err(BudgetLimit::System2Calls);
        }
        usage.system2_calls += 1;
        Ok(())
    }

    pub fn check_wall_time(&self, elapsed: Duration) -> std::result::Result<(), BudgetLimit> {
        if elapsed >= self.max_wall_time {
            Err(BudgetLimit::WallTime)
        } else {
            Ok(())
        }
    }

    /// Head-and-tail truncation that never splits a UTF-8 code point.
    pub fn bound_tool_output(&self, output: &str) -> String {
        let cap = self.max_tool_output_bytes;
        if output.len() <= cap {
            return output.to_owned();
        }

        let half = cap / 2;
        let mut head = half.min(output.len());
        while head > 0 && !output.is_char_boundary(head) {
            head -= 1;
        }

        let mut tail_start = output.len().saturating_sub(half);
        while tail_start < output.len() && !output.is_char_boundary(tail_start) {
            tail_start += 1;
        }

        format!(
            "{}\n...[{} bytes omitted]...\n{}",
            &output[..head],
            tail_start.saturating_sub(head),
            &output[tail_start..]
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_the_blueprint_transition_path() {
        let path = [
            LoopState::Boot,
            LoopState::Learn,
            LoopState::Plan,
            LoopState::Act,
            LoopState::Verify,
            LoopState::Review,
            LoopState::Compile,
            LoopState::Completed,
        ];

        for pair in path.windows(2) {
            assert!(
                transition_allowed(pair[0], pair[1]),
                "{:?} -> {:?} should be allowed",
                pair[0],
                pair[1]
            );
        }
    }

    #[test]
    fn rejects_transitions_that_skip_gates() {
        let invalid = [
            (LoopState::Boot, LoopState::Act),
            (LoopState::Learn, LoopState::Verify),
            (LoopState::Plan, LoopState::Verify),
            (LoopState::Compile, LoopState::Verify),
            (LoopState::Completed, LoopState::Learn),
            (LoopState::Blocked, LoopState::Plan),
        ];

        for (from, to) in invalid {
            assert!(
                !transition_allowed(from, to),
                "{from:?} -> {to:?} should be rejected"
            );
        }
    }

    #[test]
    fn investigate_mode_uses_the_read_only_path_only() {
        let path = [
            LoopState::Boot,
            LoopState::Learn,
            LoopState::Review,
            LoopState::Compile,
            LoopState::Completed,
        ];

        for pair in path.windows(2) {
            assert!(
                transition_allowed_for_mode(TaskMode::Investigate, pair[0], pair[1]),
                "{:?} -> {:?} should be allowed in Investigate",
                pair[0],
                pair[1]
            );
        }

        for forbidden in [LoopState::Plan, LoopState::Act, LoopState::Verify] {
            assert!(!transition_allowed_for_mode(
                TaskMode::Investigate,
                LoopState::Learn,
                forbidden
            ));
        }

        assert!(transition_allowed_for_mode(
            TaskMode::Investigate,
            LoopState::Learn,
            LoopState::Blocked
        ));
        assert!(transition_allowed_for_mode(
            TaskMode::Investigate,
            LoopState::Review,
            LoopState::Blocked
        ));
        assert!(transition_allowed_for_mode(
            TaskMode::Investigate,
            LoopState::Review,
            LoopState::Cancelled
        ));
        assert_eq!(TaskMode::default(), TaskMode::Change);
        assert_eq!(TaskMode::Investigate.as_str(), "investigate");
        assert!(TaskMode::Investigate.is_read_only());
        assert!(!TaskMode::Change.is_read_only());
    }

    #[test]
    fn terminal_states_are_reported_and_ordinary_phases_are_not() {
        for state in [
            LoopState::Completed,
            LoopState::Blocked,
            LoopState::Cancelled,
            LoopState::Failed,
        ] {
            assert!(state.is_terminal());
        }

        for state in [
            LoopState::Boot,
            LoopState::Learn,
            LoopState::Plan,
            LoopState::Act,
            LoopState::Verify,
            LoopState::Review,
            LoopState::Compile,
        ] {
            assert!(!state.is_terminal());
        }
    }

    #[test]
    fn transition_to_returns_a_structured_error() {
        let error = LoopState::Completed
            .transition_to(LoopState::Plan)
            .unwrap_err();
        assert!(matches!(
            error,
            AgentError::InvalidTransition {
                from: LoopState::Completed,
                to: LoopState::Plan
            }
        ));
        assert_eq!(
            LoopState::Plan.transition_to(LoopState::Act).unwrap(),
            LoopState::Act
        );
    }

    #[test]
    fn act_turn_budget_is_exhausted() {
        let budget = TaskBudget {
            max_act_turns: 2,
            ..TaskBudget::default()
        };
        let mut usage = BudgetUsage::default();
        let now = Duration::ZERO;

        assert_eq!(budget.try_begin_act_turn(&mut usage, now), Ok(()));
        assert_eq!(budget.try_begin_act_turn(&mut usage, now), Ok(()));
        assert_eq!(
            budget.try_begin_act_turn(&mut usage, now),
            Err(BudgetLimit::ActTurns)
        );
        assert_eq!(usage.act_turns, 2);
    }

    #[test]
    fn tool_call_and_verification_budgets_are_exhausted() {
        let budget = TaskBudget {
            max_tool_calls: 1,
            max_verify_retries: 1,
            ..TaskBudget::default()
        };
        let mut usage = BudgetUsage::default();
        let now = Duration::ZERO;

        assert_eq!(budget.try_record_tool_call(&mut usage, now), Ok(()));
        assert_eq!(
            budget.try_record_tool_call(&mut usage, now),
            Err(BudgetLimit::ToolCalls)
        );

        assert_eq!(budget.try_record_verify_retry(&mut usage, now), Ok(()));
        assert_eq!(
            budget.try_record_verify_retry(&mut usage, now),
            Err(BudgetLimit::VerifyRetries)
        );
    }

    #[test]
    fn wall_time_and_system2_budgets_are_exhausted() {
        let budget = TaskBudget {
            max_system2_calls: 1,
            max_wall_time: Duration::from_secs(10),
            ..TaskBudget::default()
        };
        let mut usage = BudgetUsage::default();

        assert_eq!(
            budget.try_record_system2_call(&mut usage, Duration::from_secs(1)),
            Ok(())
        );
        assert_eq!(
            budget.try_record_system2_call(&mut usage, Duration::from_secs(1)),
            Err(BudgetLimit::System2Calls)
        );
        assert_eq!(
            budget.try_begin_act_turn(&mut usage, Duration::from_secs(10)),
            Err(BudgetLimit::WallTime)
        );
        assert_eq!(
            budget.check_wall_time(Duration::from_secs(11)),
            Err(BudgetLimit::WallTime)
        );
    }

    #[test]
    fn tool_output_is_bounded_without_breaking_utf8() {
        let budget = TaskBudget {
            max_tool_output_bytes: 12,
            ..TaskBudget::default()
        };
        let output = "éééééééééééééééééééé";
        let bounded = budget.bound_tool_output(output);

        assert!(bounded.len() > output.len() || bounded.contains("bytes omitted"));
        assert!(bounded.is_char_boundary(bounded.len()));
        assert!(bounded.starts_with("éé"));
    }
}
