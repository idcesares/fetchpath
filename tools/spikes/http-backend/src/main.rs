use curl::easy::Easy;
use std::env;
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

fn quote(value: &str) -> String {
    format!(
        "\"{}\"",
        value
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('\n', "\\n")
    )
}

fn capabilities() {
    let version = curl::Version::get();
    println!(
        "{{\"libcurl_version\":{},\"ssl_version\":{},\"http2\":{},\"http3\":{}}}",
        quote(version.version()),
        quote(version.ssl_version().unwrap_or("unknown")),
        version.feature_http2(),
        version.feature_http3()
    );
}

fn get(url: &str, proxy: Option<&str>, cancel_after: Option<u64>) -> Result<(), curl::Error> {
    let bytes = Arc::new(AtomicU64::new(0));
    let mut easy = Easy::new();
    easy.url(url)?;
    easy.follow_location(true)?;
    easy.ssl_verify_peer(true)?;
    easy.ssl_verify_host(true)?;
    if let Some(value) = proxy {
        easy.proxy(value)?;
    }

    let counter = Arc::clone(&bytes);
    easy.write_function(move |data| {
        counter.fetch_add(data.len() as u64, Ordering::Relaxed);
        Ok(data.len())
    })?;

    if let Some(limit) = cancel_after {
        let progress = Arc::clone(&bytes);
        easy.progress(true)?;
        easy.progress_function(move |_, downloaded, _, _| {
            downloaded < limit as f64 && progress.load(Ordering::Relaxed) < limit
        })?;
    }

    let performed = easy.perform();
    let received = bytes.load(Ordering::Relaxed);
    match performed {
        Ok(()) => println!(
            "{{\"result\":\"ok\",\"response_code\":{},\"received_bytes\":{}}}",
            easy.response_code()?,
            received
        ),
        Err(error) => {
            println!(
                "{{\"result\":\"error\",\"error\":{},\"received_bytes\":{}}}",
                quote(&error.to_string()),
                received
            );
            return Err(error);
        }
    }
    Ok(())
}

fn usage() -> ! {
    eprintln!(
        "usage: fetchpath-http-spike capabilities | get URL [--proxy URL | --no-proxy] [--cancel-after BYTES]"
    );
    std::process::exit(64);
}

fn main() {
    let args: Vec<String> = env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("capabilities") && args.len() == 1 {
        capabilities();
        return;
    }
    if args.first().map(String::as_str) != Some("get") || args.len() < 2 {
        usage();
    }

    let mut proxy = None;
    let mut cancel_after = None;
    let mut index = 2;
    while index < args.len() {
        match args[index].as_str() {
            "--proxy" if index + 1 < args.len() => {
                proxy = Some(args[index + 1].as_str());
                index += 2;
            }
            "--no-proxy" => {
                proxy = Some("");
                index += 1;
            }
            "--cancel-after" if index + 1 < args.len() => {
                cancel_after = args[index + 1].parse().ok();
                if cancel_after.is_none() {
                    usage();
                }
                index += 2;
            }
            _ => usage(),
        }
    }

    if let Err(error) = get(&args[1], proxy, cancel_after) {
        eprintln!("libcurl transfer failed: {error}");
        std::process::exit(1);
    }
}
