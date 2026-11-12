//! Pure terminal tree projection for the runtime's terminals.
//!
//! The daemon reports its terminals as a flat list with parent ids; the sidebar
//! and the chat view both draw that list as a tree. The ordering and the
//! "a terminal that has not reported gets no badge" rule live here once, so the
//! two views cannot disagree about them.

use orion_core::protocol::ReportedStatus;
use serde_json::Value;
use std::collections::{HashMap, HashSet};

/// One row of the terminal tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalRow {
    /// The terminal's id.
    pub id: String,
    /// The human name, if it has one.
    pub name: Option<String>,
    /// The id of the terminal that started this one, if any.
    pub parent_id: Option<String>,
    /// What the program reported about itself, if it reported.
    pub status: Option<String>,
    /// The exit code, if the program has ended.
    pub exited: Option<i32>,
    /// The directory the program runs in.
    pub cwd: String,
    /// The command the program runs.
    pub program: String,
    /// How deep the terminal sits in the tree. A terminal with no parent is 0.
    pub depth: usize,
    /// Whether the row has children of its own.
    pub has_children: bool,
}

/// Where the sidebar and the chat view get the runtime's terminals.
///
/// The runtime lives in another process; the shell asks it for the list the
/// same way it asks the engine for the session.
pub trait TerminalSource: Send + Sync {
    /// The runtime's terminals, as the daemon reports them.
    fn terminals(&self) -> Option<Value>;
}

/// The runtime's terminals, as a tree: parents first, then their children.
///
/// `terminals` is the JSON the daemon sent: either the whole answer
/// `{"terminals": [...]}` or the bare array. A terminal whose parent is not in
/// the list is a root: the daemon may have forgotten the parent, and the
/// terminal is still there.
pub fn terminal_rows(terminals: &Value) -> Vec<TerminalRow> {
    let nodes: Vec<Node> = terminal_array(terminals)
        .iter()
        .filter_map(Node::parse)
        .collect();

    let mut by_id: HashMap<&str, usize> = HashMap::new();
    for (index, node) in nodes.iter().enumerate() {
        by_id.entry(node.id.as_str()).or_insert(index);
    }

    let mut children: HashMap<&str, Vec<usize>> = HashMap::new();
    let mut has_children: HashSet<&str> = HashSet::new();
    let mut roots: Vec<usize> = Vec::new();
    for (index, node) in nodes.iter().enumerate() {
        match node.parent_id.as_deref() {
            Some(parent) if by_id.contains_key(parent) => {
                children.entry(parent).or_default().push(index);
                has_children.insert(parent);
            }
            _ => roots.push(index),
        }
    }

    let mut tree = Tree {
        nodes: &nodes,
        children,
        has_children,
        placed: vec![false; nodes.len()],
        rows: Vec::with_capacity(nodes.len()),
    };
    for &root in &roots {
        tree.walk(root);
    }
    // Anything still unplaced sits in a parent cycle: it has no root, so the
    // first one in the daemon's order is treated as one.
    for index in 0..nodes.len() {
        tree.walk(index);
    }
    tree.rows
}

/// The token name a reported status paints with.
///
/// A terminal that has not reported has no badge: the reader cannot tell
/// what it is doing, so the shell does not guess.
pub fn status_token(status: Option<&str>) -> Option<&'static str> {
    ReportedStatus::from_wire(status?).map(ReportedStatus::as_wire)
}

/// One terminal as the daemon listed it.
struct Node {
    id: String,
    name: Option<String>,
    parent_id: Option<String>,
    status: Option<String>,
    exited: Option<i32>,
    cwd: String,
    program: String,
}

impl Node {
    /// A terminal the daemon sent, or `None` when it has no id to key it by.
    fn parse(value: &Value) -> Option<Self> {
        Some(Self {
            id: str_field(value, "id")?.to_string(),
            name: str_field(value, "name").map(str::to_string),
            parent_id: str_field(value, "parent_id").map(str::to_string),
            status: status_field(value),
            exited: value
                .get("exited")
                .and_then(Value::as_i64)
                .and_then(|code| i32::try_from(code).ok()),
            cwd: str_field(value, "cwd").unwrap_or("").to_string(),
            program: str_field(value, "program").unwrap_or("").to_string(),
        })
    }
}

/// The list to read, from either shape the caller may hold.
fn terminal_array(terminals: &Value) -> &[Value] {
    if let Some(array) = terminals.as_array() {
        return array;
    }
    match terminals.get("terminals").and_then(Value::as_array) {
        Some(array) => array,
        None => &[],
    }
}

fn str_field<'a>(value: &'a Value, field: &str) -> Option<&'a str> {
    value.get(field).and_then(Value::as_str)
}

