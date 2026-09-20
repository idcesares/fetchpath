use fetchpath_core::{FileJob, FileJobState};
use serde_json::json;
use std::path::PathBuf;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() != 3 || args[0] != "download" {
        eprintln!("usage: fetchpath download URL DESTINATION");
        std::process::exit(64);
    }
    let job = FileJob::create(args[1].clone(), PathBuf::from(&args[2]));
    let cancel = job.clone();
    ctrlc::set_handler(move || {
        cancel.cancel();
    })
    .expect("Ctrl-C handler");
    if let Err(error) = job.start() {
        eprintln!("{error}");
        std::process::exit(1);
    }
    job.join();
    let done = job.snapshot();
    if done.state == FileJobState::Completed {
        let cleanup = done
            .staging_cleanup_pending
            .as_ref()
            .map(|path| path.display().to_string());
        println!(
            "{}",
            json!({
                "result": "downloaded_observed",
                "job_id": done.job_id,
                "destination": done.destination.unwrap().display().to_string(),
                "bytes": done.bytes_received,
                "observed_sha256": done.observed_sha256.unwrap(),
                "staging_cleanup_pending": cleanup,
            })
        );
    } else {
        eprintln!(
            "{}",
            done.error
                .unwrap_or_else(|| format!("job ended as {:?}", done.state))
        );
        std::process::exit(1);
    }
}
