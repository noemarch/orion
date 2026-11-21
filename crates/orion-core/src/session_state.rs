//! The single session-state model shared by the engine and the shell.
//!
//! `AgentStatus` is the wire vocabulary; `SessionState` is the one lifecycle
//! model the sidebar badge, the Working/Settled groups and the terminal footer
//! all read from. Keeping the derivation here — pure, with no GPUI and no I/O —
//! makes the three views agree by construction instead of by convention.

use crate::protocol::AgentStatus;

/// The one lifecycle state of a session row. Exactly five variants.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionState {
    Starting,
    Running,
    NeedsInput,
    Idle,
    Exited { code: i32 },
}

impl SessionState {
    /// Derive the single state from the wire status and the pane's exit code.
    ///
    /// An exit code is authoritative: a pane that has exited is `Exited` even
    /// if its last reported status had not caught up. Without a code, `Done`
    /// is a clean exit and `Error` is a failed one. `Unknown` becomes
    /// `Starting`: an unreported pane is never silently folded into `Idle`.
    pub fn from_agent_status(status: AgentStatus, exit_code: Option<i32>) -> Self {
        if let Some(code) = exit_code {
            return Self::Exited { code };
        }
        match status {
            AgentStatus::Idle => Self::Idle,
            AgentStatus::Working => Self::Running,
            AgentStatus::Blocked => Self::NeedsInput,
            AgentStatus::Done => Self::Exited { code: 0 },
            AgentStatus::Error => Self::Exited { code: 1 },
            AgentStatus::Unknown => Self::Starting,
        }
    }

    /// The one written word shared by the sidebar badge and the footer.
    pub fn status_word(self) -> &'static str {
        match self {
            Self::Starting => "Starting",
            Self::Running => "Working",
            Self::NeedsInput => "Waiting",
            Self::Idle => "Idle",
            Self::Exited { code: 0 } => "Done",
            Self::Exited { .. } => "Error",
        }
    }

    /// Whether this state belongs to the Working group.
    pub fn is_working(self) -> bool {
        matches!(self, Self::Starting | Self::Running | Self::NeedsInput)
    }

    /// Whether this state belongs to the Settled group.
    pub fn is_settled(self) -> bool {
        matches!(self, Self::Idle | Self::Exited { .. })
    }
}

fn priority(state: SessionState) -> u8 {
    match state {
        SessionState::Exited { .. } => 0,
        SessionState::Idle => 1,
        SessionState::Starting => 2,
        SessionState::Running => 3,
        SessionState::NeedsInput => 4,
    }
}

/// The most urgent state wins: NeedsInput > Running > Starting > Idle > Exited.
/// An empty slice is `Idle`; ties keep the first state supplied.
pub fn aggregate(states: &[SessionState]) -> SessionState {
    let mut best: Option<SessionState> = None;
    for &state in states {
        match best {
            None => best = Some(state),
            Some(current) if priority(state) > priority(current) => best = Some(state),
            _ => {}
        }
    }
    best.unwrap_or(SessionState::Idle)
}

/// How many states in this slice belong to the Working group.
pub fn working_count(states: &[SessionState]) -> usize {
    states.iter().filter(|state| state.is_working()).count()
}

