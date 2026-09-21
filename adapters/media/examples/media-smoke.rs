use fetchpath_media::{MediaJob, MediaJobState, MediaKind, MediaTools};
use serde_json::json;
use std::path::PathBuf;
use std::thread;
use std::time::{Duration, Instant};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let source = args.next().ok_or("usage: media-smoke <url> <output-dir>")?;
    let output_dir = PathBuf::from(args.next().ok_or("usage: media-smoke <url> <output-dir>")?);
    std::fs::create_dir_all(&output_dir)?;
    let tools = MediaTools::discover()?;
    let versions = tools.versions()?;
    let inspection = tools.inspect(&source)?;
    let video = inspection
        .variants
        .iter()
        .find(|variant| variant.kind == MediaKind::Video)
        .ok_or("no video choice")?
        .clone();
    let audio = inspection
        .variants
        .iter()
        .find(|variant| variant.kind == MediaKind::Audio)
        .ok_or("no audio choice")?
        .clone();

    let video_snapshot = run(
        &source,
        &video.id,
        output_dir.join("selected-video.mp4"),
        &tools,
    )?;
    let audio_snapshot = run(
        &source,
        &audio.id,
        output_dir.join("selected-audio.mp3"),
        &tools,
    )?;

    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "source": "local generated fixture",
            "title": inspection.title,
            "choices": inspection.variants,
            "selectedVideo": video.id,
            "selectedAudio": audio.id,
            "videoBytes": video_snapshot.bytes_received,
            "videoSha256": video_snapshot.observed_sha256,
            "audioBytes": audio_snapshot.bytes_received,
            "audioSha256": audio_snapshot.observed_sha256,
            "ytDlpVersion": versions.0,
            "ffmpegVersion": versions.1,
        }))?
    );
    Ok(())
}

fn run(
    source: &str,
    variant_id: &str,
    destination: PathBuf,
    tools: &MediaTools,
) -> Result<fetchpath_media::MediaJobSnapshot, Box<dyn std::error::Error>> {
    if destination.exists() {
        std::fs::remove_file(&destination)?;
    }
    let job = MediaJob::create(source.into(), variant_id.into(), destination, tools.clone());
    job.start()?;
    let started = Instant::now();
    loop {
        let snapshot = job.snapshot();
        if matches!(
            snapshot.state,
            MediaJobState::Completed | MediaJobState::Cancelled | MediaJobState::Failed
        ) {
            job.join();
            if snapshot.state != MediaJobState::Completed {
                return Err(snapshot
                    .error
                    .unwrap_or_else(|| "media job failed".into())
                    .into());
            }
            return Ok(snapshot);
        }
        if started.elapsed() > Duration::from_secs(180) {
            job.cancel();
            job.join();
            return Err("media job timed out".into());
        }
        thread::sleep(Duration::from_millis(50));
    }
}
