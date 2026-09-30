use fetchpath_http::{GlobalBudget, RequestContext, TransferLimits, transfer_adaptive};
use serde_json::json;
use std::fs::OpenOptions;
use std::io;
use std::path::PathBuf;

/// Positional write that loops until every byte is written, like the engine's.
fn write_all_at(file: &std::fs::File, mut bytes: &[u8], mut offset: u64) -> io::Result<()> {
    while !bytes.is_empty() {
        #[cfg(windows)]
        let written = std::os::windows::fs::FileExt::seek_write(file, bytes, offset)?;
        #[cfg(unix)]
        let written = std::os::unix::fs::FileExt::write_at(file, bytes, offset)?;
        if written == 0 {
            return Err(io::ErrorKind::WriteZero.into());
        }
        bytes = &bytes[written..];
        offset += written as u64;
    }
    Ok(())
}

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
    let file = OpenOptions::new()
        .create_new(true)
        .read(true)
        .write(true)
        .open(&destination)?;
    let mut limits = TransferLimits::default();
    // Benchmark-only overrides, to compare receive buffers and concurrency.
    if let Some(kib) = std::env::var("FETCHPATH_BENCH_BUFFER_KIB")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
    {
        limits.receive_buffer_bytes = kib * 1024;
    }
    if let Some(lanes) = std::env::var("FETCHPATH_BENCH_MAX_LANES")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
    {
        limits.max_concurrency = lanes;
    }
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
        |chunk| write_all_at(&file, chunk.bytes, chunk.offset),
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
            "splits": report.splits,
            "replacements": report.replacements,
            "retries": report.retries,
            "throttles": report.throttles,
            "requests": report.requests,
            "connectionsOpened": report.connections_opened,
            "scheduler": format!("{:?}", limits.scheduler),
            "receiveBufferBytes": limits.receive_buffer_bytes,
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
