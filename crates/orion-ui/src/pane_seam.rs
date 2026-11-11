//! The seam the Chat Lens talks to an agent through.
//!
//! The lens never touches a PTY. It asks a [`PaneSeam`] to deliver a message,
//! to stop the agent, and to answer the menu the agent is showing, and it asks
//! the same seam what the agent is doing. A live pane answers those calls for
//! real; a test answers them with a [`RecordingPane`], so the composer's and
//! the prompt card's behaviour can be observed without an agent process.
//!
//! The seam speaks to a [`PaneHandle`], not to a process: the pane may run in
//! this process or in the daemon, and the lens has one code path either way.
//! A message sent here reaches the agent the same way it reaches it from the
//! web client: the words and a carriage return go down the pane's stdin.
//! The pane's own status is updated when the agent's output confirms the turn.

use orion_core::engine::pane::Pane;
use orion_core::prompt::DetectedPrompt;
use orion_core::protocol::AgentStatus;
use orion_core::runtime::PaneHandle;
use std::sync::Arc;

/// The agent is not there any more: its process is gone.
pub const AGENT_GONE: &str = "agent_gone";

/// The agent's menu moved on: an answer would go to the wrong question.
pub const PROMPT_CHANGED: &str = "prompt_changed";

/// The agent's name is not on the pane handle: the runtime's session snapshot
/// names every pane, so the Chat Lens asks the runtime for it.
pub const AGENT_UNKNOWN: &str = "Agent";

/// What the Chat Lens may ask an agent to do.
pub trait PaneSeam: Send + Sync {
    /// Delivers a message to the agent, the way the agent's own submit does.
    fn submit(&self, message: &str) -> Result<(), String>;
    /// Interrupts the agent's current turn.
    fn stop(&self) -> Result<(), String>;
    /// Answers the agent's on-screen menu with one of its options.
    fn answer(&self, prompt_id: &str, option_index: usize) -> Result<(), String>;
    /// Answers the agent's on-screen menu with a typed line.
    fn answer_typed(&self, prompt_id: &str, text: &str) -> Result<(), String>;
    /// The menu the agent is showing right now, if it is showing one.
    fn prompt(&self) -> Option<DetectedPrompt>;
    /// Whether the agent is working right now.
    fn is_working(&self) -> bool;
    /// The agent's status.
    fn status(&self) -> AgentStatus;
    /// The agent's name, for the role label on a turn.
    fn agent_label(&self) -> String;
}

impl PaneSeam for Pane {
    fn submit(&self, message: &str) -> Result<(), String> {
        submit_over(self, message)
    }

    fn stop(&self) -> Result<(), String> {
        stop_over(self)
    }

    fn answer(&self, prompt_id: &str, option_index: usize) -> Result<(), String> {
        answer_over(self, prompt_id, option_index, None)
    }

    fn answer_typed(&self, prompt_id: &str, text: &str) -> Result<(), String> {
        answer_over(self, prompt_id, 0, Some(text))
    }

    fn prompt(&self) -> Option<DetectedPrompt> {
        Pane::prompt(self)
    }

    fn is_working(&self) -> bool {
        self.status() == AgentStatus::Working
    }

    fn status(&self) -> AgentStatus {
        self.status()
    }

    fn agent_label(&self) -> String {
        self.agent.display_name().to_string()
    }
}

/// The seam over a pane that answers the runtime's questions.
///
/// The pane is a handle: it may live in this process or in the daemon, and the
/// lens drives it the same way either way.
struct PaneSeamOver {
    pane: Arc<dyn PaneHandle>,
}

impl PaneSeam for PaneSeamOver {
    fn submit(&self, message: &str) -> Result<(), String> {
        submit_over(&*self.pane, message)
    }

    fn stop(&self) -> Result<(), String> {
        stop_over(&*self.pane)
    }

    fn answer(&self, prompt_id: &str, option_index: usize) -> Result<(), String> {
        answer_over(&*self.pane, prompt_id, option_index, None)
    }

