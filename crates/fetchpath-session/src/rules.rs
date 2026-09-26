//! Smart rules (FP-064): by domain, file type or size, a rule chooses the
//! folder, media quality, checksum requirement and connections of a new
//! download, for every principal. Rules are tried in order and the first
//! whose every condition holds decides; nothing is merged across rules, so
//! one line says why a download went where it did.

use fetchpath_protocol::model::{
    Rule, RuleActions, RuleCheck, RuleConditions, RuleSpec, RulesVerdict,
};
use std::path::{Component, Path};

pub const MAX_RULES: usize = 100;
/// Entries in one condition list.
const MAX_ENTRIES: usize = 20;
const MAX_NAME_CHARS: usize = 60;
pub const MAX_CONNECTIONS: u32 = 8;

/// What is known about a link when a rule is tried.
#[derive(Clone, Debug, Default)]
pub struct Facts {
    pub host: Option<String>,
    /// Lower case, without the dot.
    pub file_type: Option<String>,
    pub size: Option<u64>,
}

impl Facts {
    /// From the link, the file name it will be saved under (or the server's)
    /// and its size when known.
    pub fn new(url: &str, file_name: Option<&str>, size: Option<u64>) -> Self {
        let parsed = url::Url::parse(url).ok();
        let host = parsed
            .as_ref()
            .and_then(|url| url.host_str())
            .map(|host| host.trim_end_matches('.').to_ascii_lowercase());
        // Without a name, the link's last path segment stands in.
        let from_link = parsed
            .as_ref()
            .and_then(|url| url.path_segments()?.next_back().map(str::to_owned));
        let file_type = file_name
            .map(str::to_owned)
            .or(from_link)
            .as_deref()
            .and_then(|name| Path::new(name).extension())
            .and_then(|ext| ext.to_str())
            .map(str::to_ascii_lowercase)
            .filter(|ext| !ext.is_empty());
        Self {
            host,
            file_type,
            size,
        }
    }
}

/// Tries `rules` in order and stops at the first match.
pub fn decide(rules: &[Rule], facts: &Facts) -> RulesVerdict {
    let mut checks = Vec::new();
    for rule in rules {
        let (matched, reasons) = check(&rule.spec.when, facts);
        checks.push(RuleCheck {
            rule_id: rule.id,
            matched,
            reasons,
        });
        if matched {
            return RulesVerdict {
                matched: Some(rule.clone()),
                checks,
            };
        }
    }
    RulesVerdict {
        matched: None,
        checks,
    }
}

fn check(when: &RuleConditions, facts: &Facts) -> (bool, Vec<String>) {
    let mut matched = true;
    let mut reasons = Vec::new();
    let mut say = |holds: bool, reason: String| {
        matched &= holds;
        reasons.push(reason);
    };
    if !when.domains.is_empty() {
        let list = when.domains.join(" or ");
        match &facts.host {
            Some(host) => match when.domains.iter().find(|domain| in_domain(host, domain)) {
                Some(domain) if domain == host => say(true, format!("the link is on {host}")),
                Some(domain) => say(true, format!("the link is on {host}, part of {domain}")),
                None => say(false, format!("the link is on {host}, not {list}")),
            },
            None => say(
                false,
                format!("the link has no host to compare with {list}"),
            ),
        }
    }
    if !when.file_types.is_empty() {
        let list = when
            .file_types
            .iter()
            .map(|kind| format!(".{kind}"))
            .collect::<Vec<_>>()
            .join(" or ");
        match &facts.file_type {
            Some(kind) if when.file_types.contains(kind) => {
                say(true, format!("the file is a .{kind}"))
            }
            Some(kind) => say(false, format!("the file is a .{kind}, not {list}")),
            None => say(false, format!("the file type is not known, so not {list}")),
        }
    }
    if when.min_size_bytes.is_some() || when.max_size_bytes.is_some() {
        match facts.size {
            None => say(false, "the size is not known".to_owned()),
            Some(size) => {
                if let Some(min) = when.min_size_bytes {
                    if size >= min {
                        say(true, format!("{} is at least {}", bytes(size), bytes(min)));
                    } else {
                        say(false, format!("{} is under {}", bytes(size), bytes(min)));
                    }
                }
                if let Some(max) = when.max_size_bytes {
                    if size <= max {
                        say(true, format!("{} is at most {}", bytes(size), bytes(max)));
                    } else {
                        say(false, format!("{} is over {}", bytes(size), bytes(max)));
                    }
                }
            }
        }
    }
    (matched, reasons)
}

/// "Rule 2 (Big ISOs)", for messages.
pub fn label(rule: &Rule) -> String {
    match &rule.spec.name {
        Some(name) => format!("Rule {} ({name})", rule.id),
        None => format!("Rule {}", rule.id),
    }
}

fn in_domain(host: &str, domain: &str) -> bool {
    host == domain
        || host
            .strip_suffix(domain)
            .is_some_and(|rest| rest.ends_with('.'))
}

