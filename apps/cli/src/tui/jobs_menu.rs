//! `/queue` in the inline view, and job commands given no job: a list of
//! downloads to choose from, then what can be done to the chosen one.

use super::line::{Out, Tone};
use super::live::{self, Live};
use super::menu::{Item, Menu, Pick};
use crate::client::{self, Engine};
use crate::queue::{self, Control};
use fetchpath_protocol::model::JobState;
use fetchpath_protocol::{JobId, JobSnapshot};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Deed {
    Control(Control),
    Show,
    /// Open File Explorer at the file.
    Reveal,
}

fn deed_for(command: &str) -> Deed {
    match command {
        "pause" => Deed::Control(Control::Pause),
        "resume" => Deed::Control(Control::Resume),
        "cancel" => Deed::Control(Control::Cancel),
        "retry" => Deed::Control(Control::Retry),
        "rm" => Deed::Control(Control::Remove),
        "folder" => Deed::Reveal,
        _ => Deed::Show,
    }
}

/// What makes sense for a job in its state. The engine still decides; this
/// only keeps pointless choices off the menu.
pub fn deeds(job: &JobSnapshot) -> Vec<(Deed, &'static str)> {
    use JobState::*;
    let mut deeds = Vec::new();
    let finished = matches!(job.state, Completed | Cancelled)
        || (job.state == Failed && job.retry_at.is_none());
    // A saved file is what people most often want next.
    if job.state == Completed && job.destination.is_some() {
        deeds.push((Deed::Reveal, "Show in folder"));
    }
    match job.state {
        Running | Probing | Ready | Queued if job.not_before.is_none() => {
            deeds.push((Deed::Control(Control::Pause), "Pause"));
        }
        _ => {}
    }
    if job.state == Paused || (job.state == Queued && job.not_before.is_some()) {
        deeds.push((Deed::Control(Control::Resume), "Resume now"));
    }
    if matches!(job.state, Failed | Cancelled) {
        deeds.push((Deed::Control(Control::Retry), "Retry"));
    }
    if !finished && !matches!(job.state, Cancelling | Publishing) {
        deeds.push((Deed::Control(Control::Cancel), "Cancel"));
    }
    if job.state != Completed && job.destination.is_some() {
        deeds.push((Deed::Reveal, "Open its folder"));
    }
    deeds.push((Deed::Show, "Show details"));
    if finished {
        deeds.push((Deed::Control(Control::Remove), "Remove from the list"));
    }
    deeds
}

fn applies(deed: Deed, job: &JobSnapshot) -> bool {
    deed == Deed::Show || deeds(job).iter().any(|(candidate, _)| *candidate == deed)
}

enum Stage {
    List(Vec<JobId>),
    Actions(Box<JobSnapshot>, Vec<Deed>),
}

pub struct JobsMenu {
    pub menu: Menu,
    stage: Stage,
    /// Set when opened by a command: choosing a job does it at once.
    deed: Option<Deed>,
}

/// What the job menu asks of the view.
pub enum Effect {
    None,
    Close,
    /// Lines for scrollback; the menu stays open unless `close`.
    Say {
        lines: Vec<Out>,
        close: bool,
    },
}

impl JobsMenu {
    /// Lists jobs still in the panel first, then recent finished ones. With
    /// a command, only the jobs it applies to.
    pub fn open(live: &Live, command: Option<&str>) -> Self {
        let deed = command.map(deed_for);
        let entry = |index: usize, job: &JobSnapshot| -> Option<(JobId, Item)> {
            if deed.is_some_and(|deed| !applies(deed, job)) {
                return None;
            }
            Some((
                job.job_id.clone(),
                Item::new(
                    format!("{index:>3}  {}", client::name(job)),
                    super::view::detail(job),
                    job.destination
                        .clone()
                        .unwrap_or_else(|| job.source_display.clone()),
                ),
            ))
        };
        let moving: Vec<(JobId, Item)> = live
            .panel()
            .iter()
            .filter_map(|row| entry(row.index, row.job))
            .collect();
        let finished: Vec<(JobId, Item)> = live
            .jobs()
            .iter()
            .enumerate()
            .filter(|(_, job)| live::group(job).is_none())
            .take(30)
            .filter_map(|(position, job)| entry(position + 1, job))
            .collect();
        let mut all_items = Vec::new();
        let mut all_ids = Vec::new();
        for (heading, group) in [("In progress", moving), ("Finished", finished)] {
            if group.is_empty() {
                continue;
            }
            all_items.push(Item {
                inert: true,
                ..Item::new(heading, "", "")
            });
            all_ids.push(JobId::random());
            for (id, item) in group {
                all_ids.push(id);
                all_items.push(item);
            }
        }
        let title = match command {
            Some("pause") => "Pause which download?",
            Some("resume") => "Resume which download?",
            Some("cancel") => "Cancel which download?",
            Some("retry") => "Retry which download?",
            Some("rm") => "Remove which download from the list?",
            Some("folder") => "Show which download in File Explorer?",
            Some(_) => "Show which download?",
            None => "Downloads",
        };
        if all_items.is_empty() {
            all_items.push(Item {
                inert: true,
                ..Item::new("Nothing here to choose.", "", "")
            });
            all_ids.push(JobId::random());
        }
        Self {
            menu: Menu::new(title, all_items, "↑↓ choose · Enter or click · Esc close"),
            stage: Stage::List(all_ids),
            deed,
        }
    }

