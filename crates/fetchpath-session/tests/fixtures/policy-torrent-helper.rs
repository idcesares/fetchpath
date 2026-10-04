// Deterministic isolated helper: proves the actual request cap and restart boundary.
use std::io::{Read, Write};
fn main() {
    let mut request = String::new();
    std::io::stdin().read_to_string(&mut request).unwrap();
    if request.contains("publication-window.torrent") {
        // The retained marker belongs to the original automatic root. A
        // rewritten explicit request must fail instead of masking the bug.
        if !request.contains("\"auto_name\":true") {
            println!("{{\"type\":\"failed\",\"code\":\"destination.conflict\"}}");
            return;
        }
        let root = std::path::PathBuf::from(string_field(&request, "destination"));
        assert!(std::fs::read_dir(&root).unwrap().any(|entry| {
            entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .ends_with(".part.infohash")
        }));
        let published = root.join("Album");
        assert!(published.is_dir());
        println!(
            "{{\"type\":\"published\",\"path\":{:?}}}",
            published.to_str().unwrap()
        );
        std::io::stdout().flush().unwrap();
        if !root.join("replay-publication").exists() {
            std::thread::sleep(std::time::Duration::from_secs(60));
        }
        println!("{{\"type\":\"completed\",\"received\":2097152,\"total\":2097152}}");
    } else if request.contains("\"max_bytes\":null") {
        println!("{{\"type\":\"completed\",\"received\":2097152,\"total\":2097152}}");
    } else {
        assert!(
            request.contains("\"max_bytes\":262144"),
            "unexpected cap: {request}"
        );
        println!("{{\"type\":\"progress\",\"received\":1,\"total\":1}}");
        std::io::stdout().flush().unwrap();
        // Must be stopped and replaced, since the running request cannot change.
        std::thread::sleep(std::time::Duration::from_secs(60));
        println!("{{\"type\":\"failed\",\"code\":\"size_limit\"}}");
    }
}

// The fixture uses only ordinary JSON path strings emitted by serde_json.
fn string_field(request: &str, field: &str) -> String {
    let prefix = format!("\"{field}\":\"");
    let start = request.find(&prefix).unwrap() + prefix.len();
    let mut result = String::new();
    let mut escaped = false;
    for ch in request[start..].chars() {
        if escaped {
            result.push(ch);
            escaped = false;
        } else if ch == '\\' {
            escaped = true;
        } else if ch == '"' {
            return result;
        } else {
            result.push(ch);
        }
    }
    panic!("missing string terminator");
}