/// A byte count as people read it, in binary units.
pub fn bytes(value: u64) -> String {
    const UNITS: [&str; 4] = ["KiB", "MiB", "GiB", "TiB"];
    if value < 1024 {
        return format!("{value} B");
    }
    let mut amount = value as f64;
    let mut unit = "B";
    for next in UNITS {
        if amount < 1024.0 {
            break;
        }
        amount /= 1024.0;
        unit = next;
    }
    format!("{amount:.1} {unit}")
}

/// Checks and normalizes a rule as the person wrote it.
pub fn validate(mut spec: RuleSpec) -> Result<RuleSpec, String> {
    spec.name = spec
        .name
        .map(|name| {
            name.trim()
                .chars()
                .filter(|c| !c.is_control())
                .take(MAX_NAME_CHARS)
                .collect::<String>()
        })
        .filter(|name| !name.is_empty());

    let when = &mut spec.when;
    when.domains = normalized(std::mem::take(&mut when.domains), "domains", |raw| {
        let domain = raw
            .trim()
            .trim_start_matches("*.")
            .trim_start_matches('.')
            .trim_end_matches('.')
            .to_ascii_lowercase();
        let valid = !domain.is_empty()
            && domain.len() <= 253
            && domain
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '.'));
        valid
            .then_some(domain)
            .ok_or_else(|| format!("{raw:?} is not a domain such as example.com."))
    })?;
    when.file_types = normalized(std::mem::take(&mut when.file_types), "file types", |raw| {
        let kind = raw.trim().trim_start_matches('.').to_ascii_lowercase();
        let valid = !kind.is_empty()
            && kind.len() <= 16
            && kind
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-'));
        valid
            .then_some(kind)
            .ok_or_else(|| format!("{raw:?} is not a file type such as zip or iso."))
    })?;
    if let (Some(min), Some(max)) = (when.min_size_bytes, when.max_size_bytes)
        && min > max
    {
        return Err("The smallest size is larger than the largest.".into());
    }
    if when == &RuleConditions::default() {
        return Err("Give the rule a domain, a file type or a size to match.".into());
    }

    let then = &mut spec.then;
    if let Some(folder) = then.folder.take() {
        then.folder = Some(folder_path(&folder)?);
    }
    if let Some(quality) = then.media_quality.take() {
        let quality = quality.trim().to_ascii_lowercase();
        let height = quality
            .strip_suffix('p')
            .and_then(|digits| digits.parse::<u32>().ok())
            .filter(|height| (1..=10_000).contains(height));
        if !(quality == "best" || quality == "audio" || height.is_some()) {
            return Err(format!(
                "{quality:?} is not a quality: use best, audio or a height such as 720p."
            ));
        }
        then.media_quality = Some(quality);
    }
    if let Some(connections) = then.max_connections
        && !(1..=MAX_CONNECTIONS).contains(&connections)
    {
        return Err(format!(
            "Connections must be between 1 and {MAX_CONNECTIONS}."
        ));
    }
    if then == &RuleActions::default() {
        return Err(
            "Give the rule something to do: a folder, a quality, a checksum requirement or a \
             number of connections."
                .into(),
        );
    }
    Ok(spec)
}

fn normalized(
    items: Vec<String>,
    what: &str,
    one: impl Fn(&str) -> Result<String, String>,
) -> Result<Vec<String>, String> {
    if items.len() > MAX_ENTRIES {
        return Err(format!("A rule can list at most {MAX_ENTRIES} {what}."));
    }
    let mut out: Vec<String> = Vec::with_capacity(items.len());
    for item in items {
        let item = one(&item)?;
        if !out.contains(&item) {
            out.push(item);
        }
    }
    Ok(out)
}

/// A full folder path with its drive, as a destination must be.
fn folder_path(raw: &str) -> Result<String, String> {
    let folder = raw.trim();
    let path = Path::new(folder);
    let full = path.is_absolute()
        && matches!(path.components().next(), Some(Component::Prefix(_)))
        && !path.components().any(|part| part == Component::ParentDir)
        && !folder.chars().any(char::is_control)
        && folder.len() <= crate::MAX_DESTINATION_LENGTH;
    full.then(|| folder.to_owned())
        .ok_or_else(|| format!("{folder:?} is not a full folder path, such as D:\\ISOs."))
}

