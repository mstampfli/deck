//! Computed command safety metadata.
//!
//! Safety values describe whether a command uses direct `argv`, requires shell
//! execution, is a server, and can run under locked sandbox policy.

use serde::{Deserialize, Serialize};

use crate::model::{CommandKind, CommandSpec};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandSafety {
    pub direct_argv: bool,
    pub uses_shell: bool,
    pub server: bool,
    pub locked_sandbox_compatible: bool,
    pub requires_shell_allowed: bool,
    pub notes: Vec<String>,
}

pub fn command_safety(command: &CommandSpec) -> CommandSafety {
    let direct_argv = command.argv.is_some();
    let uses_shell = !direct_argv;
    let server = command.kind == CommandKind::Server;
    let mut notes = Vec::new();

    if uses_shell {
        notes.push("uses shell parsing; blocked by allow_shell=false profiles".to_string());
    }
    if server {
        notes.push(
            "server command may need a dev sandbox profile with network/project writes".to_string(),
        );
    }
    if !command.available {
        notes.push("command is currently unavailable".to_string());
    }

    CommandSafety {
        direct_argv,
        uses_shell,
        server,
        locked_sandbox_compatible: direct_argv && command.available && !server,
        requires_shell_allowed: uses_shell,
        notes,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{CommandCategory, CommandSource};
    use std::path::PathBuf;

    fn command(argv: Option<Vec<String>>, kind: CommandKind) -> CommandSpec {
        CommandSpec {
            name: "test".to_string(),
            source: CommandSource::DeckToml,
            command: "cargo test".to_string(),
            argv,
            cwd: PathBuf::from("/tmp/project"),
            kind,
            port: None,
            category: CommandCategory::Test,
            available: true,
            unavailable_reason: None,
        }
    }

    #[test]
    fn direct_argv_is_locked_sandbox_compatible() {
        let safety = command_safety(&command(
            Some(vec!["cargo".into(), "test".into()]),
            CommandKind::Once,
        ));

        assert!(safety.direct_argv);
        assert!(!safety.uses_shell);
        assert!(safety.locked_sandbox_compatible);
    }

    #[test]
    fn shell_command_requires_shell_allowed() {
        let safety = command_safety(&command(None, CommandKind::Once));

        assert!(safety.uses_shell);
        assert!(safety.requires_shell_allowed);
        assert!(!safety.locked_sandbox_compatible);
    }
}
