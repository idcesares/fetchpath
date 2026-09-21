use fetchpath_http::{GlobalBudget, RequestContext, TransferLimits, transfer_adaptive};
use serde_json::json;
use std::fs::OpenOptions;
use std::io::{Seek, SeekFrom, Write};
use std::path::PathBuf;

fn main() {
    if let Err(error) = run() {
        eprintln!("{error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let http2_prior_knowledge = args
        .first()
        .is_some_and(|arg| arg == "--http2-prior-knowledge");
    if http2_prior_knowledge {
        args.remove(0);
    }
    if args.len() != 2 {
        return Err("usage: fetchpath-http-bench [--http2-prior-knowledge] URL OUTPUT".into());
    }
    let destination = PathBuf::from(&args[1]);
    let mut file = OpenOptions::new()
        .create_new(true)
        .read(true)
        .write(true)
        .open(&destination)?;
    let limits = TransferLimits::default();
    let budget = GlobalBudget::new(limits.max_active_requests, limits.max_buffered_bytes)?;
    let report = transfer_adaptive(
        &args[0],
        &RequestContext {
            http2_prior_knowledge,
            ..RequestContext::default()
        },
        limits,
        &budget,
        || false,
        |chunk| {
            file.seek(SeekFrom::Start(chunk.offset))?;
            file.write_all(chunk.bytes)
        },
    )?;
    file.sync_all()?;
    println!(
        "{}",
        json!({
            "bytes": report.bytes,
            "totalBytes": report.total_bytes,
            "preferredProtocol": report.preferred_protocol.as_str(),
            "negotiatedProtocol": report.negotiated_protocol.as_str(),
            "fallbackReasons": report.fallback_reasons,
            "usedRanges": report.used_ranges,
            "adaptive": report.adaptive,
            "peakConcurrency": report.peak_concurrency,
            "budget": {
                "maxActiveRequests": limits.max_active_requests,
                "maxBufferedBytes": limits.max_buffered_bytes,
                "peakActiveRequests": report.budget.peak_active_requests,
                "peakBufferedBytes": report.budget.peak_buffered_bytes,
            },
            "observations": report.observations.iter().map(|observation| json!({
                "concurrency": observation.concurrency,
                "bytes": observation.bytes,
                "elapsedMs": observation.elapsed_ms,
            })).collect::<Vec<_>>(),
        })
    );
    Ok(())
}
