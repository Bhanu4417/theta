//! Named agent presets: tool sets, prompts and permission presets.
//!
//! Mirrors Pi's mode split (a full "build" agent vs. a read-only "plan"
//! agent) and gives the `task` tool named sub-agents to delegate to.

use std::sync::Arc;

use super::tools::Tool;

/// Which built-in tools an agent may use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolSet {
    /// Every tool (read + mutate).
    All,
    /// Investigation only: read/grep/glob/webfetch.
    ReadOnly,
}

#[derive(Debug, Clone, Copy)]
pub struct AgentDef {
    pub name: &'static str,
    pub description: &'static str,
    /// Appended to the base system prompt.
    pub system_prompt: &'static str,
    pub tools: ToolSet,
    /// Permission preset: `inherit` (use config), `ask`, `allow`, `read-only`.
    pub permission: &'static str,
    /// Whether this agent may spawn sub-agents via `task`.
    pub can_delegate: bool,
}

const BUILD_PROMPT: &str = "You are the build agent. Implement the user's request \
end to end, editing files and running commands as needed. Prefer reading before \
editing, keep changes focused, and verify your work.";
const PLAN_PROMPT: &str = "You are the plan agent. Investigate the codebase and \
produce a concrete, ordered plan. You cannot modify files: read, search, and \
reason, then present the plan. Do not claim to have made changes.";
const GENERAL_PROMPT: &str = "You are a general-purpose coding agent. Complete the \
task with the available tools and report back concisely.";
const EXPLORE_PROMPT: &str = "You are an exploration sub-agent. Search the codebase \
and return a precise, structured answer. You cannot modify files.";

/// Built-in agents, in picker order.
pub fn builtin() -> Vec<AgentDef> {
    vec![
        AgentDef {
            name: "build",
            description: "Full agent: read, edit, run",
            system_prompt: BUILD_PROMPT,
            tools: ToolSet::All,
            permission: "inherit",
            can_delegate: true,
        },
        AgentDef {
            name: "plan",
            description: "Read-only planning agent",
            system_prompt: PLAN_PROMPT,
            tools: ToolSet::ReadOnly,
            permission: "read-only",
            can_delegate: false,
        },
        AgentDef {
            name: "general",
            description: "General-purpose agent",
            system_prompt: GENERAL_PROMPT,
            tools: ToolSet::All,
            permission: "inherit",
            can_delegate: true,
        },
        AgentDef {
            name: "explore",
            description: "Read-only exploration sub-agent",
            system_prompt: EXPLORE_PROMPT,
            tools: ToolSet::ReadOnly,
            permission: "read-only",
            can_delegate: false,
        },
    ]
}

pub fn find(name: &str) -> Option<AgentDef> {
    let name = name.trim().to_ascii_lowercase();
    builtin().into_iter().find(|a| a.name == name)
}

/// Default agent when none is selected.
pub fn default_name() -> &'static str {
    "build"
}

/// `(name, description)` pairs for pickers.
pub fn names() -> Vec<(String, String)> {
    builtin()
        .into_iter()
        .map(|a| (a.name.to_string(), a.description.to_string()))
        .collect()
}

/// Names offered as `task` sub-agent types (delegating agents excluded).
pub fn subagent_names() -> Vec<&'static str> {
    builtin()
        .into_iter()
        .filter(|a| a.name == "general" || a.name == "explore" || a.name == "plan")
        .map(|a| a.name)
        .collect()
}

/// The tools a definition is allowed to use, given the full tool list.
pub fn tools_for(def: &AgentDef, all: Vec<Arc<dyn Tool>>) -> Vec<Arc<dyn Tool>> {
    match def.tools {
        ToolSet::All => all,
        ToolSet::ReadOnly => {
            let allow = ["read", "grep", "glob", "webfetch"];
            all.into_iter()
                .filter(|t| allow.contains(&t.spec().name.as_str()))
                .collect()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtins_have_unique_names_and_a_default() {
        let names: Vec<&str> = builtin().iter().map(|a| a.name).collect();
        let mut sorted = names.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(names.len(), sorted.len(), "agent names are unique");
        assert!(find(default_name()).is_some());
        assert!(find("PLAN").is_some(), "lookup is case-insensitive");
    }

    #[test]
    fn plan_and_explore_are_read_only() {
        assert_eq!(find("plan").unwrap().tools, ToolSet::ReadOnly);
        assert_eq!(find("explore").unwrap().tools, ToolSet::ReadOnly);
        assert_eq!(find("build").unwrap().tools, ToolSet::All);
        assert!(!find("plan").unwrap().can_delegate);
        assert!(find("build").unwrap().can_delegate);
    }

    #[test]
    fn tool_selection_filters_read_only() {
        let all = super::super::tools::default_tools();
        let plan = find("plan").unwrap();
        let ro = tools_for(&plan, all.clone());
        assert!(ro.iter().all(|t| {
            matches!(t.spec().name.as_str(), "read" | "grep" | "glob" | "webfetch")
        }));
        assert!(!ro.iter().any(|t| t.spec().name == "bash"));
        assert_eq!(tools_for(&find("build").unwrap(), all.clone()).len(), all.len());
        // Every subagent type resolves to a real definition.
        for n in subagent_names() {
            assert!(find(n).is_some());
        }
    }
}
