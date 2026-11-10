//! The seam the Chat Lens talks to an agent through.
//!
//! The lens never touches a PTY. It asks a [`PaneSeam`] to deliver a message,
//! to stop the agent, and to answer the menu the agent is showing, and it asks
//! the same seam what the agent is doing. A live [`Pane`] answers those calls
//! for real; a test answers them with a [`RecordingPane`], so the composer's
//! and the prompt card's behaviour can be observed without an agent process.
//!
//! A message sent here reaches the agent the same way it reaches it from the
//! web client: the Engine's own submit (`frame_dispatch::FrameDispatch::submit`)
//! writes the words and a carriage return, then marks the turn as started.
//! The pane's status is updated locally; the Engine tells the other clients
//! when the agent's own output confirms it.

use orion_core::engine::pane::Pane;
use orion_core::prompt::DetectedPrompt;
use orion_core::protocol::AgentStatus;
use std::sync::Arc;

/// The agent is not there any more: its process is gone.
pub const AGENT_GONE: &str = "agent_gone";

/// The agent's menu moved on: an answer would go to the wrong question.
pub const PROMPT_CHANGED: &str = "prompt_changed";

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
        let mut typed = message.to_string();
        typed.push('\r');
        // A PTY write fails when the agent's process is gone.
        self.write(&typed).map_err(|_| AGENT_GONE.to_string())?;
        // The turn has started: the pane's status says so, and any menu the
        // agent was showing is over.
        self.note_submit();
        Ok(())
    }

    fn stop(&self) -> Result<(), String> {
        Pane::interrupt(self).map_err(|_| AGENT_GONE.to_string())
    }

    fn answer(&self, prompt_id: &str, option_index: usize) -> Result<(), String> {
        Pane::answer_prompt(self, prompt_id, option_index)
    }

    fn answer_typed(&self, prompt_id: &str, text: &str) -> Result<(), String> {
        Pane::answer_prompt_custom(self, prompt_id, text)
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

/// The seam a live pane answers through.
pub fn seam_of(pane: &Arc<Pane>) -> Arc<dyn PaneSeam> {
    let pane: Arc<Pane> = Arc::clone(pane);
    let seam: Arc<dyn PaneSeam> = pane;
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
