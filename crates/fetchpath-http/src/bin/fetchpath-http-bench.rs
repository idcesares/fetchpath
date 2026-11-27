use fetchpath_http::{
    ConnectionPool, GlobalBudget, PoolKey, RequestContext, ResumePoint, SegmentMonitor,
    TransferLimits, transfer_resumable,
};
use serde_json::json;
use std::fs::OpenOptions;
use std::io::{self, Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

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
    if args.first().is_some_and(|arg| arg == "--pool-batch") {
        let files: usize = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(50);
        let handshake_ms: u64 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(5);
        let reps: usize = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(3);
        return run_pool_batch(files, handshake_ms, reps);
    }
    let http2_prior_knowledge = args
        .first()
        .is_some_and(|arg| arg == "--http2-prior-knowledge");
    if http2_prior_knowledge {
        args.remove(0);
    }
    let resume = if args.first().is_some_and(|arg| arg == "--resume") {
        if args.len() != 7 {
            return Err("usage: --resume OFFSET ETAG TOTAL PREFIX URL OUTPUT".into());
        }
        let point = ResumePoint {
            offset: args[1].parse()?,
            strong_etag: args[2].clone(),
            expected_total: Some(args[3].parse()?),
            completed_extents: Vec::new(),
        };
        let prefix = PathBuf::from(&args[4]);
        if std::fs::metadata(&prefix)?.len() != point.offset {
            return Err("prefix length differs from resume offset".into());
        }
        args.drain(..5);
        Some((point, prefix))
    } else {
        None
    };
    if args.len() != 2 {
        return Err("usage: fetchpath-http-bench [--http2-prior-knowledge] [--resume OFFSET ETAG TOTAL PREFIX] URL OUTPUT".into());
    }
    let destination = PathBuf::from(&args[1]);
    let mut file = OpenOptions::new()
        .create_new(true)
        .read(true)
        .write(true)
        .open(&destination)?;
    if let Some((_, prefix)) = &resume {
        io::copy(&mut std::fs::File::open(prefix)?, &mut file)?;
    }
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
    let budget = GlobalBudget::new(
        limits.max_active_requests.max(limits.max_concurrency),
        limits.max_buffered_bytes,
    )?;
    let report = transfer_resumable(
        &args[0],
        &RequestContext {
            http2_prior_knowledge,
            ..RequestContext::default()
        },
        limits,
        &budget,
        &SegmentMonitor::default(),
        resume.as_ref().map(|(point, _)| point),
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

fn run_pool_batch(
    files: usize,
    handshake_ms: u64,
    reps: usize,
) -> Result<(), Box<dyn std::error::Error>> {
    let body = vec![0x42_u8; 16 * 1024];
    let handshake = Duration::from_millis(handshake_ms);
    let mut seq_runs = Vec::new();
    let mut conc_runs = Vec::new();

    for concurrency in [1usize, 4usize] {
        for rep in 0..reps {
            let iso = measure_batch(files, concurrency, &body, handshake, false)?;
            let pooled = measure_batch(files, concurrency, &body, handshake, true)?;
            let entry = json!({
                "rep": rep + 1,
                "isolatedMs": iso.0,
                "isolatedTcpAccepts": iso.1,
                "isolatedCurlConnects": iso.2,
                "pooledMs": pooled.0,
                "pooledTcpAccepts": pooled.1,
                "pooledCurlConnects": pooled.2,
            });
            if concurrency == 1 {
                seq_runs.push((iso.0, pooled.0, entry));
            } else {
                conc_runs.push((iso.0, pooled.0, entry));
            }
        }
    }

    let summarize = |runs: &[(f64, f64, serde_json::Value)]| {
        let mut iso: Vec<f64> = runs.iter().map(|r| r.0).collect();
        let mut pooled: Vec<f64> = runs.iter().map(|r| r.1).collect();
        iso.sort_by(f64::total_cmp);
        pooled.sort_by(f64::total_cmp);
        let iso_med = iso[iso.len() / 2];
        let pooled_med = pooled[pooled.len() / 2];
        let reduction_pct = (1.0 - pooled_med / iso_med) * 100.0;
        json!({
            "isolatedMedianMs": (iso_med * 100.0).round() / 100.0,
            "pooledMedianMs": (pooled_med * 100.0).round() / 100.0,
            "wallTimeReductionPct": (reduction_pct * 10.0).round() / 10.0,
            "runs": runs.iter().map(|r| r.2.clone()).collect::<Vec<_>>(),
        })
    };

    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "benchmark": "fp151-pool-batch",
            "files": files,
            "fileBytes": body.len(),
            "connectHandshakeMs": handshake_ms,
            "reps": reps,
            "sequential": summarize(&seq_runs),
            "concurrent4": summarize(&conc_runs),
        }))?
    );
    Ok(())
}

