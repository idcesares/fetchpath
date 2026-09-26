//! Plain mode: append-only lines, nothing redrawn, for screen readers,
//! `NO_COLOR` and dumb terminals. Lines are read with the console's own
//! editing, so its history and screen-reader support apply.

use super::Session;
use super::line::Out;
use super::live::{Notice, Row};
use super::review::{self, Card, Draft};
use super::view::{self, ASCII};
use crate::client::{self, EXIT_ENGINE};
use std::collections::VecDeque;
use std::io::{BufRead, Write};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::Duration;

/// A panel row as one line: number, name, then state or progress.
pub fn row_line(row: &Row) -> String {
    format!(
        "{} {}: {}",
        row.index,
        client::name(row.job),
        view::detail(row.job)
    )
}

/// What a notice says in plain mode: every event, not only problems.
pub fn notice_line(notice: &Notice) -> String {
    match notice {
        Notice::Receipt(job) => view::receipt(job, &ASCII).text,
        Notice::Event { name, text, .. } => format!("{name}: {text}"),
    }
}

/// A card as plain lines, with what to type instead of which keys to press.
pub fn card_lines(card: &Card) -> Vec<String> {
    // Plain lines wrap in the console itself; nothing is cut.
    let text = view::card_text(card, &ASCII, 0, usize::MAX / 2);
    let mut lines = vec![text.title];
    lines.extend(text.body.iter().map(|line| {
        let flat: String = line
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect();
        format!("  {}", flat.trim_end())
    }));
    lines.push(if card.tools_missing() && !card.can_confirm() {
        "Type i to see what would be downloaded and set up the video tools, or press Enter to skip."
            .to_owned()
    } else if card.tools_missing() {
        "Press Enter to save the page, type i to set up the video tools, or no to cancel."
            .to_owned()
    } else if !card.can_confirm() {
        "Press Enter to close.".to_owned()
    } else if card.is_media() {
        "Press Enter for the chosen format, type its number for another, or no to cancel."
            .to_owned()
    } else {
        "Press Enter to download, or type no to cancel.".to_owned()
    });
    lines
}

/// What a typed answer to a card means.
#[derive(Debug, Eq, PartialEq)]
pub enum Reply {
    Confirm,
    Cancel,
    Unclear,
    SetUpTools,
}

pub fn answer(card: &mut Card, text: &str) -> Reply {
    let text = text.trim().to_ascii_lowercase();
    if card.tools_missing() && text == "i" {
        return Reply::SetUpTools;
    }
    if !card.can_confirm() {
        return Reply::Cancel;
    }
    match text.as_str() {
        "" | "y" | "yes" => Reply::Confirm,
        "n" | "no" | "cancel" | "esc" => Reply::Cancel,
        number => match number.parse::<usize>() {
            Ok(row) if row >= 1 && row <= card.variants().len() => {
                card.choose(row - 1);
                Reply::Confirm
            }
            _ => Reply::Unclear,
        },
    }
}

/// Looks at the next waiting link (blocking; plain mode redraws nothing,
/// so a pause here costs no animation) and prints its card.
fn next_card(session: &Session, waiting: &mut VecDeque<Draft>) -> Option<Card> {
    let draft = waiting.pop_front()?;
    println!("Looking at {}...", review::host(&draft.link));
    let mut card = Card::new(draft);
    card.set(review::look(&session.engine, &card.draft));
    for line in card_lines(&card) {
        println!("{line}");
    }
    Some(card)
}

/// Says what setting up the video tools will download and asks.
fn ask_setup() -> bool {
    match crate::tools::install_dir() {
        Ok(dir) => {
            for line in crate::tools::disclosure(&dir) {
                println!("{line}");
            }
            println!("Type yes to download and set them up, or no.");
            true
        }
        Err(error) => {
            println!("{}", error.message);
            false
        }
    }
}

