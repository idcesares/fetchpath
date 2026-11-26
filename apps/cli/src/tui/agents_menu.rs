//! `/agents` interactive menu in the inline TUI: inspect connected and
//! configured AI agents and cycle their permission modes (`ask`, `scoped`,
//! `auto`, `full-access`) live with Enter or ←/→.

use super::menu::{Item, Menu, Pick};
use crate::agent_setup;
use crate::agents;
use crate::client::{self, Engine};
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, MouseEvent};
use fetchpath_protocol::ProtocolError;
use fetchpath_protocol::command::Command;
use fetchpath_protocol::message::CommandResult;
use fetchpath_protocol::principal::{AgentAccess, AgentName, AgentPermissionMode, AgentPolicy};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, Eq, PartialEq)]
enum Row {
    Heading,
    AllAgents,
    Agent(AgentName),
}

#[derive(Debug, Eq, PartialEq)]
pub enum Effect {
    None,
    Close,
}

pub struct AgentsMenu {
    pub menu: Menu,
    policies: BTreeMap<AgentName, AgentPolicy>,
    connected: BTreeSet<AgentName>,
    rows: Vec<Row>,
}

fn mode_detail(policy: &AgentPolicy) -> String {
    match policy.effective_mode() {
        AgentPermissionMode::Ask => "Every download waits for your approval".into(),
        AgentPermissionMode::Scoped => {
            if policy.folders.is_empty() {
                "No folders granted yet; waits for approval (grant with /agents grant NAME FOLDER)"
                    .into()
            } else {
                format!(
                    "{} folder(s) · up to {}, {}/hr · ←/→ change mode",
                    policy.folders.len(),
                    client::bytes(policy.max_bytes),
                    policy.max_new_jobs_per_hour
                )
            }
        }
        AgentPermissionMode::Auto => {
            if policy.folders.is_empty() {
                "Auto needs a folder (grant with /agents grant NAME FOLDER), or switch to full-access"
                    .into()
            } else {
                format!(
                    "Auto in {} folder(s): no size/rate/peer holds · ←/→ change mode",
                    policy.folders.len()
                )
            }
        }
        AgentPermissionMode::FullAccess => {
            "Full access: skips all download approvals (never overwrites files or exposes settings)"
                .into()
        }
    }
}

impl AgentsMenu {
    pub fn open(engine: &Engine) -> Result<Self, ProtocolError> {
        let policies = fetch_policies(engine)?;
        let connected = connected_hosts();
        let mut this = Self {
            menu: Menu::new(
                "AI Agents",
                Vec::new(),
                "↑↓ choose · Enter or ←→ change permission mode · Esc close",
            ),
            policies,
            connected,
            rows: Vec::new(),
        };
        this.rebuild();
        this.menu.select(1, 1);
        Ok(this)
    }

    fn all_names(&self) -> Vec<AgentName> {
        self.policies
            .keys()
            .chain(self.connected.iter())
            .cloned()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    }

    fn policy_for(&self, name: &AgentName) -> AgentPolicy {
        self.policies.get(name).cloned().unwrap_or_default()
    }

    fn rebuild(&mut self) {
        let names = self.all_names();
        let heading = |text: &str| Item {
            inert: true,
            ..Item::new(text, "", "")
        };
        let mut built: Vec<(Row, Item)> = Vec::new();
        if names.is_empty() {
            built.push((Row::Heading, heading("No agents configured or connected")));
            built.push((
                Row::Heading,
                heading("Use /agents mode NAME full  or  /agents grant NAME FOLDER"),
            ));
        } else {
            if names.len() >= 2 {
                let first_mode = self.policy_for(&names[0]).effective_mode();
                let uniform = names
                    .iter()
                    .all(|n| self.policy_for(n).effective_mode() == first_mode);
                let value = if uniform { first_mode.label() } else { "mixed" };
                built.push((Row::Heading, heading("Quick control")));
                built.push((
                    Row::AllAgents,
                    Item::new(
                        "All connected & configured agents",
                        value,
                        "Enter or ←/→ sets ask / scoped / auto / full-access for every listed agent",
                    ),
                ));
            }
            built.push((Row::Heading, heading("Agents")));
            for name in names {
                let policy = self.policy_for(&name);
                let tag = if self.connected.contains(&name) {
                    format!("{name} (connected)")
                } else {
                    name.to_string()
                };
                built.push((
                    Row::Agent(name),
                    Item::new(tag, policy.effective_mode().label(), mode_detail(&policy)),
                ));
            }
        }
        let selected = self.menu.selected;
        let offset = self.menu.offset;
        self.rows = built.iter().map(|(row, _)| row.clone()).collect();
        self.menu.items = built.into_iter().map(|(_, item)| item).collect();
        self.menu.selected = selected.min(self.menu.items.len().saturating_sub(1));
        self.menu.offset = offset;
    }

