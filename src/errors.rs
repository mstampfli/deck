//! Error classification for stable JSON failures.
//!
//! Internal errors are mapped to coarse `DeckErrorKind` values so agents can
//! handle failures without parsing human prose.

/// Marker error for failures whose output was already emitted.
///
/// Commands that print a structured failure (for example a run summary with
/// `ok: false`) return this so the process exits nonzero without printing a
/// second, redundant error document.
#[derive(Debug)]
pub struct Reported;

impl std::fmt::Display for Reported {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("failure already reported")
    }
}

impl std::error::Error for Reported {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeckErrorKind {
    UnknownProject,
    AmbiguousProject,
    UnknownCommand,
    UnknownWorkflow,
    UnknownPlugin,
    UnknownTask,
    Conflict,
    InvalidInput,
    UnavailableTool,
    SandboxBackendMissing,
    SandboxPolicyDenied,
    CommandFailed,
    Error,
}

impl DeckErrorKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::UnknownProject => "unknown_project",
            Self::AmbiguousProject => "ambiguous_project",
            Self::UnknownCommand => "unknown_command",
            Self::UnknownWorkflow => "unknown_workflow",
            Self::UnknownPlugin => "unknown_plugin",
            Self::UnknownTask => "unknown_task",
            Self::Conflict => "conflict",
            Self::InvalidInput => "invalid_input",
            Self::UnavailableTool => "unavailable_tool",
            Self::SandboxBackendMissing => "sandbox_backend_missing",
            Self::SandboxPolicyDenied => "sandbox_policy_denied",
            Self::CommandFailed => "command_failed",
            Self::Error => "error",
        }
    }
}

pub fn classify(error: &anyhow::Error) -> DeckErrorKind {
    let message = error.to_string();
    if message.contains("no project matches") {
        DeckErrorKind::UnknownProject
    } else if message.contains("ambiguous") {
        DeckErrorKind::AmbiguousProject
    } else if message.contains("has no command")
        || message.contains("missing command")
        || message.contains("no command named")
    {
        DeckErrorKind::UnknownCommand
    } else if message.contains("has no workflow") || message.contains("no workflow named") {
        DeckErrorKind::UnknownWorkflow
    } else if message.contains("no plugin") {
        DeckErrorKind::UnknownPlugin
    } else if message.contains("no task") {
        DeckErrorKind::UnknownTask
    } else if message.contains("already exists") {
        DeckErrorKind::Conflict
    } else if message.contains("cannot be empty") {
        DeckErrorKind::InvalidInput
    } else if message.contains("unavailable") || message.contains("missing required tool") {
        DeckErrorKind::UnavailableTool
    } else if message.contains("sandbox backend missing") {
        DeckErrorKind::SandboxBackendMissing
    } else if message.contains("sandbox policy denied")
        || message.contains("sandbox writable path")
        || message.contains("outside project")
        || message.contains("cannot escape project")
        || message.contains("invalid sandbox env")
        || message.contains("sandbox timeout_seconds")
        || message.contains("shell commands are disabled")
        || message.contains("namespace creation is blocked")
        || message.contains("network namespace creation is blocked")
    {
        DeckErrorKind::SandboxPolicyDenied
    } else if message.contains("failed") {
        DeckErrorKind::CommandFailed
    } else {
        DeckErrorKind::Error
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_lookup_errors() {
        assert_eq!(
            classify(&anyhow::anyhow!("no project matches \"missing\"")),
            DeckErrorKind::UnknownProject
        );
        assert_eq!(
            classify(&anyhow::anyhow!("fixture has no command \"test\"")),
            DeckErrorKind::UnknownCommand
        );
    }

    #[test]
    fn classifies_conflict_and_invalid_input() {
        assert_eq!(
            classify(&anyhow::anyhow!("\"test\" already exists")),
            DeckErrorKind::Conflict
        );
        assert_eq!(
            classify(&anyhow::anyhow!("command name cannot be empty")),
            DeckErrorKind::InvalidInput
        );
    }
}