fn measure_batch(
    files: usize,
    concurrency: usize,
    body: &[u8],
    handshake: Duration,
    use_pool: bool,
) -> Result<(f64, usize, u32), Box<dyn std::error::Error>> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let addr = listener.local_addr()?;
    listener.set_nonblocking(true)?;
    let accepts = Arc::new(AtomicUsize::new(0));
    let stop = Arc::new(AtomicBool::new(false));
    let accepts_w = Arc::clone(&accepts);
    let stop_w = Arc::clone(&stop);
    let payload = body.to_vec();

    let server = std::thread::spawn(move || {
        let mut handlers = Vec::new();
        while !stop_w.load(Ordering::Relaxed) {
            match listener.accept() {
                Ok((mut stream, _)) => {
                    accepts_w.fetch_add(1, Ordering::SeqCst);
                    let stop = Arc::clone(&stop_w);
                    let body = payload.clone();
                    handlers.push(std::thread::spawn(move || {
                        let _ = stream.set_nonblocking(false);
                        let _ = stream.set_read_timeout(Some(Duration::from_millis(200)));
                        if !handshake.is_zero() {
                            std::thread::sleep(handshake);
                        }
                        while !stop.load(Ordering::Relaxed) {
                            let mut req = Vec::new();
                            let mut buf = [0_u8; 1024];
                            let mut ok = true;
                            while !req.windows(4).any(|w| w == b"\r\n\r\n") {
                                if stop.load(Ordering::Relaxed) {
                                    ok = false;
                                    break;
                                }
                                match stream.read(&mut buf) {
                                    Ok(0) => {
                                        ok = false;
                                        break;
                                    }
                                    Ok(n) => req.extend_from_slice(&buf[..n]),
                                    Err(e)
                                        if matches!(
                                            e.kind(),
                                            io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                                        ) =>
                                    {
                                        continue;
                                    }
                                    Err(_) => {
                                        ok = false;
                                        break;
                                    }
                                }
                            }
                            if !ok {
                                break;
                            }
                            let head = format!(
                                "HTTP/1.1 200 OK\r\nETag: \"bench-v1\"\r\nContent-Length: {}\r\nConnection: keep-alive\r\n\r\n",
                                body.len()
                            );
                            if stream.write_all(head.as_bytes()).is_err()
                                || stream.write_all(&body).is_err()
                            {
                                break;
                            }
                        }
                    }));
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(1));
                }
                Err(_) => break,
            }
        }
        for h in handlers {
            let _ = h.join();
        }
    });

    let pool = ConnectionPool::new();
    let partition = use_pool.then(|| pool.partition(PoolKey::public("user")).unwrap());
    let next = Arc::new(AtomicUsize::new(0));
    let curl_connects = Arc::new(AtomicUsize::new(0));
    let started = Instant::now();
    let expected_len = body.len() as u64;
    let mut workers = Vec::new();
    for _ in 0..concurrency {
        let part = partition.clone();
        let next = Arc::clone(&next);
        let connects = Arc::clone(&curl_connects);
        workers.push(std::thread::spawn(move || {
            let limits = TransferLimits::default();
            let budget = GlobalBudget::new(8, 32 * 1024 * 1024).unwrap();
            let ctx = RequestContext {
                pool: part,
                ..RequestContext::default()
            };
            loop {
                let i = next.fetch_add(1, Ordering::Relaxed);
                if i >= files {
                    break;
                }
                let mut got = 0u64;
                let report = transfer_resumable(
                    &format!("http://{addr}/item-{i}.bin"),
                    &ctx,
                    limits,
                    &budget,
                    &SegmentMonitor::default(),
                    None,
                    || false,
                    |chunk| {
                        got += chunk.bytes.len() as u64;
                        Ok(())
                    },
                )
                .unwrap();
                assert_eq!(got, expected_len);
                connects.fetch_add(report.connections_opened as usize, Ordering::Relaxed);
            }
        }));
    }
    for w in workers {
        w.join().unwrap();
    }
    let elapsed_ms = started.elapsed().as_secs_f64() * 1000.0;
    let tcp_accepts = accepts.load(Ordering::SeqCst);
    stop.store(true, Ordering::SeqCst);
    let _ = server.join();
    Ok((
        (elapsed_ms * 100.0).round() / 100.0,
        tcp_accepts,
        curl_connects.load(Ordering::SeqCst) as u32,
    ))
}
