use std::time::Instant;

use crate::usage::TokenStats;

/// Task-level cumulative budget counters (issue #569).
///
/// Unlike the per-segment iteration counter (reset when the user chooses to
/// continue), these counters accumulate for the lifetime of the task across
/// continues, compactions, and sub-agent merges. They are never reset by
/// "continue" — only persisted state or an explicit budget adjustment can
/// change their trajectory.
#[derive(Debug, Default, Clone)]
pub struct TaskBudgetState {
    /// Cumulative tool-loop rounds across the whole task (continues included).
    pub task_rounds: u64,
    /// Cumulative tool executions across the whole task (parallel calls
    /// within one round each count).
    pub tool_calls: u64,
    /// Active wall-clock seconds accumulated while a turn was in flight.
    /// Advanced incrementally (`last_accounted += elapsed`) so continues,
    /// pauses, and resumes never double-count. Idle time is not counted.
    pub active_secs: u64,
    /// Monotonic anchor for wall-clock accounting: the instant from which
    /// active time is being accumulated. `None` while no turn is running.
    pub last_accounted: Option<Instant>,
    /// Cumulative token usage. Cached-prefix reads are excluded from budget
    /// math via `TokenStats::total_tokens()`.
    pub token_usage: TokenStats,
}

impl TaskBudgetState {
    /// Begin (or resume) wall-clock accounting.
    pub fn start_turn(&mut self) {
        self.last_accounted = Some(Instant::now());
    }

    /// Flush pending wall-clock time into `active_secs` using the advancing
    /// anchor pattern: the anchor moves forward by exactly the flushed amount,
    /// so a later flush never double-counts and a pause/continue never resets
    /// the baseline. Whole seconds only — sub-second remainder stays pending.
    pub fn flush_wall_clock(&mut self) {
        if let Some(anchor) = self.last_accounted {
            let elapsed = anchor.elapsed().as_secs();
            if elapsed > 0 {
                self.active_secs += elapsed;
                self.last_accounted = Some(anchor + std::time::Duration::from_secs(elapsed));
            }
        }
    }

    /// Stop wall-clock accounting (turn ended). Flushes any pending whole
    /// seconds first.
    pub fn end_turn(&mut self) {
        self.flush_wall_clock();
        self.last_accounted = None;
    }

    /// Merge a sub-agent's cumulative counters into this parent task.
    /// Each sub-agent reports exactly once on completion, so totals do not
    /// double-count. Wall-clock anchors are not merged — they are process
    /// local.
    pub fn merge_task(&mut self, other: &TaskBudgetState) {
        self.task_rounds += other.task_rounds;
        self.tool_calls += other.tool_calls;
        self.active_secs += other.active_secs;
        self.token_usage.merge_totals(&other.token_usage);
    }
}

/// Near-threshold advisory state (issue #569 acceptance 3 / omp
/// `budgetReportedFor` pattern): the "you are approaching the budget" hint
/// fires once per threshold cycle and is re-armed by an explicit budget
/// adjustment. Tracked on the session, not the persisted counters.
#[derive(Debug, Default, Clone)]
pub struct NearThresholdState {
    /// True once the hint has been shown since the last re-arm.
    pub reported: bool,
}

impl NearThresholdState {
    /// True when usage crossed `NEAR_THRESHOLD_PERCENT` and the hint has
    /// not been shown yet in this cycle. Idempotent: subsequent calls with
    /// usage still above the threshold return false.
    pub fn should_report(&mut self, check: &BudgetCheck) -> bool {
        let crossed = check
            .nearest_percent
            .is_some_and(|pct| pct >= NEAR_THRESHOLD_PERCENT);
        if crossed && !self.reported {
            self.reported = true;
            return true;
        }
        if !crossed {
            // Usage dropped below the threshold (e.g. a limit raise):
            // re-arm so a future crossing reports again.
            self.reported = false;
        }
        false
    }

    /// Explicit re-arm (budget adjustment path).
    pub fn rearm(&mut self) {
        self.reported = false;
    }
}

/// Configurable upper bounds for one task. `None` = unlimited for that
/// dimension. Limits come from `config.yaml` (`llm.task_budget`) and can only
/// be changed through the explicit budget-adjustment flow (never by "continue").
#[derive(Debug, Clone, Default)]
pub struct TaskBudgetLimit {
    pub max_rounds: Option<u64>,
    pub max_tool_calls: Option<u64>,
    pub max_tokens: Option<u64>,
    pub max_duration_secs: Option<u64>,
}

impl TaskBudgetLimit {
    /// True when any dimension is bounded.
    pub fn is_bounded(&self) -> bool {
        self.max_rounds.is_some()
            || self.max_tool_calls.is_some()
            || self.max_tokens.is_some()
            || self.max_duration_secs.is_some()
    }
}

/// Which dimension exhausted, in check order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BudgetDimension {
    Rounds,
    ToolCalls,
    Tokens,
    Duration,
}

