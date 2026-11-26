//! `fetchpath agents` and `fetchpath approvals` (FP-066): the access the
//! person gives each agent host, and the requests waiting for them. The
//! engine keeps and enforces both; only the person (`user`) can change them.

use crate::client::{self, Engine};
use crate::download::{self, EXIT_USAGE};
use fetchpath_protocol::command::{Command, JobFilter};
use fetchpath_protocol::message::CommandResult;
use fetchpath_protocol::principal::{AgentAccess, AgentName, AgentPermissionMode, AgentPolicy};
use fetchpath_protocol::{JobSnapshot, ProtocolError};
use std::path::Path;

pub const USAGE: &str = "usage: fetchpath agents [list]
       fetchpath agents mode NAME ask|scoped|auto|full
       fetchpath agents mode --all ask|scoped|auto|full
       fetchpath agents grant NAME FOLDER...      let NAME save into these folders
       fetchpath agents revoke NAME [FOLDER...]   take folders away, or all of NAME's access
       fetchpath agents limit NAME [--size SIZE] [--per-hour N]
       fetchpath agents auto NAME on|off          shorthand for `mode NAME auto` / `scoped`
       fetchpath approvals                        requests waiting for you

Permission modes:
  ask          every download waits for your approval (keeps saved folders/limits)
  scoped       inside granted folders, allowed up to size/hourly limits (default)
  auto         inside granted folders, skips size, hourly, and torrent peer holds
  full         full access: skip all download approvals anywhere on disk
               (still never overwrites files or exposes settings/credentials)

NAME is the name given with `fetchpath mcp --agent NAME` (or auto-detected host id).";

pub fn run(args: &[String]) -> i32 {
    let json = args.iter().any(|arg| arg == "--json");
    let words: Vec<&str> = args
        .iter()
        .map(String::as_str)
        .filter(|arg| *arg != "--json")
        .collect();
    let Some(request) = Request::parse(&words) else {
        eprintln!("{USAGE}");
        return EXIT_USAGE;
    };
    match Engine::connect().and_then(|engine| request.perform(&engine)) {
        Ok(policies) => {
            print(&policies, json);
            0
        }
        Err(error) => client::fail(&error, json),
    }
}

