//! `fetchpath rules`: smart rules, kept and applied by the engine (FP-064).
//! By domain, file type or size, a rule chooses where a new download is
//! saved, the video quality, whether it needs a checksum and how many
//! connections it opens. Rules are tried in order; the first match decides.

use crate::client::{self, Engine};
use crate::download::EXIT_USAGE;
use fetchpath_protocol::ProtocolError;
use fetchpath_protocol::command::Command;
use fetchpath_protocol::message::CommandResult;
use fetchpath_protocol::model::{LinkInspection, Rule, RuleSpec, SensitiveUrl};

pub const USAGE: &str =
    "usage: fetchpath rules [list | add CONDITION... ACTION... | rm ID | test LINK]
conditions: --domain example.com  --type iso,img  --min-size 1GB  --max-size 50MB
actions:    --folder D:\\ISOs  --quality 720p|best|audio  --require-checksum  --connections N
also:       --name NAME  --position N (where it goes in the order, from 1)";

pub fn run(args: &[String]) -> i32 {
    let json = args.iter().any(|arg| arg == "--json");
    let args: Vec<&str> = args
        .iter()
        .map(String::as_str)
        .filter(|arg| *arg != "--json")
        .collect();
    let outcome = match args.as_slice() {
        [] | ["list"] => list(json),
        ["add", rest @ ..] => match parse_add(rest) {
            Ok((spec, position)) => add(spec, position, json),
            Err(message) => {
                eprintln!("fetchpath: {message}\n{USAGE}");
                return EXIT_USAGE;
            }
        },
        ["rm", id] => match id.trim_start_matches('#').parse() {
            Ok(id) => remove(id, json),
            Err(_) => {
                eprintln!("fetchpath: {id} is not a rule number\n{USAGE}");
                return EXIT_USAGE;
            }
        },
        ["test", link] => test(link, json),
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

fn print_rules(rules: &[Rule], json: bool) {
    if json {
        client::print_json(&CommandResult::Rules {
            rules: rules.to_vec(),
        });
        return;
    }
    for line in list_lines(rules) {
        println!("{line}");
    }
}

fn list(json: bool) -> Result<i32, ProtocolError> {
    let rules = fetch(&Engine::connect()?, Command::ListRules)?;
    print_rules(&rules, json);
    Ok(0)
}

fn add(spec: RuleSpec, position: Option<u32>, json: bool) -> Result<i32, ProtocolError> {
    let rules = fetch(
        &Engine::connect()?,
        Command::AddRule {
            rule: Box::new(spec),
            position,
        },
    )?;
    print_rules(&rules, json);
    Ok(0)
}

fn remove(id: u32, json: bool) -> Result<i32, ProtocolError> {
    let rules = fetch(&Engine::connect()?, Command::RemoveRule { rule_id: id })?;
    print_rules(&rules, json);
    Ok(0)
}

fn test(link: &str, json: bool) -> Result<i32, ProtocolError> {
    let inspection = inspect(&Engine::connect()?, link)?;
    if json {
        client::print_json(&CommandResult::LinkInspection { inspection });
        return Ok(0);
    }
    for line in verdict_lines(inspection.rules.as_ref()) {
        println!("{line}");
    }
    Ok(0)
}

/// Looks at a link, which also says how the rules decide for it.
pub fn inspect(engine: &Engine, link: &str) -> Result<LinkInspection, ProtocolError> {
    let url = SensitiveUrl::try_from(link.to_owned())
        .map_err(|message| client::input_error(&format!("That link cannot be used: {message}.")))?;
    match engine.send(Command::InspectLink { url })? {
        CommandResult::LinkInspection { inspection } => Ok(inspection),
        other => Err(client::unexpected(&other)),
    }
}

/// Sends a rules command and returns the rules after it.
pub fn fetch(engine: &Engine, command: Command) -> Result<Vec<Rule>, ProtocolError> {
    match engine.send(command)? {
        CommandResult::Rules { rules } => Ok(rules),
        other => Err(client::unexpected(&other)),
    }
}

// ---------------------------------------------------------------- words

pub use fetchpath_protocol::describe::{actions, conditions, label};

pub fn list_lines(rules: &[Rule]) -> Vec<String> {
    if rules.is_empty() {
        return vec![
            "No rules yet. Add one, for example:".to_owned(),
            r"  fetchpath rules add --type iso,img --folder D:\ISOs".to_owned(),
        ];
    }
    let mut lines = vec!["Rules, tried in order; the first that matches decides:".to_owned()];
    for rule in rules {
        lines.push(format!(
            "  {}: {} → {}",
            label(rule),
            conditions(&rule.spec.when),
            actions(&rule.spec.then)
        ));
    }
    lines
}

pub use fetchpath_protocol::describe::verdict as verdict_lines;

/// One line for a confirmation card: which rule matched and why.
pub use fetchpath_protocol::describe::matched as card_line;

// ---------------------------------------------------------------- add

/// Reads `rules add` flags into a rule and its position.
pub fn parse_add(args: &[&str]) -> Result<(RuleSpec, Option<u32>), String> {
    let mut spec = RuleSpec::default();
    let mut position = None;
    let mut words = args.iter();
    while let Some(flag) = words.next() {
        let mut value = || {
            words
                .next()
                .map(|value| value.to_string())
                .ok_or_else(|| format!("{flag} needs a value"))
        };
        match *flag {
            "--name" => spec.name = Some(value()?),
            "--domain" => spec.when.domains.extend(items(&value()?)),
            "--type" => spec.when.file_types.extend(items(&value()?)),
            "--min-size" => spec.when.min_size_bytes = Some(size(&value()?)?),
            "--max-size" => spec.when.max_size_bytes = Some(size(&value()?)?),
            "--folder" => {
                spec.then.folder = Some(
                    crate::download::absolute(std::path::Path::new(&value()?))?
                        .display()
                        .to_string(),
                )
            }
            "--quality" => spec.then.media_quality = Some(value()?),
            "--require-checksum" => spec.then.require_checksum = true,
            "--connections" => {
                let text = value()?;
                spec.then.max_connections = Some(
                    text.parse()
                        .map_err(|_| format!("{text} is not a number of connections"))?,
                );
            }
            "--position" => {
                let text = value()?;
                position = Some(
                    text.parse()
                        .map_err(|_| format!("{text} is not a position"))?,
                );
            }
            other => return Err(format!("unknown option {other}")),
        }
    }
    Ok((spec, position))
}

fn items(text: &str) -> Vec<String> {
    text.split(',')
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(str::to_owned)
        .collect()
}

/// `1GB`, `500 MB`, `1.5g`, `700k` or plain bytes. Units count in 1024s, as
/// File Explorer shows sizes.
pub fn size(text: &str) -> Result<u64, String> {
    let lower = text.trim().to_ascii_lowercase();
    let split = lower
        .find(|c: char| !(c.is_ascii_digit() || c == '.'))
        .unwrap_or(lower.len());
    let (number, unit) = lower.split_at(split);
    let factor: u64 = match unit.trim().trim_end_matches("ib").trim_end_matches('b') {
        "" => 1,
        "k" => 1 << 10,
        "m" => 1 << 20,
        "g" => 1 << 30,
        "t" => 1 << 40,
        _ => return Err(format!("{text} is not a size such as 500MB or 2GB")),
    };
    let number: f64 = number
        .parse()
        .map_err(|_| format!("{text} is not a size such as 500MB or 2GB"))?;
    if !number.is_finite() || number < 0.0 {
        return Err(format!("{text} is not a size such as 500MB or 2GB"));
    }
    Ok((number * factor as f64).round() as u64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use fetchpath_protocol::model::{RuleActions, RuleCheck, RuleConditions, RulesVerdict};

    #[test]
    fn sizes_read_as_people_write_them() {
        assert_eq!(size("1GB"), Ok(1 << 30));
        assert_eq!(size("500 MB"), Ok(500 << 20));
        assert_eq!(size("1.5g"), Ok(3 << 29));
        assert_eq!(size("700KiB"), Ok(700 << 10));
        assert_eq!(size("42"), Ok(42));
        assert!(size("lots").is_err());
        assert!(size("5 parsecs").is_err());
    }

    #[test]
    fn add_reads_conditions_actions_and_position() {
        let (spec, position) = parse_add(&[
            "--type",
            "iso, img",
            "--domain",
            "example.com",
            "--min-size",
            "1GB",
            "--folder",
            r"D:\ISOs",
            "--connections",
            "2",
            "--require-checksum",
            "--position",
            "1",
            "--name",
            "Disc images",
        ])
        .unwrap();
        assert_eq!(spec.when.file_types, ["iso", "img"]);
        assert_eq!(spec.when.domains, ["example.com"]);
        assert_eq!(spec.when.min_size_bytes, Some(1 << 30));
        assert_eq!(spec.then.folder.as_deref(), Some(r"D:\ISOs"));
        assert_eq!(spec.then.max_connections, Some(2));
        assert!(spec.then.require_checksum);
        assert_eq!(position, Some(1));
        assert!(parse_add(&["--type"]).is_err());
        assert!(parse_add(&["--colour", "red"]).is_err());
    }

    #[test]
    fn a_verdict_says_which_rule_decides_and_why_each_was_tried() {
        let rule = Rule {
            id: 2,
            spec: RuleSpec {
                name: Some("Disc images".into()),
                when: RuleConditions {
                    file_types: vec!["iso".into()],
                    min_size_bytes: Some(1 << 30),
                    ..RuleConditions::default()
                },
                then: RuleActions {
                    folder: Some(r"D:\ISOs".into()),
                    max_connections: Some(2),
                    ..RuleActions::default()
                },
            },
        };
        let verdict = RulesVerdict {
            matched: Some(rule.clone()),
            checks: vec![
                RuleCheck {
                    rule_id: 1,
                    matched: false,
                    reasons: vec!["the link is on a.test, not example.com".into()],
                },
                RuleCheck {
                    rule_id: 2,
                    matched: true,
                    reasons: vec![
                        "the file is a .iso".into(),
                        "4.0 GiB is at least 1.0 GiB".into(),
                    ],
                },
            ],
        };
        assert_eq!(
            verdict_lines(Some(&verdict)),
            [
                r"Rule 2 (Disc images) decides: save in D:\ISOs; at most 2 connections.",
                "  Rule 1: no — the link is on a.test, not example.com",
                "  Rule 2: matches — the file is a .iso; 4.0 GiB is at least 1.0 GiB",
            ]
        );
        assert_eq!(
            card_line(Some(&verdict)).as_deref(),
            Some("Rule 2 (Disc images): the file is a .iso, 4.0 GiB is at least 1.0 GiB")
        );
        assert_eq!(
            list_lines(&[rule])[1],
            r"  Rule 2 (Disc images): a .iso file, 1.0 GiB or more → save in D:\ISOs; at most 2 connections"
        );
    }
}