    fn answer_typed(&self, prompt_id: &str, text: &str) -> Result<(), String> {
        answer_over(&*self.pane, prompt_id, 0, Some(text))
    }

    fn prompt(&self) -> Option<DetectedPrompt> {
        self.pane.prompt()
    }

    fn is_working(&self) -> bool {
        self.pane.status() == AgentStatus::Working
    }

    fn status(&self) -> AgentStatus {
        self.pane.status()
    }

    fn agent_label(&self) -> String {
        // A pane handle does not name its agent: the runtime's session
        // snapshot does, and the Chat Lens asks the runtime for it.
        AGENT_UNKNOWN.to_string()
    }
}

/// Delivers a message to the agent, the way the agent's own submit does: the
/// words and a carriage return go down the pane's stdin.
fn submit_over(pane: &dyn PaneHandle, message: &str) -> Result<(), String> {
    let mut typed = message.to_string();
    typed.push('\r');
    // A PTY write fails when the agent's process is gone.
    pane.write(&typed).map_err(|_| AGENT_GONE.to_string())
}

/// Interrupts the agent's current turn.
fn stop_over(pane: &dyn PaneHandle) -> Result<(), String> {
    pane.interrupt().map_err(|_| AGENT_GONE.to_string())
}

/// Answers the menu the agent is showing.
///
/// The answer is validated against the menu the pane shows right now: a card
/// that names a menu the pane no longer shows is refused as `prompt_changed`
/// rather than typing a choice into whatever came next.
fn answer_over(
    pane: &dyn PaneHandle,
    prompt_id: &str,
    option_index: usize,
    custom: Option<&str>,
) -> Result<(), String> {
    let Some(current) = pane.prompt() else {
        return Err(PROMPT_CHANGED.to_string());
    };
    if current.id != prompt_id {
        return Err(PROMPT_CHANGED.to_string());
    }
    let keys = match custom {
        Some(text) => current.answer_custom_keys(text),
        None => current.answer_keys(option_index),
    }
    .ok_or_else(|| "invalid_option".to_string())?;
    pane.write(&keys).map_err(|_| AGENT_GONE.to_string())
}

/// The seam a live pane answers through.
pub fn seam_of(pane: &Arc<dyn PaneHandle>) -> Arc<dyn PaneSeam> {
    let seam: Arc<dyn PaneSeam> = Arc::new(PaneSeamOver {
        pane: Arc::clone(pane),
    });
    seam
}

#[cfg(test)]
pub mod test_doubles {
    //! A pane that records what it was asked to do.

    use super::PaneSeam;
    use orion_core::prompt::{DetectedPrompt, MenuOption};
    use orion_core::protocol::AgentStatus;
    use std::sync::Mutex;