/// One `agents` request, in the words `fetchpath agents` and the terminal's
/// `/agents` share.
pub(crate) enum Request<'a> {
    List,
    Mode(&'a str, AgentPermissionMode),
    ModeAll(AgentPermissionMode),
    Grant(&'a str, &'a [&'a str]),
    Revoke(&'a str, &'a [&'a str]),
    Limit(&'a str, &'a [&'a str]),
    Automatic(&'a str, bool),
}

impl<'a> Request<'a> {
    /// `None` when the words are not a request.
    pub(crate) fn parse(words: &'a [&'a str]) -> Option<Self> {
        Some(match words {
            [] | ["list"] => Self::List,
            ["mode", "--all", mode] => Self::ModeAll(AgentPermissionMode::parse(mode)?),
            ["mode", name, mode] => Self::Mode(name, AgentPermissionMode::parse(mode)?),
            ["grant", name, folders @ ..] if !folders.is_empty() => Self::Grant(name, folders),
            ["revoke", name, folders @ ..] => Self::Revoke(name, folders),
            ["limit", name, rest @ ..] if !rest.is_empty() => Self::Limit(name, rest),
            ["auto", name, switch @ ("on" | "off")] => Self::Automatic(name, *switch == "on"),
            _ => return None,
        })
    }

    /// Carries it out and returns every agent's access afterwards.
    pub(crate) fn perform(&self, engine: &Engine) -> Result<Vec<AgentAccess>, ProtocolError> {
        match *self {
            Self::List => policies(engine),
            Self::Mode(name, mode) => set_mode(engine, parse_name(name)?, mode),
            Self::ModeAll(mode) => set_mode_all(engine, mode),
            Self::Grant(name, folders) => grant(engine, parse_name(name)?, folders),
            Self::Revoke(name, folders) => revoke(engine, parse_name(name)?, folders),
            Self::Limit(name, rest) => limit(engine, parse_name(name)?, rest),
            Self::Automatic(name, on) => automatic(engine, parse_name(name)?, on),
        }
    }
}

fn parse_name(name: &str) -> Result<AgentName, ProtocolError> {
    AgentName::try_from(name)
        .map_err(|message| client::input_error(&format!("{name:?}: {message}.")))
}

fn policies(engine: &Engine) -> Result<Vec<AgentAccess>, ProtocolError> {
    match engine.send(Command::GetAgentPolicies)? {
        CommandResult::AgentPolicies { policies } => Ok(policies),
        other => Err(client::unexpected(&other)),
    }
}

/// An agent's access now, or the default for one not configured yet.
fn current(engine: &Engine, agent: &AgentName) -> Result<AgentPolicy, ProtocolError> {
    Ok(policies(engine)?
        .into_iter()
        .find(|access| &access.agent == agent)
        .map(|access| access.policy)
        .unwrap_or_default())
}

fn set(
    engine: &Engine,
    agent: &AgentName,
    policy: Option<AgentPolicy>,
) -> Result<Vec<AgentAccess>, ProtocolError> {
    match engine.send(Command::SetAgentPolicy {
        agent: agent.clone(),
        policy,
    })? {
        CommandResult::AgentPolicies { policies } => Ok(policies),
        other => Err(client::unexpected(&other)),
    }
}

pub fn lines(policies: &[AgentAccess]) -> Vec<String> {
    if policies.is_empty() {
        return vec![
            "No agent has access yet. Every agent's request waits for you.".into(),
            "Run `fetchpath agents grant NAME FOLDER` or `fetchpath agents mode NAME full`.".into(),
        ];
    }
    let mut out = Vec::new();
    for access in policies {
        let policy = &access.policy;
        let mode = policy.effective_mode();
        out.push(match mode {
            AgentPermissionMode::Ask => format!(
                "{}  [ask] every download waits for your approval",
                access.agent
            ),
            AgentPermissionMode::Scoped => format!(
                "{}  [scoped] up to {} a download, {} downloads an hour",
                access.agent,
                client::bytes(policy.max_bytes),
                policy.max_new_jobs_per_hour
            ),
            AgentPermissionMode::Auto => format!(
                "{}  [auto] automatic inside its folders, nothing it downloads waits for you",
                access.agent
            ),
            AgentPermissionMode::FullAccess => format!(
                "{}  [full-access] skips all download approvals anywhere on disk",
                access.agent
            ),
        });
        if mode == AgentPermissionMode::FullAccess {
            if !policy.folders.is_empty() {
                out.push("  (saved folders kept if you switch back to scoped/auto:)".into());
                for folder in &policy.folders {
                    out.push(format!("  {folder}"));
                }
            }
            continue;
        }
        if policy.folders.is_empty() {
            out.push("  no folders: everything it asks for waits for you".into());
        }
        for folder in &policy.folders {
            out.push(format!("  {folder}"));
        }
    }
    out
}

fn print(policies: &[AgentAccess], json: bool) {
    if json {
        client::print_json(&CommandResult::AgentPolicies {
            policies: policies.to_vec(),
        });
        return;
    }
    for line in lines(policies) {
        println!("{line}");
    }
}

/// A folder as the engine wants it: full, existing, and spelled the same way
/// each time so it can be found again to revoke.
fn folder(text: &str) -> Result<String, ProtocolError> {
    let path = download::absolute(Path::new(text.trim()))
        .map_err(|message| client::input_error(&message))?;
    if !path.is_dir() {
        return Err(client::input_error(&format!(
            "{} is not a folder. Create it first, then grant it.",
            path.display()
        )));
    }
    let text = path.display().to_string();
    // `D:\` keeps its separator; any other folder drops a trailing one.
    Ok(if text.len() > 3 {
        text.trim_end_matches(['\\', '/']).to_owned()
    } else {
        text
    })
}

fn same_folder(a: &str, b: &str) -> bool {
    a.trim_end_matches(['\\', '/'])
        .eq_ignore_ascii_case(b.trim_end_matches(['\\', '/']))
}

fn grant(
    engine: &Engine,
    agent: AgentName,
    folders: &[&str],
) -> Result<Vec<AgentAccess>, ProtocolError> {
    let mut policy = current(engine, &agent)?;
    for text in folders {
        let folder = folder(text)?;
        if !policy.folders.iter().any(|held| same_folder(held, &folder)) {
            policy.folders.push(folder);
        }
    }
    set(engine, &agent, Some(policy))
}

fn revoke(
    engine: &Engine,
    agent: AgentName,
    folders: &[&str],
) -> Result<Vec<AgentAccess>, ProtocolError> {
    if folders.is_empty() {
        // All of it: the agent goes back to asking for everything, and its
        // unfinished downloads wait for the person (engine, D4).
        return set(engine, &agent, None);
    }
    let mut policy = current(engine, &agent)?;
    for text in folders {
        let before = policy.folders.len();
        let wanted = download::absolute(Path::new(text.trim()))
            .map(|path| path.display().to_string())
            .unwrap_or_else(|_| (*text).to_owned());
        policy.folders.retain(|held| !same_folder(held, &wanted));
        if policy.folders.len() == before {
            return Err(client::input_error(&format!(
                "{agent} was not given {wanted}. `fetchpath agents` lists its folders."
            )));
        }
    }
    set(engine, &agent, Some(policy))
}

fn limit(
    engine: &Engine,
    agent: AgentName,
    rest: &[&str],
) -> Result<Vec<AgentAccess>, ProtocolError> {
    let mut policy = current(engine, &agent)?;
    let mut words = rest.iter();
    while let Some(word) = words.next() {
        let (flag, inline) = match word.split_once('=') {
            Some((flag, value)) => (flag, Some(value)),
            None => (*word, None),
        };
        let Some(value) = inline.or_else(|| words.next().copied()) else {
            return Err(client::input_error(&format!(
                "{flag} needs a value.\n{USAGE}"
            )));
        };
        match flag {
            "--size" => {
                policy.max_bytes =
                    crate::rules::size(value).map_err(|message| client::input_error(&message))?;
            }
            "--per-hour" => {
                policy.max_new_jobs_per_hour = value.parse().map_err(|_| {
                    client::input_error(&format!("{value} is not a whole number of downloads."))
                })?;
            }
            other => {
                return Err(client::input_error(&format!(
                    "unknown option {other}.\n{USAGE}"
                )));
            }
        }
    }
    set(engine, &agent, Some(policy))
}

/// `fetchpath agents mode NAME ask|scoped|auto|full`.
pub(crate) fn set_mode(
    engine: &Engine,
    agent: AgentName,
    mode: AgentPermissionMode,
) -> Result<Vec<AgentAccess>, ProtocolError> {
    let mut policy = current(engine, &agent)?;
    policy.set_mode(mode);
    set(engine, &agent, Some(policy))
}

/// Applies `mode` to every configured agent and every connected MCP host on this PC.
fn set_mode_all(
    engine: &Engine,
    mode: AgentPermissionMode,
) -> Result<Vec<AgentAccess>, ProtocolError> {
    let mut names: std::collections::BTreeSet<AgentName> = policies(engine)?
        .into_iter()
        .map(|access| access.agent)
        .collect();
    if let Ok(status) = crate::agent_setup::status_json()
        && let Some(hosts) = status["hosts"].as_array()
    {
        for host in hosts {
            if host["registered"] == serde_json::Value::Bool(true)
                && let Some(id) = host["id"].as_str()
                && let Ok(name) = AgentName::try_from(id)
            {
                names.insert(name);
            }
        }
    }
    if names.is_empty() {
        return Err(client::input_error(
            "No configured or connected agents found. Connect a host with `fetchpath agent-setup add --detected` or pass a specific name: `fetchpath agents mode NAME full`.",
        ));
    }
    let mut latest = Vec::new();
    for agent in names {
        latest = set_mode(engine, agent, mode)?;
    }
    Ok(latest)
}

/// `fetchpath agents auto NAME on|off` (contract D7). Folders still apply.
fn automatic(
    engine: &Engine,
    agent: AgentName,
    on: bool,
) -> Result<Vec<AgentAccess>, ProtocolError> {
    set_mode(
        engine,
        agent,
        if on {
            AgentPermissionMode::Auto
        } else {
            AgentPermissionMode::Scoped
        },
    )
}

/// `fetchpath approvals`: every request waiting for the person, as the
/// terminal's card describes it.
pub fn approvals(args: &[String]) -> i32 {
    let json = match args {
        [] => false,
        [flag] if flag == "--json" => true,
        _ => {
            eprintln!("usage: fetchpath approvals [--json]");
            return EXIT_USAGE;
        }
    };
    let result = Engine::connect().and_then(|engine| engine.jobs(JobFilter::AwaitingApproval));
    match result {
        Ok(jobs) => {
            if json {
                client::print_json(&CommandResult::Jobs { jobs });
            } else {
                for line in approval_lines(&jobs) {
                    println!("{line}");
                }
            }
            0
        }
        Err(error) => client::fail(&error, json),
    }
}

fn approval_lines(jobs: &[JobSnapshot]) -> Vec<String> {
    if jobs.is_empty() {
        return vec!["No agent is waiting for an answer.".into()];
    }
    let mut out = Vec::new();
    for job in jobs {
        out.extend(crate::tui::approval_lines(job, None));
    }
    out.push("Answer with fetchpath approve JOB or fetchpath deny JOB.".into());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folders_compare_without_case_or_trailing_separators() {
        assert!(same_folder(r"C:\Downloads\", r"c:\downloads"));
        assert!(!same_folder(r"C:\Downloads", r"C:\Downloads2"));
    }

    #[test]
    fn an_agent_without_folders_is_said_to_ask_for_everything() {
        let lines = lines(&[AgentAccess {
            agent: AgentName::try_from("helper").unwrap(),
            policy: AgentPolicy::default(),
        }]);
        assert!(lines[0].starts_with("helper  [scoped] up to 1.0 GiB a download, 20 downloads"));
        assert!(lines[1].contains("waits for you"));
    }

    #[test]
    fn mode_requests_parse_aliases_and_all_flag() {
        assert!(matches!(
            Request::parse(&["mode", "codex", "full"]),
            Some(Request::Mode("codex", AgentPermissionMode::FullAccess))
        ));
        assert!(matches!(
            Request::parse(&["mode", "codex", "bypass"]),
            Some(Request::Mode("codex", AgentPermissionMode::FullAccess))
        ));
        assert!(matches!(
            Request::parse(&["mode", "--all", "ask"]),
            Some(Request::ModeAll(AgentPermissionMode::Ask))
        ));
        assert!(Request::parse(&["mode", "codex", "invalid"]).is_none());
    }
}
