//! Agent inbox offers, agent access policies, smart rules, and content cache commands.

use crate::{Engine, job_id, off_thread, text, unexpected, view};
use fetchpath_protocol::SensitiveUrl;
use fetchpath_protocol::command::Command;
use fetchpath_protocol::message::CommandResult;
use fetchpath_protocol::model::{self, RuleSpec};
use fetchpath_protocol::principal::{AgentName, AgentPolicy};
use std::sync::Arc;

/// Files agents offered you. Only your own clients can answer them.
#[tauri::command]
pub(crate) async fn list_inbox(engine: Engine<'_>) -> Result<Vec<view::OfferRow>, String> {
    let engine = Arc::clone(&engine);
    off_thread(
        move || match engine.send(Command::ListInbox).map_err(text)? {
            CommandResult::Inbox { offers } => Ok(offers.iter().map(view::offer).collect()),
            other => Err(unexpected(&other)),
        },
    )
    .await
}

fn offer_row(result: CommandResult) -> Result<view::OfferRow, String> {
    match result {
        CommandResult::Offer { offer } => Ok(view::offer(&offer)),
        other => Err(unexpected(&other)),
    }
}

/// Copies an offered file into `folder` (or the default save folder). The
/// engine checks the copy against the offered hash and never replaces a file.
#[tauri::command]
pub(crate) async fn accept_offer(
    offer_id: String,
    folder: Option<String>,
    engine: Engine<'_>,
) -> Result<view::OfferRow, String> {
    let engine = Arc::clone(&engine);
    off_thread(move || {
        offer_row(
            engine
                .send(Command::AcceptOffer {
                    offer_id: job_id(&offer_id)?,
                    destination: folder,
                })
                .map_err(text)?,
        )
    })
    .await
}

#[tauri::command]
pub(crate) async fn decline_offer(
    offer_id: String,
    engine: Engine<'_>,
) -> Result<view::OfferRow, String> {
    let engine = Arc::clone(&engine);
    off_thread(move || {
        offer_row(
            engine
                .send(Command::DeclineOffer {
                    offer_id: job_id(&offer_id)?,
                })
                .map_err(text)?,
        )
    })
    .await
}

fn agent_views(result: CommandResult) -> Result<Vec<view::AgentView>, String> {
    match result {
        CommandResult::AgentPolicies { policies } => Ok(view::agents(&policies)),
        other => Err(unexpected(&other)),
    }
}

fn agent_name(name: &str) -> Result<AgentName, String> {
    AgentName::try_from(name.trim())
        .map_err(|message| format!("That name cannot be used: {message}."))
}

#[tauri::command]
pub(crate) async fn list_agents(engine: Engine<'_>) -> Result<Vec<view::AgentView>, String> {
    let engine = Arc::clone(&engine);
    off_thread(move || agent_views(engine.send(Command::GetAgentPolicies).map_err(text)?)).await
}

/// Sets one agent's folders and limits. Its unfinished downloads outside
/// the new folders wait for approval again (engine, D4).
#[tauri::command]
pub(crate) async fn set_agent(
    name: String,
    folders: Vec<String>,
    max_bytes: u64,
    max_new_jobs_per_hour: u32,
    automatic: Option<bool>,
    engine: Engine<'_>,
) -> Result<Vec<view::AgentView>, String> {
    let engine = Arc::clone(&engine);
    off_thread(move || {
        let command = Command::SetAgentPolicy {
            agent: agent_name(&name)?,
            policy: Some(AgentPolicy {
                folders,
                max_bytes,
                max_new_jobs_per_hour,
                automatic: automatic.unwrap_or(false),
            }),
        };
        agent_views(engine.send(command).map_err(text)?)
    })
    .await
}

/// Removes an agent's access; everything it has not finished waits for the
/// person again.
#[tauri::command]
pub(crate) async fn revoke_agent(
    name: String,
    engine: Engine<'_>,
) -> Result<Vec<view::AgentView>, String> {
    let engine = Arc::clone(&engine);
    off_thread(move || {
        let command = Command::SetAgentPolicy {
            agent: agent_name(&name)?,
            policy: None,
        };
        agent_views(engine.send(command).map_err(text)?)
    })
    .await
}

fn rule_views(result: CommandResult) -> Result<Vec<view::RuleView>, String> {
    match result {
        CommandResult::Rules { rules } => Ok(view::rules(&rules)),
        other => Err(unexpected(&other)),
    }
}

#[tauri::command]
pub(crate) async fn list_rules(engine: Engine<'_>) -> Result<Vec<view::RuleView>, String> {
    let engine = Arc::clone(&engine);
    off_thread(move || rule_views(engine.send(Command::ListRules).map_err(text)?)).await
}

/// Adds a rule last. The engine checks and normalizes it (FP-064).
#[tauri::command]
pub(crate) async fn add_rule(
    rule: RuleSpec,
    engine: Engine<'_>,
) -> Result<Vec<view::RuleView>, String> {
    let engine = Arc::clone(&engine);
    off_thread(move || {
        let command = Command::AddRule {
            rule: Box::new(rule),
            position: None,
        };
        rule_views(engine.send(command).map_err(text)?)
    })
    .await
}

#[tauri::command]
pub(crate) async fn remove_rule(
    rule_id: u32,
    engine: Engine<'_>,
) -> Result<Vec<view::RuleView>, String> {
    let engine = Arc::clone(&engine);
    off_thread(move || rule_views(engine.send(Command::RemoveRule { rule_id }).map_err(text)?))
        .await
}

/// How the rules decide for a link. The engine reads the link's headers
/// (not its body) so a size rule can decide.
#[tauri::command]
pub(crate) async fn inspect_rules(
    url: String,
    engine: Engine<'_>,
) -> Result<view::RuleAdvice, String> {
    let engine = Arc::clone(&engine);
    off_thread(move || {
        let url = SensitiveUrl::try_from(url)
            .map_err(|message| format!("That link cannot be used: {message}."))?;
        match engine.send(Command::InspectLink { url }).map_err(text)? {
            CommandResult::LinkInspection { inspection } => {
                Ok(view::rule_advice(inspection.rules.as_ref()))
            }
            other => Err(unexpected(&other)),
        }
    })
    .await
}

fn cache_view(result: CommandResult) -> Result<model::CacheView, String> {
    match result {
        CommandResult::Cache { cache } => Ok(cache),
        other => Err(unexpected(&other)),
    }
}

/// How much the content cache holds, and its quota bounds (FP-032).
#[tauri::command]
pub(crate) async fn cache_status(engine: Engine<'_>) -> Result<model::CacheView, String> {
    let engine = Arc::clone(&engine);
    off_thread(move || cache_view(engine.send(Command::CacheStatus).map_err(text)?)).await
}

#[tauri::command]
pub(crate) async fn clear_cache(engine: Engine<'_>) -> Result<model::CacheView, String> {
    let engine = Arc::clone(&engine);
    off_thread(move || cache_view(engine.send(Command::ClearCache).map_err(text)?)).await
}
