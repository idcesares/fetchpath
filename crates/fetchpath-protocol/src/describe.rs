//! Rules and sizes in the words every client shows, so the desktop, the
//! command line and the engine's own messages say the same thing.

use crate::model::{Rule, RuleActions, RuleConditions, RulesVerdict};

/// A byte count as people read it, in binary units: "512 B", "1.5 MiB".
pub fn bytes(value: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut size = value as f64;
    let mut unit = 0;
    while size >= 1024.0 && unit < UNITS.len() - 1 {
        size /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{value} B")
    } else {
        format!("{size:.1} {}", UNITS[unit])
    }
}

/// "Rule 2 (Big ISOs)".
pub fn label(rule: &Rule) -> String {
    match &rule.spec.name {
        Some(name) => format!("Rule {} ({name})", rule.id),
        None => format!("Rule {}", rule.id),
    }
}

/// "a .iso or .img file, on example.com, 1.0 GiB or more".
pub fn conditions(when: &RuleConditions) -> String {
    let mut parts = Vec::new();
    if !when.file_types.is_empty() {
        let types: Vec<String> = when
            .file_types
            .iter()
            .map(|kind| format!(".{kind}"))
            .collect();
        parts.push(format!("a {} file", types.join(" or ")));
    }
    if !when.domains.is_empty() {
        parts.push(format!("on {}", when.domains.join(" or ")));
    }
    match (when.min_size_bytes, when.max_size_bytes) {
        (Some(min), Some(max)) => parts.push(format!("{} to {}", bytes(min), bytes(max))),
        (Some(min), None) => parts.push(format!("{} or more", bytes(min))),
        (None, Some(max)) => parts.push(format!("{} or less", bytes(max))),
        (None, None) => {}
    }
    parts.join(", ")
}

/// "save in D:\ISOs; video up to 720p; needs a checksum; at most 2 connections".
pub fn actions(then: &RuleActions) -> String {
    let mut parts = Vec::new();
    if let Some(folder) = &then.folder {
        parts.push(format!("save in {folder}"));
    }
    if let Some(quality) = &then.media_quality {
        parts.push(match quality.as_str() {
            "best" => "best video".to_owned(),
            "audio" => "audio only".to_owned(),
            height => format!("video up to {height}"),
        });
    }
    if then.require_checksum {
        parts.push("needs a checksum".to_owned());
    }
    if let Some(connections) = then.max_connections {
        parts.push(format!(
            "at most {connections} connection{}",
            if connections == 1 { "" } else { "s" }
        ));
    }
    parts.join("; ")
}

/// Which rule decides for a link and why, with every rule tried.
pub fn verdict(verdict: Option<&RulesVerdict>) -> Vec<String> {
    let Some(verdict) = verdict else {
        return vec!["The engine did not say how its rules decide for this link.".to_owned()];
    };
    if verdict.checks.is_empty() {
        return vec!["There are no rules; the link goes to the default folder.".to_owned()];
    }
    let mut lines = match &verdict.matched {
        Some(rule) => vec![format!(
            "{} decides: {}.",
            label(rule),
            actions(&rule.spec.then)
        )],
        None => vec!["No rule matches; the link goes to the default folder.".to_owned()],
    };
    for check in &verdict.checks {
        lines.push(format!(
            "  Rule {}: {} — {}",
            check.rule_id,
            if check.matched { "matches" } else { "no" },
            check.reasons.join("; ")
        ));
    }
    lines
}

/// Which rule matched and why, in one line: "Rule 1 (Disc images): a .iso file".
pub fn matched(verdict: Option<&RulesVerdict>) -> Option<String> {
    let verdict = verdict?;
    let rule = verdict.matched.as_ref()?;
    let why = verdict
        .checks
        .iter()
        .find(|check| check.rule_id == rule.id)
        .map(|check| check.reasons.join(", "))
        .unwrap_or_default();
    Some(format!("{}: {why}", label(rule)))
}