/// Runs the setup to its end, one line per step, and says what is ready.
fn run_setup() -> bool {
    let progress = match crate::tools::install_in_background() {
        Ok(progress) => progress,
        Err(error) => {
            println!("{}", error.message);
            return false;
        }
    };
    // Ctrl+C cancels the helper being downloaded; the ending still arrives
    // as the install's result.
    let mut current: Option<fetchpath_core::CancellationToken> = None;
    loop {
        let message = match progress.recv_timeout(Duration::from_millis(100)) {
            Ok(message) => Ok(message),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if client::interrupted()
                    && let Some(token) = &current
                {
                    token.cancel();
                }
                continue;
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => Err(()),
        };
        match message {
            Ok(crate::tools::Progress::Step { label, token }) => {
                if client::interrupted() {
                    token.cancel();
                }
                current = Some(token);
                println!("Downloading {label}... (Ctrl+C cancels)");
            }
            Ok(crate::tools::Progress::Done(Ok(ready))) => {
                for line in ready {
                    println!("{line}");
                }
                return true;
            }
            Ok(crate::tools::Progress::Done(Err(message))) => {
                println!("{message}");
                return false;
            }
            Err(_) => {
                println!("The setup stopped unexpectedly.");
                return false;
            }
        }
    }
}

fn say(lines: &[Out]) {
    let mut out = std::io::stdout().lock();
    for line in lines {
        let _ = writeln!(out, "{}", line.text);
    }
    let _ = out.flush();
}

