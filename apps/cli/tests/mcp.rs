//! `fetchpath mcp` (FP-065) driven by a scripted MCP client over stdio,
//! against a real engine in its own data folder: the handshake, the tool
//! list and its schemas, progress notifications, and the agent boundary
//! (grants, approvals, credentials, other principals' jobs, paths).
#![cfg(windows)]

mod common;

use common::*;
use fetchpath_protocol::command::Command as Wire;
use fetchpath_protocol::launch::{self, EngineHome};
use fetchpath_protocol::pipe::Limits;
use fetchpath_protocol::principal::{AgentName, AgentPolicy};
use fetchpath_protocol::{ClientId, EngineClient};
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::Duration;

/// An MCP client speaking newline-delimited JSON-RPC to `fetchpath mcp`.
struct Client {
    child: Child,
    stdin: ChildStdin,
    lines: Receiver<String>,
    next_id: u64,
}

impl Client {
    fn start(home: &Home, agent: &str) -> Self {
        let mut child = Command::new(EXE)
            .args(["mcp", "--agent", agent])
            .current_dir(home.out())
            .env("FETCHPATH_APP_DATA_DIR", home.dir.path().join("data"))
            .env("USERPROFILE", home.dir.path().join("profile"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let (send, lines) = mpsc::channel();
        thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if send.send(line).is_err() {
                    break;
                }
            }
        });
        Self {
            child,
            stdin,
            lines,
            next_id: 1,
        }
    }

    fn write(&mut self, message: &Value) {
        writeln!(self.stdin, "{message}").unwrap();
        self.stdin.flush().unwrap();
    }

    /// Sends a request and returns its result (or error) with the
    /// notifications that arrived before it.
    fn request(&mut self, method: &str, params: Value) -> (Value, Vec<Value>) {
        let id = self.next_id;
        self.next_id += 1;
        self.write(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        let mut notes = Vec::new();
        loop {
            let line = self
                .lines
                .recv_timeout(Duration::from_secs(60))
                .unwrap_or_else(|_| panic!("no answer to {method}"));
            // Standard output carries nothing but protocol messages.
            let message: Value =
                serde_json::from_str(&line).unwrap_or_else(|_| panic!("not JSON: {line}"));
            if message["id"] == json!(id) {
                return (message, notes);
            }
            notes.push(message);
        }
    }

    fn call(&mut self, tool: &str, arguments: Value) -> Value {
        self.call_with(tool, arguments, None).0
    }

    fn call_with(
        &mut self,
        tool: &str,
        arguments: Value,
        token: Option<&str>,
    ) -> (Value, Vec<Value>) {
        let mut params = json!({"name": tool, "arguments": arguments});
        if let Some(token) = token {
            params["_meta"] = json!({"progressToken": token});
        }
        let (message, notes) = self.request("tools/call", params);
        assert!(message.get("error").is_none(), "{tool}: {message}");
        (message["result"].clone(), notes)
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn grant(home: &Home, agent: &str, folder: &std::path::Path) {
    let data = EngineHome::at(home.dir.path().join("data"));
    launch::attach(&data, Limits::default())
        .unwrap()
        .send(
            &ClientId::random(),
            Wire::SetAgentPolicy {
                agent: AgentName::try_from(agent).unwrap(),
                policy: Some(AgentPolicy {
                    folders: vec![folder.display().to_string()],
                    ..AgentPolicy::default()
                }),
            },
        )
        .unwrap();
}

#[test]
fn an_agent_downloads_through_mcp_inside_its_grant_and_asks_for_the_rest() {
    let home = Home::new();
    let granted = home.dir.path().join("granted");
    let elsewhere = home.dir.path().join("elsewhere");
    std::fs::create_dir_all(&granted).unwrap();
    std::fs::create_dir_all(&elsewhere).unwrap();
    grant(&home, "helper", &granted);
    let base = server(body(200_000), Duration::from_millis(150));

    // The person's own download, which the agent must never see.
    let mine = home.run(&[
        "add",
        &format!("{base}/persons.bin"),
        "--to",
        &elsewhere.display().to_string(),
    ]);
    assert_eq!(code(&mine), 0, "{}", text(&mine.stderr));

    let mut client = Client::start(&home, "helper");
    let (hello, _) = client.request(
        "initialize",
        json!({
            "protocolVersion": "2025-06-18",
            "capabilities": {},
            "clientInfo": {"name": "fetchpath-test", "version": "1"}
        }),
    );
    let info = &hello["result"];
    assert_eq!(info["serverInfo"]["name"], "fetchpath", "{hello}");
    assert!(info["capabilities"]["tools"].is_object());
    assert!(info["instructions"].as_str().unwrap().contains("untrusted"));
    client.write(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));

    // Every tool the spec names, each with an object input and output schema.
    let (listed, _) = client.request("tools/list", json!({}));
    let tools = listed["result"]["tools"].as_array().unwrap();
    let mut names: Vec<&str> = tools
        .iter()
        .map(|tool| tool["name"].as_str().unwrap())
        .collect();
    names.sort_unstable();
    assert_eq!(
        names,
        [
            "cancel",
            "download",
            "get_download",
            "inspect_link",
            "list_downloads",
            "pause",
            "resume",
            "search_history",
            "wait_for_download"
        ]
    );
    for tool in tools {
        assert_eq!(tool["inputSchema"]["type"], "object", "{tool}");
        assert_eq!(tool["outputSchema"]["type"], "object", "{tool}");
        // Only formats JSON Schema defines, so hosts' validators stay quiet.
        let text = tool.to_string();
        assert!(
            !text.contains("\"format\":\"uint") && !text.contains("\"format\":\"int"),
            "{text}"
        );
    }
    let list_tool = tools
        .iter()
        .find(|tool| tool["name"] == "list_downloads")
        .unwrap();
    // The filter's values come from the protocol's own JobFilter.
    let schema = list_tool["inputSchema"].to_string();
    for value in ["awaiting_approval", "finished", "active", "failed"] {
        assert!(schema.contains(value), "{schema}");
    }

    let link = format!("{base}/report.bin");
    let seen = client.call("inspect_link", json!({"url": link}));
    assert_eq!(seen["structuredContent"]["kind"], "file", "{seen}");
    assert_eq!(seen["structuredContent"]["size_bytes"], 200_000);

    // Inside the grant: it starts at once, reports progress, and says where
    // it was saved.
    let (saved, notes) = client.call_with(
        "download",
        json!({"url": link, "wait": true, "timeout_seconds": 60}),
        Some("dl-1"),
    );
    let saved = &saved["structuredContent"];
    assert_eq!(saved["state"], "completed", "{saved}");
    let path = saved["saved_path"].as_str().unwrap();
    assert!(path.starts_with(&granted.display().to_string()), "{path}");
    assert_eq!(std::fs::read(path).unwrap(), body(200_000));
    assert_eq!(saved["integrity"]["outcome"], "downloaded_observed");
    assert!(
        saved["integrity"]["meaning"]
            .as_str()
            .unwrap()
            .contains("not evidence of what the publisher intended")
    );
    assert_eq!(saved["untrusted"]["file_name"], "report.bin");
    let progress: Vec<&Value> = notes
        .iter()
        .filter(|note| note["method"] == "notifications/progress")
        .collect();
    assert!(progress.len() >= 2, "{notes:?}");
    for note in &progress {
        assert_eq!(note["params"]["progressToken"], "dl-1");
        // The size is known once the server has answered.
        let total = &note["params"]["total"];
        assert!(total.is_null() || *total == 200_000.0, "{note}");
    }
    assert!(
        progress
            .iter()
            .any(|note| note["params"]["total"] == 200_000.0)
    );
    // Progress text is Fetchpath's own: sizes, never a name from outside.
    assert!(
        progress
            .iter()
            .all(|note| !note.to_string().contains("report"))
    );
    let id = saved["id"].as_str().unwrap().to_owned();

    // Outside the grant: it waits for the person, and the path stays hidden.
    let asked = client.call(
        "download",
        json!({"url": format!("{base}/other.bin"), "folder": elsewhere.display().to_string()}),
    );
    let asked = &asked["structuredContent"];
    assert_eq!(asked["state"], "awaiting_approval", "{asked}");
    assert_eq!(asked["destination_granted"], false);
    assert!(asked.get("destination").is_none() && asked.get("saved_path").is_none());
    assert!(!asked.to_string().contains("elsewhere"), "{asked}");
    assert_eq!(
        asked["approval"]["reasons"],
        json!(["outside_granted_folders"])
    );
    assert_eq!(asked["settled"], true);
    let waiting = asked["id"].as_str().unwrap().to_owned();

    // Credentials are refused outright, not offered for approval.
    let refused = client.call(
        "download",
        json!({"url": format!("http://user:secret@{}/x.bin", base.trim_start_matches("http://"))}),
    );
    assert_eq!(refused["isError"], true, "{refused}");
    let said = refused["content"][0]["text"].as_str().unwrap();
    assert!(said.contains("policy.credentials_not_allowed"), "{said}");
    assert!(!said.contains("secret"), "{said}");

    // A name with folders in it is refused.
    let refused = client.call(
        "download",
        json!({"url": link, "file_name": "..\\..\\escape.bin"}),
    );
    assert_eq!(refused["isError"], true, "{refused}");

    // Only the agent's own downloads are listed or found.
    let listed = client.call("list_downloads", json!({}));
    assert_eq!(listed["structuredContent"]["total"], 2, "{listed}");
    assert!(!listed.to_string().contains("persons.bin"));
    let pending = client.call("list_downloads", json!({"filter": "awaiting_approval"}));
    assert_eq!(pending["structuredContent"]["total"], 1);

    let got = client.call("get_download", json!({"id": &id[..8]}));
    assert_eq!(got["structuredContent"]["id"], id.as_str());
    let waited = client.call("wait_for_download", json!({"id": id, "timeout_seconds": 5}));
    assert_eq!(waited["structuredContent"]["state"], "completed");

    let found = client.call("search_history", json!({"text": "report"}));
    assert_eq!(found["structuredContent"]["total"], 1, "{found}");

    // Withdrawing the waiting request cancels it.
    let withdrawn = client.call("cancel", json!({"id": waiting}));
    assert_eq!(
        withdrawn["structuredContent"]["state"], "cancelled",
        "{withdrawn}"
    );
    let unknown = client.call("pause", json!({"id": "ffffffff"}));
    assert_eq!(unknown["isError"], true);
}

#[test]
fn a_bad_agent_name_is_refused_before_serving() {
    let home = Home::new();
    let output = home.run(&["mcp", "--agent", "Not Valid"]);
    assert_eq!(code(&output), 2);
    assert!(output.stdout.is_empty());
}

/// Flooding the server (FP-067): more waits than it runs at once are refused
/// at once rather than queued, a huge argument is an error rather than a
/// crash, and the server keeps answering afterwards.
#[test]
fn a_flood_of_waits_and_a_huge_argument_are_refused_and_the_server_carries_on() {
    let home = Home::new();
    let granted = home.dir.path().join("granted");
    std::fs::create_dir_all(&granted).unwrap();
    grant(&home, "helper", &granted);
    // Hold completion until the server has refused the seventeenth wait,
    // independent of parallel transfer speed or process startup delays.
    let (base, gate) = held_server(body(1024 * 1024));
    let mut gate = Some(gate);
    let mut client = Client::start(&home, "helper");
    client.request(
        "initialize",
        json!({
            "protocolVersion": "2025-06-18",
            "capabilities": {},
            "clientInfo": {"name": "fetchpath-test", "version": "1"}
        }),
    );
    client.write(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));
    let started = client.call("download", json!({"url": format!("{base}/slow.bin")}));
    let id = started["structuredContent"]["id"]
        .as_str()
        .unwrap()
        .to_owned();

    // Seventeen waits sent together; one more than the server runs at once.
    let first = client.next_id;
    for offset in 0..17 {
        client.write(&json!({
            "jsonrpc": "2.0",
            "id": first + offset,
            "method": "tools/call",
            "params": {"name": "wait_for_download", "arguments": {"id": id, "timeout_seconds": 60}}
        }));
    }
    client.next_id += 17;
    let mut refused = 0;
    let mut settled = 0;
    while refused + settled < 17 {
        let line = client
            .lines
            .recv_timeout(Duration::from_secs(90))
            .expect("every wait answers");
        let message: Value = serde_json::from_str(&line).unwrap();
        if message.get("id").is_none() {
            continue;
        }
        let result = &message["result"];
        if result["isError"] == true {
            assert!(
                result["content"][0]["text"]
                    .as_str()
                    .unwrap()
                    .contains("At most 16 waits"),
                "{message}"
            );
            refused += 1;
            drop(gate.take());
        } else {
            assert_eq!(
                result["structuredContent"]["state"], "completed",
                "{message}"
            );
            settled += 1;
        }
    }
    assert_eq!((refused, settled), (1, 16));

    // A megabyte of link is an error, and the server still answers.
    let huge = format!("{base}/{}", "a".repeat(1024 * 1024));
    let refused = client.call("download", json!({"url": huge}));
    assert_eq!(refused["isError"], true);
    let listed = client.call("list_downloads", json!({}));
    assert_eq!(listed["structuredContent"]["total"], 1, "{listed}");
}