    /// Keeps the list's progress current while it is open.
    pub fn refresh(&mut self, live: &Live) {
        let Stage::List(ids) = &self.stage else {
            return;
        };
        for (item, id) in self.menu.items.iter_mut().zip(ids) {
            if let Some(job) = live.jobs().iter().find(|job| &job.job_id == id) {
                item.value = super::view::detail(job);
            }
        }
    }

    fn actions(&mut self, job: JobSnapshot) {
        let offered = deeds(&job);
        let items = offered
            .iter()
            .map(|(_, label)| Item::new(*label, "", ""))
            .collect();
        let title = format!("{}  ·  {}", client::name(&job), client::state_label(&job));
        self.menu = Menu::new(title, items, "↑↓ choose · Enter or click · Esc back");
        self.stage = Stage::Actions(
            Box::new(job),
            offered.into_iter().map(|(deed, _)| deed).collect(),
        );
    }

    pub(super) fn act(engine: &Engine, deed: Deed, job: &JobSnapshot) -> Vec<Out> {
        match deed {
            Deed::Reveal => vec![match crate::reveal::show_job(job) {
                Ok(said) => Out::new(Tone::Normal, said),
                Err(message) => Out::new(Tone::Bad, message),
            }],
            Deed::Show => queue::job_lines(job)
                .into_iter()
                .map(|line| Out::new(Tone::Normal, line))
                .collect(),
            Deed::Control(action) => match engine.send(queue::control_command(action, job)) {
                Ok(result) => vec![Out::new(
                    Tone::Normal,
                    queue::describe(action, job, &result),
                )],
                Err(error) => vec![Out::new(
                    Tone::Bad,
                    format!(
                        "{} {}: {}",
                        client::short_id(job),
                        client::name(job),
                        error.message
                    ),
                )],
            },
        }
    }

    pub fn pick(&mut self, engine: &Engine, live: &Live, pick: Pick) -> Effect {
        match (&self.stage, pick) {
            (Stage::List(_), Pick::Close) => Effect::Close,
            (Stage::Actions(..), Pick::Close) => {
                *self = Self::open(live, None);
                Effect::None
            }
            (Stage::List(ids), Pick::Choose(index)) => {
                let Some(job) = live
                    .jobs()
                    .iter()
                    .find(|job| job.job_id == ids[index])
                    .cloned()
                else {
                    return Effect::None;
                };
                match self.deed {
                    Some(deed) => Effect::Say {
                        lines: Self::act(engine, deed, &job),
                        close: true,
                    },
                    None => {
                        self.actions(job);
                        Effect::None
                    }
                }
            }
            (Stage::Actions(job, deeds), Pick::Choose(index)) => {
                let lines = Self::act(engine, deeds[index], job);
                *self = Self::open(live, None);
                Effect::Say { lines, close: true }
            }
            _ => Effect::None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn job(state: &str, extra: serde_json::Value) -> JobSnapshot {
        let mut value = serde_json::json!({
            "job_id": "3f1c2a9b-0000-4000-8000-000000000001",
            "kind": "file", "state": state, "job_revision": 1, "last_seq": 1,
            "source_display": "https://a.test/x.zip",
            "progress": { "bytes_received": 0 },
            "created_at": "2026-09-26T10:00:00Z",
        });
        for (key, field) in extra.as_object().unwrap() {
            value[key] = field.clone();
        }
        serde_json::from_value(value).unwrap()
    }

    fn labels(job: &JobSnapshot) -> Vec<&'static str> {
        deeds(job).into_iter().map(|(_, label)| label).collect()
    }

    #[test]
    fn each_state_offers_only_what_makes_sense() {
        assert_eq!(
            labels(&job("running", serde_json::json!({}))),
            ["Pause", "Cancel", "Show details"]
        );
        let saved = job(
            "completed",
            serde_json::json!({"destination": "C:\\Users\\person\\Downloads\\x.zip"}),
        );
        assert_eq!(
            labels(&saved),
            ["Show in folder", "Show details", "Remove from the list"]
        );
        let moving = job(
            "running",
            serde_json::json!({"destination": "C:\\Users\\person\\Downloads\\x.zip"}),
        );
        assert_eq!(
            labels(&moving),
            ["Pause", "Cancel", "Open its folder", "Show details"]
        );
        assert_eq!(
            labels(&job("paused", serde_json::json!({}))),
            ["Resume now", "Cancel", "Show details"]
        );
        assert_eq!(
            labels(&job(
                "queued",
                serde_json::json!({"not_before": "2026-09-27T10:00:00Z"})
            )),
            ["Resume now", "Cancel", "Show details"]
        );
        assert_eq!(
            labels(&job("failed", serde_json::json!({}))),
            ["Retry", "Show details", "Remove from the list"]
        );
        assert_eq!(
            labels(&job("completed", serde_json::json!({}))),
            ["Show details", "Remove from the list"]
        );
    }

    #[test]
    fn a_command_lists_only_the_jobs_it_applies_to() {
        let live = Live::new(vec![
            job(
                "completed",
                serde_json::json!({"job_id": "00000002-0000-4000-8000-000000000000"}),
            ),
            job(
                "running",
                serde_json::json!({"job_id": "00000001-0000-4000-8000-000000000000"}),
            ),
        ]);
        let pause = JobsMenu::open(&live, Some("pause"));
        let names: Vec<&str> = pause
            .menu
            .items
            .iter()
            .map(|item| item.label.as_str())
            .collect();
        assert_eq!(names.len(), 2, "{names:?}");
        assert!(names[1].trim_start().starts_with('2'), "{names:?}");
        let all = JobsMenu::open(&live, None);
        assert_eq!(all.menu.items.len(), 4, "two headings and two jobs");
    }
}