    /// What the lens asked the agent to do.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum SeamCall {
        /// A message was delivered.
        Submit(String),
        /// The agent was interrupted.
        Stop,
        /// A menu option was chosen.
        Answer(String, usize),
        /// A menu was answered with a typed line.
        AnswerTyped(String, String),
    }

    /// A seam that remembers every call, and can be told what the agent is
    /// doing.
    #[derive(Debug, Default)]
    pub struct RecordingPane {
        calls: Mutex<Vec<SeamCall>>,
        /// When set, every call fails with this message.
        failure: Mutex<Option<String>>,
        /// The menu the agent is showing, if any.
        prompt: Mutex<Option<DetectedPrompt>>,
        /// Whether the agent is working.
        working: Mutex<bool>,
    }

    impl RecordingPane {
        /// A pane that answers every call.
        pub fn new() -> Self {
            Self::default()
        }

        /// Every call the lens made, in order.
        pub fn calls(&self) -> Vec<SeamCall> {
            self.calls
                .lock()
                .expect("the call log is not poisoned")
                .clone()
        }

        /// The messages the lens delivered.
        pub fn submitted(&self) -> Vec<String> {
            self.calls()
                .into_iter()
                .filter_map(|call| match call {
                    SeamCall::Submit(message) => Some(message),
                    _ => None,
                })
                .collect()
        }

        /// Make every call fail with this message.
        pub fn fail_with(&self, message: &str) {
            *self.failure.lock().expect("the failure is not poisoned") = Some(message.to_string());
        }

        /// Show a menu on the agent's screen.
        pub fn show_prompt(&self, prompt: DetectedPrompt) {
            *self.prompt.lock().expect("the prompt is not poisoned") = Some(prompt);
        }

        /// Take the menu off the agent's screen.
        pub fn clear_prompt(&self) {
            *self.prompt.lock().expect("the prompt is not poisoned") = None;
        }

        /// Say whether the agent is working.
        pub fn set_working(&self, working: bool) {
            *self
                .working
                .lock()
                .expect("the working flag is not poisoned") = working;
        }
    }

    /// A menu an agent is showing: two options and a question.
    pub fn menu_prompt() -> DetectedPrompt {
        DetectedPrompt {
            id: "p1".to_string(),
            agent: "claude".to_string(),
            kind: "approval",
            title: "Approve?".to_string(),
            question: "Run the command?".to_string(),
            body: None,
            options: vec![
                MenuOption {
                    number: 1,
                    label: "Yes".to_string(),
                    description: None,
                    selected: true,
                },
                MenuOption {
                    number: 2,
                    label: "No".to_string(),
                    description: None,
                    selected: false,
                },
            ],
            custom_option_index: None,
            queued: None,
            row_count: 2,
            selected_row: Some(0),
        }
    }

    impl PaneSeam for RecordingPane {
        fn submit(&self, message: &str) -> Result<(), String> {
            self.calls
                .lock()
                .expect("the call log is not poisoned")
                .push(SeamCall::Submit(message.to_string()));
            match self
                .failure
                .lock()
                .expect("the failure is not poisoned")
                .clone()
            {
                Some(message) => Err(message),
                None => Ok(()),
            }
        }

        fn stop(&self) -> Result<(), String> {
            self.calls
                .lock()
                .expect("the call log is not poisoned")
                .push(SeamCall::Stop);
            match self
                .failure
                .lock()
                .expect("the failure is not poisoned")
                .clone()
            {
                Some(message) => Err(message),
                None => Ok(()),
            }
        }

        fn answer(&self, prompt_id: &str, option_index: usize) -> Result<(), String> {
            self.calls
                .lock()
                .expect("the call log is not poisoned")
                .push(SeamCall::Answer(prompt_id.to_string(), option_index));
            match self
                .failure
                .lock()
                .expect("the failure is not poisoned")
                .clone()
            {
                Some(message) => Err(message),
                None => {
                    // The answer went out: the agent's menu is over.
                    self.clear_prompt();
                    Ok(())
                }
            }
        }

        fn answer_typed(&self, prompt_id: &str, text: &str) -> Result<(), String> {
            self.calls
                .lock()
                .expect("the call log is not poisoned")
                .push(SeamCall::AnswerTyped(
                    prompt_id.to_string(),
                    text.to_string(),
                ));
            match self
                .failure
                .lock()
                .expect("the failure is not poisoned")
                .clone()
            {
                Some(message) => Err(message),
                None => {
                    // The answer went out: the agent's menu is over.
                    self.clear_prompt();
                    Ok(())
                }
            }
        }

        fn prompt(&self) -> Option<DetectedPrompt> {
            self.prompt
                .lock()
                .expect("the prompt is not poisoned")
                .clone()
        }

        fn is_working(&self) -> bool {
            *self
                .working
                .lock()
                .expect("the working flag is not poisoned")
        }

        fn status(&self) -> AgentStatus {
            if self.is_working() {
                AgentStatus::Working
            } else {
                AgentStatus::Idle
            }
        }

        fn agent_label(&self) -> String {
            "Claude".to_string()
        }
    }
}