/// A reported status is one of the wire names, or absent. An unknown string is
/// not a status this shell can paint, so it is dropped.
fn status_field(value: &Value) -> Option<String> {
    let raw = str_field(value, "status")?;
    ReportedStatus::from_wire(raw).map(|status| status.as_wire().to_string())
}

/// The rows being built, with the parent index each child hangs off.
struct Tree<'a> {
    nodes: &'a [Node],
    children: HashMap<&'a str, Vec<usize>>,
    has_children: HashSet<&'a str>,
    placed: Vec<bool>,
    rows: Vec<TerminalRow>,
}

impl Tree<'_> {
    /// Emits one terminal and everything under it, depth first. A terminal
    /// already placed is skipped, which is what stops a parent cycle.
    fn walk(&mut self, start: usize) {
        let mut pending = vec![(start, 0usize)];
        while let Some((index, depth)) = pending.pop() {
            if self.placed[index] {
                continue;
            }
            self.placed[index] = true;
            let node = &self.nodes[index];
            self.rows.push(TerminalRow {
                id: node.id.clone(),
                name: node.name.clone(),
                parent_id: node.parent_id.clone(),
                status: node.status.clone(),
                exited: node.exited,
                cwd: node.cwd.clone(),
                program: node.program.clone(),
                depth,
                has_children: self.has_children.contains(node.id.as_str()),
            });
            if let Some(children) = self.children.get(node.id.as_str()) {
                for &child in children.iter().rev() {
                    if !self.placed[child] {
                        pending.push((child, depth + 1));
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_terminal_with_no_parent_is_a_root() {
        let terminals = json!([
            { "id": "a", "name": "shell", "cwd": "/tmp", "program": "claude" },
        ]);
        let rows = terminal_rows(&terminals);

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, "a");
        assert_eq!(rows[0].depth, 0);
        assert!(!rows[0].has_children);
    }

    #[test]
    fn a_child_sits_under_its_parent() {
        let terminals = json!([
            { "id": "a", "program": "claude" },
            { "id": "b", "parent_id": "a", "program": "omp" },
        ]);
        let rows = terminal_rows(&terminals);

        let ids: Vec<&str> = rows.iter().map(|row| row.id.as_str()).collect();
        assert_eq!(ids, ["a", "b"]);
        assert_eq!(rows[0].depth, 0);
        assert!(rows[0].has_children);
        assert_eq!(rows[1].depth, 1);
        assert!(!rows[1].has_children);
    }

    #[test]
    fn a_terminal_whose_parent_is_missing_is_a_root() {
        let terminals = json!([
            { "id": "b", "parent_id": "gone", "program": "omp" },
        ]);
        let rows = terminal_rows(&terminals);

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].depth, 0);
        assert!(!rows[0].has_children);
    }

    #[test]
    fn a_terminal_that_has_not_reported_has_no_status() {
        let terminals = json!([{ "id": "a", "program": "claude" }]);
        let rows = terminal_rows(&terminals);

        assert_eq!(rows[0].status, None);
        assert_eq!(status_token(rows[0].status.as_deref()), None);
    }

    #[test]
    fn the_status_token_names_the_reported_state() {
        assert_eq!(status_token(Some("working")), Some("working"));
        assert_eq!(status_token(Some("done")), Some("done"));
        assert_eq!(status_token(Some("blocked")), Some("blocked"));
        assert_eq!(status_token(Some("error")), Some("error"));
        assert_eq!(status_token(None), None);
        assert_eq!(status_token(Some("compacting")), None);
    }

    #[test]
    fn a_cycle_in_the_parents_does_not_hang() {
        let terminals = json!([
            { "id": "a", "parent_id": "b", "program": "claude" },
            { "id": "b", "parent_id": "a", "program": "omp" },
        ]);
        let rows = terminal_rows(&terminals);

        assert_eq!(rows.len(), 2, "both are placed once");
        let ids: Vec<&str> = rows.iter().map(|row| row.id.as_str()).collect();
        assert!(ids.contains(&"a"));
        assert!(ids.contains(&"b"));
    }

    #[test]
    fn the_list_is_read_from_either_shape() {
        let bare = json!([
            { "id": "a", "program": "claude" },
            { "id": "b", "parent_id": "a", "program": "omp" },
        ]);
        let wrapped = json!({
            "id": "answer",
            "terminals": [
                { "id": "a", "program": "claude" },
                { "id": "b", "parent_id": "a", "program": "omp" },
            ],
        });

        assert_eq!(terminal_rows(&bare), terminal_rows(&wrapped));
    }

    #[test]
    fn a_terminal_missing_its_fields_is_not_invented() {
        let terminals = json!([{ "id": "a" }]);
        let rows = terminal_rows(&terminals);

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].name, None);
        assert_eq!(rows[0].status, None);
        assert_eq!(rows[0].cwd, "");
    }
}