/// How many states in this slice belong to the Settled group.
pub fn settled_count(states: &[SessionState]) -> usize {
    states.iter().filter(|state| state.is_settled()).count()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn five_variants_are_distinct() {
        let all = [
            SessionState::Starting,
            SessionState::Running,
            SessionState::NeedsInput,
            SessionState::Idle,
            SessionState::Exited { code: 0 },
        ];
        for (i, a) in all.iter().enumerate() {
            for (j, b) in all.iter().enumerate() {
                assert_eq!(
                    a == b,
                    i == j,
                    "variants {i} and {j} must compare equal only when they are the same"
                );
            }
        }
        assert_ne!(
            SessionState::Exited { code: 0 },
            SessionState::Exited { code: 1 }
        );
    }

    #[test]
    fn exit_code_is_preserved() {
        assert_eq!(
            SessionState::from_agent_status(AgentStatus::Done, Some(7)),
            SessionState::Exited { code: 7 }
        );
        assert_eq!(
            SessionState::from_agent_status(AgentStatus::Working, Some(-3)),
            SessionState::Exited { code: -3 }
        );
    }

    #[test]
    fn error_maps_to_exited_one() {
        assert_eq!(
            SessionState::from_agent_status(AgentStatus::Error, None),
            SessionState::Exited { code: 1 }
        );
    }

    #[test]
    fn unknown_maps_to_starting() {
        assert_eq!(
            SessionState::from_agent_status(AgentStatus::Unknown, None),
            SessionState::Starting
        );
    }

    #[test]
    fn every_wire_status_maps_to_one_of_the_five_variants() {
        assert_eq!(
            SessionState::from_agent_status(AgentStatus::Idle, None),
            SessionState::Idle
        );
        assert_eq!(
            SessionState::from_agent_status(AgentStatus::Working, None),
            SessionState::Running
        );
        assert_eq!(
            SessionState::from_agent_status(AgentStatus::Blocked, None),
            SessionState::NeedsInput
        );
        assert_eq!(
            SessionState::from_agent_status(AgentStatus::Done, None),
            SessionState::Exited { code: 0 }
        );
    }

    #[test]
    fn aggregate_priority_needs_input_running_starting_idle_exited() {
        use SessionState::*;
        assert_eq!(aggregate(&[]), Idle);
        assert_eq!(aggregate(&[Exited { code: 1 }]), Exited { code: 1 });
        assert_eq!(aggregate(&[Idle, Exited { code: 0 }]), Idle);
        assert_eq!(aggregate(&[Starting, Idle]), Starting);
        assert_eq!(aggregate(&[Running, Starting]), Running);
        assert_eq!(aggregate(&[NeedsInput, Running]), NeedsInput);
        assert_eq!(aggregate(&[Exited { code: 1 }, Running]), Running);
        assert_eq!(aggregate(&[NeedsInput, Exited { code: 0 }]), NeedsInput);
    }

    #[test]
    fn aggregate_does_not_fold_error_or_unknown_into_idle() {
        let error = SessionState::from_agent_status(AgentStatus::Error, None);
        let unknown = SessionState::from_agent_status(AgentStatus::Unknown, None);
        assert_eq!(error, SessionState::Exited { code: 1 });
        assert_eq!(unknown, SessionState::Starting);
        assert_eq!(aggregate(&[error]), SessionState::Exited { code: 1 });
        assert_eq!(aggregate(&[unknown]), SessionState::Starting);
        assert_eq!(aggregate(&[error, unknown]), SessionState::Starting);
    }

    #[test]
    fn counters_come_from_one_slice() {
        let states = [
            SessionState::Starting,
            SessionState::Running,
            SessionState::NeedsInput,
            SessionState::Idle,
            SessionState::Exited { code: 0 },
            SessionState::Exited { code: 1 },
        ];
        assert_eq!(working_count(&states), 3);
        assert_eq!(settled_count(&states), 3);
        assert_eq!(
            working_count(&states) + settled_count(&states),
            states.len()
        );
        assert_eq!(working_count(&[]), 0);
        assert_eq!(settled_count(&[]), 0);
    }

    #[test]
    fn status_word_is_the_shared_vocabulary() {
        assert_eq!(SessionState::Starting.status_word(), "Starting");
        assert_eq!(SessionState::Running.status_word(), "Working");
        assert_eq!(SessionState::NeedsInput.status_word(), "Waiting");
        assert_eq!(SessionState::Idle.status_word(), "Idle");
        assert_eq!(SessionState::Exited { code: 0 }.status_word(), "Done");
        assert_eq!(SessionState::Exited { code: 1 }.status_word(), "Error");
    }

    #[test]
    fn working_and_settled_are_complementary() {
        for state in [
            SessionState::Starting,
            SessionState::Running,
            SessionState::NeedsInput,
            SessionState::Idle,
            SessionState::Exited { code: 0 },
            SessionState::Exited { code: 9 },
        ] {
            assert_ne!(
                state.is_working(),
                state.is_settled(),
                "{state:?} must belong to exactly one group"
            );
        }
    }
}
