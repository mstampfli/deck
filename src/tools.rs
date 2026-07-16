//! Thin wrappers around existing host tools.
//!
//! Deck exposes focused integrations for git, docker, gh, search, SSH hosts, and
//! journalctl without reimplementing those tools.

use std::ffi::OsStr;
use std::fs;
use std::io::Read;
use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result};

use crate::model::Project;

#[derive(Debug, Clone, Copy)]
pub enum GitAction {
    Diff,
    Branches,
    Commits,
}

pub fn git(project: &Project, action: GitAction) -> Result<String> {
    if !project.root.join(".git").exists() {
        anyhow::bail!("{} is not a git repository", project.name);
    }
    let args: &[&str] = match action {
        GitAction::Diff => &["diff", "--stat"],
        GitAction::Branches => &["branch", "--all", "--verbose"],
        GitAction::Commits => &["log", "--oneline", "--decorate", "-20"],
    };
    run_tool("git", args, &project.root)
}

pub fn docker_ps(project: Option<&Project>) -> Result<String> {
    let cwd = project.map_or_else(std::env::current_dir, |project| Ok(project.root.clone()))?;
    run_tool(
        "docker",
        &[
            "ps",
            "--format",
            "table {{.Names}}\t{{.Image}}\t{{.Status}}\t{{.Ports}}",
        ],
        &cwd,
    )
}

pub fn gh_issues(project: &Project) -> Result<String> {
    run_tool("gh", &["issue", "list", "--limit", "20"], &project.root)
}

pub fn search(project: &Project, query: &str, limit: usize) -> Result<String> {
    let limit = limit.to_string();
    match run_tool(
        "rg",
        &[
            "-n",
            "--hidden",
            "--glob",
            "!.git",
            "--glob",
            "!target",
            "--glob",
            "!node_modules",
            "--max-count",
            &limit,
            query,
        ],
        &project.root,
    ) {
        Ok(output) => Ok(output),
        Err(_) => search_internal(&project.root, query, limit.parse().unwrap_or(20)),
    }
}

pub fn ssh_hosts() -> Result<String> {
    let Some(base_dirs) = directories::BaseDirs::new() else {
        anyhow::bail!("could not resolve home directory");
    };
    let config = base_dirs.home_dir().join(".ssh/config");
    let raw =
        fs::read_to_string(&config).with_context(|| format!("reading {}", config.display()))?;
    let hosts = parse_ssh_hosts(&raw);
    if hosts.is_empty() {
        Ok("no hosts found in ~/.ssh/config\n".to_string())
    } else {
        Ok(hosts.join("\n") + "\n")
    }
}

pub fn journal(unit: Option<&str>, lines: usize) -> Result<String> {
    let line_arg = format!("-n{lines}");
    let mut args = vec!["--no-pager", line_arg.as_str()];
    if let Some(unit) = unit {
        args.push("-u");
        args.push(unit);
    }
    run_tool("journalctl", &args, &std::env::current_dir()?)
}

fn run_tool<S>(tool: &str, args: &[S], cwd: &Path) -> Result<String>
where
    S: AsRef<OsStr>,
{
    let output = Command::new(tool)
        .args(args)
        .current_dir(cwd)
        .env("PATH", command_path())
        .output()
        .with_context(|| format!("{tool} is not available"))?;

    let mut text = String::new();
    text.push_str(&String::from_utf8_lossy(&output.stdout));
    text.push_str(&String::from_utf8_lossy(&output.stderr));
    if !output.status.success() {
        anyhow::bail!("{tool} exited with status {}\n{text}", output.status);
    }
    Ok(text)
}

fn search_internal(root: &Path, query: &str, limit: usize) -> Result<String> {
    let mut matches = Vec::new();
    let mut walker = ignore::WalkBuilder::new(root);
    walker.hidden(false).git_ignore(true).filter_entry(|entry| {
        let Some(name) = entry.file_name().to_str() else {
            return true;
        };
        !matches!(name, ".git" | "target" | "node_modules")
    });

    for entry in walker.build() {
        let Ok(entry) = entry else {
            continue;
        };
        if !entry.file_type().is_some_and(|kind| kind.is_file()) {
            continue;
        }
        collect_file_matches(root, entry.path(), query, limit, &mut matches)?;
        if matches.len() >= limit {
            break;
        }
    }

    if matches.is_empty() {
        Ok(String::new())
    } else {
        Ok(matches.join("\n") + "\n")
    }
}

fn collect_file_matches(
    root: &Path,
    path: &Path,
    query: &str,
    limit: usize,
    matches: &mut Vec<String>,
) -> Result<()> {
    if matches.len() >= limit {
        return Ok(());
    }

    let mut file = fs::File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let mut bytes = Vec::new();
    file.by_ref().take(1024 * 1024).read_to_end(&mut bytes)?;
    if bytes.contains(&0) {
        return Ok(());
    }
    let Ok(text) = String::from_utf8(bytes) else {
        return Ok(());
    };
    let display_path = path.strip_prefix(root).unwrap_or(path);
    for (index, line) in text.lines().enumerate() {
        if line.contains(query) {
            matches.push(format!("{}:{}:{}", display_path.display(), index + 1, line));
            if matches.len() >= limit {
                break;
            }
        }
    }
    Ok(())
}

fn command_path() -> String {
    let current = std::env::var("PATH").unwrap_or_default();
    let Some(base_dirs) = directories::BaseDirs::new() else {
        return current;
    };
    let cargo_bin = base_dirs.home_dir().join(".cargo/bin");
    prepend_path_if_present(&current, &cargo_bin)
}

fn prepend_path_if_present(current: &str, path: &Path) -> String {
    if !path.is_dir() {
        return current.to_string();
    }
    let path = path.to_string_lossy();
    if current.split(':').any(|entry| entry == path) {
        current.to_string()
    } else if current.is_empty() {
        path.into_owned()
    } else {
        format!("{path}:{current}")
    }
}

fn parse_ssh_hosts(raw: &str) -> Vec<String> {
    raw.lines()
        .filter_map(|line| {
            let trimmed = line.trim();
            if trimmed.starts_with('#') {
                return None;
            }
            let mut parts = trimmed.split_whitespace();
            let keyword = parts.next()?;
            if !keyword.eq_ignore_ascii_case("host") {
                return None;
            }
            Some(
                parts
                    .filter(|host| !host.contains('*') && !host.contains('?') && *host != "!")
                    .map(str::to_string)
                    .collect::<Vec<_>>(),
            )
        })
        .flatten()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ssh_hosts_and_skips_patterns() {
        let raw = r#"
Host hub maka-*
  HostName example
Host github.com
Host *
  ForwardAgent no
"#;

        assert_eq!(parse_ssh_hosts(raw), vec!["hub", "github.com"]);
    }

    #[test]
    fn prepends_existing_path_once() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("bin");
        fs::create_dir(&path).unwrap();
        let rendered = prepend_path_if_present("/bin", &path);
        assert!(rendered.starts_with(path.to_string_lossy().as_ref()));
        assert_eq!(prepend_path_if_present(&rendered, &path), rendered);
    }

    #[test]
    fn internal_search_finds_text_and_skips_binary() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(temp.path().join("a.txt"), "hello\nneedle\n").unwrap();
        fs::write(temp.path().join("b.bin"), b"needle\0hidden").unwrap();

        let output = search_internal(temp.path(), "needle", 10).unwrap();

        assert!(output.contains("a.txt:2:needle"));
        assert!(!output.contains("b.bin"));
    }
}
