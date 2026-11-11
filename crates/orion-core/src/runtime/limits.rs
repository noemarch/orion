//! What a runtime allows one session to do.
//!
//! The limits live in the daemon, not in the client: a client that asks for more than this is
//! refused however it asks. An agent that starts agents must not be able to fill the machine,
//! so the checks sit next to the thing that starts them rather than in whichever caller
//! remembers to ask.

use std::path::{Component, Path, PathBuf};

/// What a runtime allows one session to do.
///
/// The limits live in the daemon, not in the client: a client that asks for
/// more than this is refused however it asks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RuntimeLimits {
    /// How deep a chain of terminals may go.
    pub max_depth: u32,
    /// How many terminals one terminal may start.
    pub max_children: u32,
    /// How many terminals one session may hold.
    pub max_terminals: u32,
}

impl Default for RuntimeLimits {
    /// The defaults a daemon runs with.
    ///
    /// A root terminal is depth zero, so four levels is the root agent and three rounds of
    /// delegation: enough for real work, small enough that a loop of agents starting agents is a
    /// refusal rather than a fork bomb. Sixteen children and sixty-four terminals cover a wide
    /// workspace on a shared machine without letting one session starve the others.
    fn default() -> Self {
        Self {
            max_depth: 4,
            max_children: 16,
            max_terminals: 64,
        }
    }
}

impl RuntimeLimits {
    /// Whether a terminal at `depth` may exist. A root terminal is depth zero.
    pub fn allows_depth(&self, depth: u32) -> bool {
        depth <= self.max_depth
    }

    /// Whether a terminal that already started `children` may start one more.
    pub fn allows_children(&self, children: u32) -> bool {
        children < self.max_children
    }

    /// Whether a session that already holds `terminals` may start one more.
    pub fn allows_terminals(&self, terminals: u32) -> bool {
        terminals < self.max_terminals
    }
}

/// Whether a child may run in `child_cwd`.
///
/// A child runs inside its parent's directory: an agent that starts another
/// agent works where it works, not anywhere on the machine.
///
/// Both paths are canonicalised first, so `..` and a symlink pointing outside the parent cannot
/// be used to escape. A path that does not exist yet cannot be canonicalised; for those the
/// comparison falls back to a lexical walk over the components, which still resolves `.` and
/// `..` but cannot see through a symlink.
pub fn cwd_is_within(parent_cwd: &Path, child_cwd: &Path) -> bool {
    match (parent_cwd.canonicalize(), child_cwd.canonicalize()) {
        (Ok(parent), Ok(child)) => child.starts_with(parent),
        _ => clean(child_cwd).starts_with(clean(parent_cwd)),
    }
}

/// Resolves `.` and `..` without touching the filesystem.
fn clean(path: &Path) -> PathBuf {
    let mut cleaned = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            // Popping past the root leaves the root, exactly as the OS does.
            Component::ParentDir => {
                cleaned.pop();
            }
            other => cleaned.push(other.as_os_str()),
        }
    }
    cleaned
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_cwd_cannot_escape_with_two_dots() {
        let root = Path::new("work");
        assert!(cwd_is_within(root, &root.join("nested")));
        assert!(!cwd_is_within(root, &root.join("..").join("elsewhere")));
    }
}