pub fn run(mut session: Session) -> i32 {
    client::catch_interrupt();
    println!(
        "Fetchpath {}. Type a link to download it, /help for commands, /quit to leave.",
        crate::VERSION
    );
    let panel = session.live.panel();
    if !panel.is_empty() {
        println!("In the queue:");
        for row in &panel {
            println!("  {}", row_line(row));
        }
    }
    // Reading blocks, so a thread reads while this one follows the engine.
    let (lines, input) = mpsc::channel();
    std::thread::spawn(move || {
        for line in std::io::stdin().lock().lines() {
            if lines.send(line).is_err() {
                break;
            }
        }
    });
    let mut waiting = VecDeque::new();
    let mut card: Option<Card> = None;
    // Waiting for yes or no to the video tools setup, with the video link
    // that asked for it, if one did.
    let mut asking: Option<Option<Draft>> = None;
    loop {
        if client::interrupted() {
            return 0;
        }
        match input.recv_timeout(Duration::from_millis(100)) {
            Ok(Ok(text)) if asking.is_some() => {
                let then = asking.take().expect("checked");
                match text.trim().to_ascii_lowercase().as_str() {
                    "y" | "yes" => {
                        if run_setup()
                            && let Some(draft) = then
                        {
                            waiting.push_front(draft);
                        }
                    }
                    _ => println!("Video tools not set up; nothing was downloaded."),
                }
                if card.is_none() {
                    card = next_card(&session, &mut waiting);
                }
            }
            Ok(Ok(text)) if card.is_some() => {
                let current = card.as_mut().expect("checked");
                match answer(current, &text) {
                    Reply::Confirm => {
                        say(&[super::inline::confirmed(&session, current)]);
                        card = next_card(&session, &mut waiting);
                    }
                    Reply::Cancel => {
                        println!("Not downloaded: {}", current.draft.link);
                        card = next_card(&session, &mut waiting);
                    }
                    Reply::Unclear => {
                        println!("Type a number from the list, press Enter, or type no.");
                    }
                    Reply::SetUpTools => {
                        let draft = current.draft.clone();
                        card = None;
                        if ask_setup() {
                            asking = Some(Some(draft));
                        }
                    }
                }
            }
            Ok(Ok(text)) => {
                let reply = session.run_line(&text, false);
                say(&reply.lines);
                if reply.quit {
                    return 0;
                }
                waiting.extend(reply.drafts);
                card = next_card(&session, &mut waiting);
                if reply.set_up_tools && card.is_none() && ask_setup() {
                    asking = Some(None);
                }
            }
            // The end of input, or an unreadable console, ends the session.
            Ok(Err(_)) | Err(RecvTimeoutError::Disconnected) => return 0,
            Err(RecvTimeoutError::Timeout) => {}
        }
        if let Some(lines) = session.tools_notice() {
            for line in lines {
                println!("{line}");
            }
        }
        match session.poll(Duration::ZERO) {
            Ok(notices) => {
                for notice in &notices {
                    println!("{}", notice_line(notice));
                }
            }
            Err(error) => {
                eprintln!("fetchpath: lost the engine: {}", error.message);
                return EXIT_ENGINE;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::live::{Live, tests::recorded};
    use fetchpath_protocol::message::ServerMessage;

    #[test]
    fn a_typed_answer_confirms_picks_a_format_or_cancels() {
        use fetchpath_protocol::model::{
            LinkInspection, LinkKind, MediaInspection, MediaVariant, MediaVariantKind,
        };
        let link = "https://youtu.be/x";
        let draft = Draft {
            link: link.into(),
            url: fetchpath_protocol::SensitiveUrl::try_from(link.to_owned()).unwrap(),
            to: None,
            at: None,
            sha256: None,
            quality: None,
        };
        let variant = |id: &str, height| MediaVariant {
            id: id.into(),
            label: format!("{height}p"),
            kind: MediaVariantKind::Video,
            extension: "mp4".into(),
            height: Some(height),
            fps: None,
        };
        let mut card = Card::new(draft);
        assert_eq!(
            answer(&mut card, ""),
            Reply::Cancel,
            "nothing to confirm yet"
        );
        card.set(Ok(review::Look {
            link: LinkInspection {
                kind: LinkKind::MediaPage,
                file_name: None,
                content_type: None,
                size_bytes: None,
                resumable: false,
            },
            media: Some(MediaInspection {
                title: "T".into(),
                duration_seconds: None,
                variants: vec![variant("a", 720), variant("b", 1080)],
            }),
            media_error: None,
            tools_missing: false,
            folder: None,
        }));
        let lines = card_lines(&card);
        assert!(lines[0].starts_with("Video"), "{lines:?}");
        assert!(
            lines.iter().any(|line| line.contains("(*)  1  1080p")),
            "{lines:?}"
        );
        assert!(lines.last().unwrap().contains("type its number"));
        assert_eq!(answer(&mut card, "maybe"), Reply::Unclear);
        assert_eq!(answer(&mut card, "9"), Reply::Unclear);
        assert_eq!(answer(&mut card, "2"), Reply::Confirm);
        assert_eq!(card.variants()[card.choice].label, "720p");
        assert_eq!(answer(&mut card, " No "), Reply::Cancel);
        assert_eq!(answer(&mut card, ""), Reply::Confirm);
    }

    /// The whole recorded stream, as plain mode prints it once each job's
    /// snapshot has settled a failure.
    #[test]
    fn plain_mode_prints_one_line_per_change_and_a_receipt_per_finish() {
        let mut live = Live::default();
        let mut printed = Vec::new();
        for message in recorded() {
            match message {
                ServerMessage::Event(event) => {
                    let (_, notices) = live.apply(&event);
                    printed.extend(notices.iter().map(notice_line));
                }
                ServerMessage::Progress(sample) => live.progress(&sample),
                ServerMessage::Reply(_) => {}
            }
        }
        let rows: Vec<String> = live.panel().iter().map(row_line).collect();
        assert_eq!(rows, ["1 missing.zip: failed"]);
        let mut failed = live.jobs()[0].clone();
        failed.retry_at = None;
        printed.extend(live.refresh(failed).iter().map(notice_line));
        assert_eq!(
            printed,
            [
                r"+ sample.bin  2.9 MiB  C:\Users\person\Downloads\sample.bin",
                "x missing.zip  source.transfer_failed: HTTP status 404  (/retry 4175930f)",
            ]
        );
        // Nothing is redrawn: every line is plain text without escapes.
        assert!(printed.iter().all(|line| !line.contains('\u{1b}')));
    }
}