    pub fn key(&mut self, engine: &Engine, key: KeyEvent) -> Effect {
        if key.kind == KeyEventKind::Release {
            return Effect::None;
        }
        match key.code {
            KeyCode::Left => {
                self.step(engine, -1);
                Effect::None
            }
            KeyCode::Right => {
                self.step(engine, 1);
                Effect::None
            }
            _ => match self.menu.key(key) {
                Pick::None => Effect::None,
                Pick::Close => Effect::Close,
                Pick::Choose(_) => {
                    self.step(engine, 1);
                    Effect::None
                }
                Pick::Adjust(_, delta) => {
                    self.step(engine, if delta < 0 { -1 } else { 1 });
                    Effect::None
                }
            },
        }
    }

    pub fn mouse(
        &mut self,
        engine: &Engine,
        event: MouseEvent,
        row_of: impl Fn(u16) -> Option<usize>,
    ) -> Effect {
        match self.menu.mouse(event, row_of) {
            Pick::None => Effect::None,
            Pick::Close => Effect::Close,
            Pick::Choose(_) => {
                self.step(engine, 1);
                Effect::None
            }
            Pick::Adjust(_, delta) => {
                self.step(engine, if delta < 0 { -1 } else { 1 });
                Effect::None
            }
        }
    }

    fn step(&mut self, engine: &Engine, delta: i8) {
        let Some(row) = self.rows.get(self.menu.selected).cloned() else {
            return;
        };
        let next_mode = |mode: AgentPermissionMode| {
            if delta < 0 { mode.prev() } else { mode.next() }
        };
        let result: Result<Vec<AgentAccess>, ProtocolError> = match row {
            Row::Heading => return,
            Row::AllAgents => {
                let names = self.all_names();
                let current = names
                    .first()
                    .map(|n| self.policy_for(n).effective_mode())
                    .unwrap_or_default();
                let target = next_mode(current);
                let mut latest = Ok(Vec::new());
                for name in names {
                    latest = agents::set_mode(engine, name, target);
                    if latest.is_err() {
                        break;
                    }
                }
                latest
            }
            Row::Agent(name) => {
                let target = next_mode(self.policy_for(&name).effective_mode());
                agents::set_mode(engine, name, target)
            }
        };
        match result {
            Ok(updated) => {
                self.policies = updated
                    .into_iter()
                    .map(|access| (access.agent, access.policy))
                    .collect();
                self.menu.note = None;
                self.rebuild();
            }
            Err(error) => {
                self.menu.note = Some(error.message);
            }
        }
    }
}

fn fetch_policies(engine: &Engine) -> Result<BTreeMap<AgentName, AgentPolicy>, ProtocolError> {
    match engine.send(Command::GetAgentPolicies)? {
        CommandResult::AgentPolicies { policies } => Ok(policies
            .into_iter()
            .map(|access| (access.agent, access.policy))
            .collect()),
        other => Err(client::unexpected(&other)),
    }
}

fn connected_hosts() -> BTreeSet<AgentName> {
    let mut set = BTreeSet::new();
    if let Ok(status) = agent_setup::status_json()
        && let Some(hosts) = status["hosts"].as_array()
    {
        for host in hosts {
            if host["registered"] == serde_json::Value::Bool(true)
                && let Some(id) = host["id"].as_str()
                && let Ok(name) = AgentName::try_from(id)
            {
                set.insert(name);
            }
        }
    }
    set
}
