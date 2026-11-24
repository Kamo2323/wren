//! A platform-neutral copy of an accessibility tree (UI Automation on Windows, AX on macOS), so the
//! Teams parser is pure and testable from saved JSON dumps (plan Task 4).

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct UiNode {
    /// Control type / AX role, e.g. "Button", "ListItem", "AXButton".
    pub role: String,
    pub name: String,
    /// UIA HelpText / AX help or description.
    pub help: String,
    pub value: String,
    /// UIA AutomationId / AX identifier (Teams sets these from `data-tid`).
    pub automation_id: String,
    pub class: String,
    /// Extra states, e.g. "toggled_on", "toggled_off", "selected".
    pub states: Vec<String>,
    pub children: Vec<UiNode>,
}

impl UiNode {
    /// Depth-first walk over this node and every descendant.
    pub fn walk(&self) -> Vec<&UiNode> {
        let mut out = Vec::new();
        let mut stack = vec![self];
        while let Some(n) = stack.pop() {
            out.push(n);
            for c in n.children.iter().rev() {
                stack.push(c);
            }
        }
        out
    }

    pub fn count(&self) -> usize {
        1 + self.children.iter().map(UiNode::count).sum::<usize>()
    }
}

/// What `teams_probe` writes and the fixture tests read.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Dump {
    pub title: String,
    pub tree: UiNode,
}
