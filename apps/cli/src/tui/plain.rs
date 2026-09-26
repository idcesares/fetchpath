//! Plain mode: append-only lines, nothing redrawn, for screen readers,
//! `NO_COLOR` and dumb terminals. Lines are read with the console's own
//! editing, so its history and screen-reader support apply.

use super::Session;
use super::line::Out;
use super::live::{Notice, Row};
use super::view::{self, ASCII};
use crate::client::{self, EXIT_ENGINE};
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
    loop {
        if client::interrupted() {
            return 0;
        }
        match input.recv_timeout(Duration::from_millis(100)) {
            Ok(Ok(text)) => {
                let reply = session.run_line(&text);
                say(&reply.lines);
                if reply.quit {
                    return 0;
                }
            }
            // The end of input, or an unreadable console, ends the session.
            Ok(Err(_)) | Err(RecvTimeoutError::Disconnected) => return 0,
            Err(RecvTimeoutError::Timeout) => {}
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