/// Keeps the rules that are still valid, with unique ids, at most
/// [`MAX_RULES`]. Used when settings are loaded, so a hand edit cannot
/// bring in a rule the commands would refuse.
pub fn sanitized(rules: Vec<Rule>) -> Vec<Rule> {
    let mut out: Vec<Rule> = Vec::new();
    for rule in rules {
        if rule.id == 0 || out.iter().any(|kept| kept.id == rule.id) {
            continue;
        }
        if let Ok(spec) = validate(rule.spec) {
            out.push(Rule { id: rule.id, spec });
        }
        if out.len() == MAX_RULES {
            break;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(id: u32, when: RuleConditions, then: RuleActions) -> Rule {
        Rule {
            id,
            spec: validate(RuleSpec {
                name: None,
                when,
                then,
            })
            .unwrap(),
        }
    }

    fn folder(path: &str) -> RuleActions {
        RuleActions {
            folder: Some(path.into()),
            ..RuleActions::default()
        }
    }

    const GIB: u64 = 1024 * 1024 * 1024;

    #[test]
    fn the_first_rule_whose_every_condition_holds_decides() {
        let rules = vec![
            rule(
                1,
                RuleConditions {
                    domains: vec!["example.com".into()],
                    file_types: vec!["zip".into()],
                    ..RuleConditions::default()
                },
                folder(r"D:\Zips"),
            ),
            rule(
                2,
                RuleConditions {
                    file_types: vec!["ISO".into(), ".img".into()],
                    min_size_bytes: Some(GIB),
                    ..RuleConditions::default()
                },
                folder(r"D:\ISOs"),
            ),
            rule(
                3,
                RuleConditions {
                    domains: vec!["*.example.com".into()],
                    ..RuleConditions::default()
                },
                folder(r"D:\Example"),
            ),
        ];
        let facts = Facts::new(
            "https://cdn.example.com/os.iso",
            Some("os.iso"),
            Some(4 * GIB),
        );
        let verdict = decide(&rules, &facts);
        assert_eq!(verdict.matched.as_ref().map(|rule| rule.id), Some(2));
        assert_eq!(verdict.checks.len(), 2, "trying stops at the match");
        assert_eq!(
            verdict.checks[0].reasons,
            [
                "the link is on cdn.example.com, part of example.com",
                "the file is a .iso, not .zip"
            ]
        );
        assert_eq!(
            verdict.checks[1].reasons,
            ["the file is a .iso", "4.0 GiB is at least 1.0 GiB"]
        );

        // Smaller than the second rule allows: the third decides.
        let small = Facts::new(
            "https://cdn.example.com/os.iso",
            Some("os.iso"),
            Some(GIB / 2),
        );
        assert_eq!(decide(&rules, &small).matched.map(|rule| rule.id), Some(3));
        // A size rule never matches an unknown size.
        let unknown = Facts::new("https://other.test/os.iso", Some("os.iso"), None);
        let verdict = decide(&rules, &unknown);
        assert_eq!(verdict.matched, None);
        assert!(
            verdict.checks[1]
                .reasons
                .contains(&"the size is not known".to_owned())
        );
    }

    #[test]
    fn a_domain_matches_itself_and_its_subdomains_only() {
        assert!(in_domain("example.com", "example.com"));
        assert!(in_domain("a.b.example.com", "example.com"));
        assert!(!in_domain("badexample.com", "example.com"));
        assert!(!in_domain("example.com.evil.test", "example.com"));
    }

    #[test]
    fn a_rule_needs_a_condition_an_action_and_valid_values() {
        let only_action = RuleSpec {
            then: folder(r"D:\x"),
            ..RuleSpec::default()
        };
        assert!(validate(only_action).unwrap_err().contains("to match"));
        let when = RuleConditions {
            file_types: vec!["zip".into()],
            ..RuleConditions::default()
        };
        let spec = |then| RuleSpec {
            name: None,
            when: when.clone(),
            then,
        };
        assert!(
            validate(spec(RuleActions::default()))
                .unwrap_err()
                .contains("something to do")
        );
        assert!(validate(spec(folder("relative"))).is_err());
        assert!(validate(spec(folder(r"D:\a\..\b"))).is_err());
        for quality in ["best", "Audio", "720p"] {
            let then = RuleActions {
                media_quality: Some(quality.into()),
                ..RuleActions::default()
            };
            assert!(validate(spec(then)).is_ok(), "{quality}");
        }
        let then = RuleActions {
            media_quality: Some("hd".into()),
            ..RuleActions::default()
        };
        assert!(validate(spec(then)).is_err());
        let then = RuleActions {
            max_connections: Some(0),
            ..RuleActions::default()
        };
        assert!(validate(spec(then)).is_err());
        let bad_domain = RuleSpec {
            when: RuleConditions {
                domains: vec!["https://example.com/".into()],
                ..RuleConditions::default()
            },
            then: folder(r"D:\x"),
            name: None,
        };
        assert!(validate(bad_domain).is_err());
    }

    #[test]
    fn loading_drops_invalid_and_duplicate_rules() {
        let good = rule(
            1,
            RuleConditions {
                file_types: vec!["zip".into()],
                ..RuleConditions::default()
            },
            folder(r"D:\Zips"),
        );
        let mut duplicate = good.clone();
        duplicate.spec.then = folder(r"D:\Other");
        let invalid = Rule {
            id: 2,
            spec: RuleSpec::default(),
        };
        assert_eq!(
            sanitized(vec![good.clone(), duplicate, invalid]),
            vec![good]
        );
    }
}
