//! `fetchpath agents` and `fetchpath approvals` (FP-066): the access the
//! person gives each agent host, and the requests waiting for them. The
//! engine keeps and enforces both; only the person (`user`) can change them.

use crate::client::{self, Engine};
use crate::download::{self, EXIT_USAGE};
use fetchpath_protocol::command::{Command, JobFilter};
use fetchpath_protocol::message::CommandResult;
use fetchpath_protocol::principal::{AgentAccess, AgentName, AgentPolicy};
use fetchpath_protocol::{JobSnapshot, ProtocolError};
use std::path::Path;

pub const USAGE: &str = "usage: fetchpath agents [list]
       fetchpath agents grant NAME FOLDER...      let NAME save into these folders
       fetchpath agents revoke NAME [FOLDER...]   take folders away, or all of NAME's access
       fetchpath agents limit NAME [--size SIZE] [--per-hour N]
       fetchpath agents auto NAME on|off          inside its folders, never ask about size,
                                                  downloads an hour or torrent peers
       fetchpath approvals                        requests waiting for you
NAME is the name given with `fetchpath mcp --agent NAME`. Anything outside
an agent's folders or limits waits for `fetchpath approve JOB`.";

pub fn run(args: &[String]) -> i32 {
    let json = args.iter().any(|arg| arg == "--json");
    let words: Vec<&str> = args
        .iter()
        .map(String::as_str)
        .filter(|arg| *arg != "--json")
        .collect();
    let outcome = match words.as_slice() {
        [] | ["list"] => list(json),
        ["grant", name, folders @ ..] if !folders.is_empty() => {
            parse_name(name).and_then(|agent| grant(agent, folders, json))
        }
        ["revoke", name, folders @ ..] => {
            parse_name(name).and_then(|agent| revoke(agent, folders, json))
        }
        ["limit", name, rest @ ..] if !rest.is_empty() => {
            parse_name(name).and_then(|agent| limit(agent, rest, json))
        }
        ["auto", name, switch @ ("on" | "off")] => {
            parse_name(name).and_then(|agent| automatic(agent, *switch == "on", json))
        }
        _ => {
            eprintln!("{USAGE}");
            return EXIT_USAGE;
        }
    };
    match outcome {
        Ok(code) => code,
        Err(error) => client::fail(&error, json),
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
    json: bool,
) -> Result<i32, ProtocolError> {
    let policies = match engine.send(Command::SetAgentPolicy {
        agent: agent.clone(),
        policy,
    })? {
        CommandResult::AgentPolicies { policies } => policies,
        other => return Err(client::unexpected(&other)),
    };
    print(&policies, json);
    Ok(0)
}

pub fn lines(policies: &[AgentAccess]) -> Vec<String> {
    if policies.is_empty() {
        return vec![
            "No agent has access yet. Every agent's request waits for you.".into(),
            "fetchpath agents grant NAME FOLDER lets one save into a folder.".into(),
        ];
    }
    let mut out = Vec::new();
    for access in policies {
        let policy = &access.policy;
        out.push(if policy.automatic {
            format!(
                "{}  automatic: inside its folders, nothing it downloads waits for you",
                access.agent
            )
        } else {
            format!(
                "{}  up to {} a download, {} downloads an hour",
                access.agent,
                client::bytes(policy.max_bytes),
                policy.max_new_jobs_per_hour
            )
        });
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

fn list(json: bool) -> Result<i32, ProtocolError> {
    print(&policies(&Engine::connect()?)?, json);
    Ok(0)
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

fn grant(agent: AgentName, folders: &[&str], json: bool) -> Result<i32, ProtocolError> {
    let engine = Engine::connect()?;
    let mut policy = current(&engine, &agent)?;
    for text in folders {
        let folder = folder(text)?;
        if !policy.folders.iter().any(|held| same_folder(held, &folder)) {
            policy.folders.push(folder);
        }
    }
    set(&engine, &agent, Some(policy), json)
}

fn revoke(agent: AgentName, folders: &[&str], json: bool) -> Result<i32, ProtocolError> {
    let engine = Engine::connect()?;
    if folders.is_empty() {
        // All of it: the agent goes back to asking for everything, and its
        // unfinished downloads wait for the person (engine, D4).
        return set(&engine, &agent, None, json);
    }
    let mut policy = current(&engine, &agent)?;
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
    set(&engine, &agent, Some(policy), json)
}

fn limit(agent: AgentName, rest: &[&str], json: bool) -> Result<i32, ProtocolError> {
    let engine = Engine::connect()?;
    let mut policy = current(&engine, &agent)?;
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
    set(&engine, &agent, Some(policy), json)
}

/// `fetchpath agents auto NAME on|off` (contract D7). Folders still apply.
fn automatic(agent: AgentName, on: bool, json: bool) -> Result<i32, ProtocolError> {
    let engine = Engine::connect()?;
    let mut policy = current(&engine, &agent)?;
    policy.automatic = on;
    set(&engine, &agent, Some(policy), json)
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
        assert!(lines[0].starts_with("helper  up to 1.0 GiB a download, 20 downloads"));
        assert!(lines[1].contains("waits for you"));
    }
}