impl BudgetDimension {
    pub fn key(self) -> &'static str {
        match self {
            BudgetDimension::Rounds => "rounds",
            BudgetDimension::ToolCalls => "tool_calls",
            BudgetDimension::Tokens => "tokens",
            BudgetDimension::Duration => "duration",
        }
    }
}

/// Result of a budget check: which dimension is over (if any) and how close
/// the task is to the nearest threshold (for near-threshold UI, 0-100).
pub struct BudgetCheck {
    pub exhausted: Option<BudgetDimension>,
    /// Highest usage/threshold ratio across bounded dimensions, scaled to
    /// percent. `None` when nothing is bounded.
    pub nearest_percent: Option<u64>,
}

pub const NEAR_THRESHOLD_PERCENT: u64 = 80;

impl TaskBudgetLimit {
    /// Evaluate the current state against all bounds. Pure computation —
    /// callers decide what to do with exhaustion (stop the loop) or
    /// near-threshold pressure (checkpoint + UI hint).
    pub fn check(&self, state: &TaskBudgetState) -> BudgetCheck {
        // First exhausted dimension in a stable check order.
        let exhausted_dim = [
            (state.task_rounds, self.max_rounds, BudgetDimension::Rounds),
            (
                state.tool_calls,
                self.max_tool_calls,
                BudgetDimension::ToolCalls,
            ),
            (
                state.token_usage.total_tokens(),
                self.max_tokens,
                BudgetDimension::Tokens,
            ),
            (
                state.active_secs,
                self.max_duration_secs,
                BudgetDimension::Duration,
            ),
        ]
        .into_iter()
        .find(|(used, limit, _)| limit.is_some_and(|max| *used >= max))
        .map(|(_, _, dim)| dim);

        // Highest usage/threshold ratio across bounded dimensions.
        let mut nearest: Option<u64> = None;
        for (used, limit) in [
            (state.task_rounds, self.max_rounds),
            (state.tool_calls, self.max_tool_calls),
            (state.token_usage.total_tokens(), self.max_tokens),
            (state.active_secs, self.max_duration_secs),
        ] {
            if let Some(max) = limit {
                if let Some(pct) = used.saturating_mul(100).checked_div(max) {
                    nearest = Some(nearest.map_or(pct, |p: u64| p.max(pct)));
                }
            }
        }

        BudgetCheck {
            exhausted: exhausted_dim,
            nearest_percent: nearest,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn advancing_wall_clock_never_double_counts() {
        let mut state = TaskBudgetState::default();
        state.start_turn();
        state.flush_wall_clock();
        let first = state.active_secs;
        state.flush_wall_clock();
        let second = state.active_secs;
        assert!(second >= first, "flush must advance the counter");
        state.end_turn();
        assert!(state.last_accounted.is_none());
    }

    #[test]
    fn exhaustion_is_detected_per_dimension_in_order() {
        let mut state = TaskBudgetState::default();
        state.task_rounds = 10;
        state.tool_calls = 50;
        let limit = TaskBudgetLimit {
            max_rounds: Some(10),
            max_tool_calls: Some(50),
            max_tokens: None,
            max_duration_secs: None,
        };
        assert_eq!(limit.check(&state).exhausted, Some(BudgetDimension::Rounds));

        state.task_rounds = 9;
        assert_eq!(
            limit.check(&state).exhausted,
            Some(BudgetDimension::ToolCalls)
        );
    }

    #[test]
    fn near_threshold_percent_is_the_max_ratio() {
        let mut state = TaskBudgetState::default();
        state.task_rounds = 10;
        state.tool_calls = 45;
        let limit = TaskBudgetLimit {
            max_rounds: Some(100),
            max_tool_calls: Some(50),
            max_tokens: None,
            max_duration_secs: None,
        };
        let check = limit.check(&state);
        assert_eq!(check.nearest_percent, Some(90));
        assert!(check.exhausted.is_none());
    }

    #[test]
    fn near_threshold_reports_once_then_rearms() {
        let mut nt = NearThresholdState::default();
        let state = TaskBudgetState {
            task_rounds: 85,
            ..Default::default()
        };
        let limit = TaskBudgetLimit {
            max_rounds: Some(100),
            ..Default::default()
        };
        let check = limit.check(&state);
        assert!(nt.should_report(&check), "first crossing reports");
        assert!(
            !nt.should_report(&check),
            "second call in the same cycle is deduplicated"
        );
        nt.rearm();
        assert!(nt.should_report(&check), "rearm allows reporting again");
    }

    #[test]
    fn near_threshold_ignores_unbounded_dimensions() {
        let mut nt = NearThresholdState::default();
        let check = TaskBudgetLimit::default().check(&TaskBudgetState::default());
        assert!(!nt.should_report(&check));
    }

    #[test]
    fn unbounded_limit_never_exhausts() {
        let mut state = TaskBudgetState::default();
        state.task_rounds = u64::MAX;
        let check = TaskBudgetLimit::default().check(&state);
        assert!(check.exhausted.is_none());
        assert!(check.nearest_percent.is_none());
    }
}
