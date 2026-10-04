//! FP-102: isolated user-scope registration, ownership and recovery contracts.
use serde_json::{Value, json};
use std::{
    fs,
    path::PathBuf,
    process::{Command, Output},
};

const EXE: &str = env!("CARGO_BIN_EXE_fetchpath");
struct Home {
    dir: tempfile::TempDir,
}
impl Home {
    fn new() -> Self {
        let home = Self {
            dir: tempfile::tempdir().unwrap(),
        };
        for name in ["codex", "claude", "bin"] {
            fs::create_dir_all(home.path(name)).unwrap();
        }
        for name in ["codex", "claude"] {
            fs::write(home.path(&format!("bin/{name}.cmd")), "@exit /b 0\n").unwrap();
            fs::write(home.path(&format!("bin/{name}")), "").unwrap();
        }
        home
    }
    fn path(&self, path: &str) -> PathBuf {
        self.dir.path().join(path)
    }
    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(EXE);
        command
            .args(["agent-setup"])
            .args(args)
            .env("CODEX_HOME", self.path("codex"))
            .env("CLAUDE_CONFIG_DIR", self.path("claude"))
            .env("USERPROFILE", self.path("profile"))
            .env("HOME", self.path("profile"))
            .env("LOCALAPPDATA", self.path("local"))
            .env("FETCHPATH_AGENT_SETUP_DIR", self.path("inventory"))
            .env("FETCHPATH_APP_DATA_DIR", self.path("data"))
            .env("PATH", self.path("bin"));
        command
    }
    fn run(&self, args: &[&str]) -> Output {
        self.command(args).output().unwrap()
    }
    fn ok(&self, args: &[&str]) -> Output {
        let output = self.run(args);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        output
    }
    fn inventory(&self) -> Value {
        serde_json::from_slice(&fs::read(self.path("inventory/inventory.json")).unwrap()).unwrap()
    }
    fn entry(&self, host: &str) -> Value {
        if host == "codex" {
            toml_edit::de::from_str::<Value>(
                &fs::read_to_string(self.path("codex/config.toml")).unwrap(),
            )
            .unwrap()["mcp_servers"]["fetchpath"]
                .clone()
        } else {
            serde_json::from_slice::<Value>(&fs::read(self.path("claude/.claude.json")).unwrap())
                .unwrap()["mcpServers"]["fetchpath"]
                .clone()
        }
    }
}

#[test]
fn explicit_host_setup_preserves_settings_and_comments_is_repeatable_and_grants_nothing() {
    let home = Home::new();
    fs::write(home.path("codex/config.toml"), "# keep this comment\nmodel = 'mine' # and this one\n[mcp_servers.other]\ncommand = 'other'\n").unwrap();
    fs::write(home.path("claude/.claude.json"), r#"{"theme":"dark","projects":{"private":{"allowedTools":[]}},"mcpServers":{"other":{"command":"other"}}}"#).unwrap();
    home.ok(&["add", "codex"]);
    assert!(home.entry("claude-code").is_null());
    let once = fs::read(home.path("codex/config.toml")).unwrap();
    home.ok(&["add", "codex"]);
    assert_eq!(once, fs::read(home.path("codex/config.toml")).unwrap());
    let toml = String::from_utf8(once).unwrap();
    assert!(
        toml.contains("# keep this comment")
            && toml.contains("# and this one")
            && toml.contains("command = 'other'")
    );
    home.ok(&["add", "claude-code"]);
    let json: Value =
        serde_json::from_slice(&fs::read(home.path("claude/.claude.json")).unwrap()).unwrap();
    assert_eq!(json["projects"]["private"]["allowedTools"], json!([]));
    assert_eq!(json["theme"], "dark");
    assert_eq!(json["mcpServers"]["other"]["command"], "other");
    assert_eq!(
        home.entry("codex")["args"],
        json!(["mcp", "--agent", "codex"])
    );
    assert_eq!(
        home.entry("claude-code")["args"],
        json!(["mcp", "--agent", "claude-code"])
    );
    assert!(PathBuf::from(home.entry("codex")["command"].as_str().unwrap()).is_absolute());
    assert!(
        !home.path("data").exists(),
        "setup must not initialize policy/engine data"
    );
    let status: Value = serde_json::from_slice(&home.ok(&["status", "--json"]).stdout).unwrap();
    assert_eq!(status["codex"]["available"], true);
    assert_eq!(status["claude_code"]["owned"], true);
    assert_eq!(status["access_granted_by_setup"], false);
    assert_eq!(home.inventory()["records"].as_array().unwrap().len(), 2);
}

#[test]
fn cleanup_uses_inventory_after_home_overrides_change_and_hosts_disappear() {
    let home = Home::new();
    home.ok(&["add", "codex"]);
    home.ok(&["add", "claude-code"]);
    let output = home
        .command(&["cleanup"])
        .env("PATH", home.path("missing"))
        .env("CODEX_HOME", home.path("different"))
        .env("CLAUDE_CONFIG_DIR", home.path("different-claude"))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(home.entry("codex").is_null() && home.entry("claude-code").is_null());
    assert_eq!(home.inventory()["records"], json!([]));
    home.ok(&["cleanup"]);
}

#[test]
fn unowned_conflicts_require_explicit_replace_and_exact_adoption() {
    let home = Home::new();
    fs::write(
        home.path("codex/config.toml"),
        "[mcp_servers.fetchpath]\ncommand = 'private-secret-command'\n",
    )
    .unwrap();
    let before = fs::read(home.path("codex/config.toml")).unwrap();
    let failed = home.run(&["add", "codex"]);
    assert!(!failed.status.success());
    assert!(!String::from_utf8_lossy(&failed.stderr).contains("private-secret-command"));
    assert!(!home.run(&["add", "codex", "--adopt"]).status.success());
    home.ok(&["remove", "codex"]);
    assert_eq!(before, fs::read(home.path("codex/config.toml")).unwrap());
    home.ok(&["add", "codex", "--replace"]);
    let entry = home.entry("codex");
    let mut inventory = home.inventory();
    inventory["records"] = json!([]);
    fs::write(home.path("inventory/inventory.json"), inventory.to_string()).unwrap();
    assert!(!home.run(&["add", "codex"]).status.success());
    home.ok(&["add", "codex", "--adopt"]);
    assert_eq!(entry, home.entry("codex"));
    home.ok(&["remove", "codex"]);
    assert!(home.entry("codex").is_null());
}

#[test]
fn edited_owned_entries_are_preserved_and_recoverable() {
    let home = Home::new();
    home.ok(&["add", "claude-code"]);
    let mut config: Value =
        serde_json::from_slice(&fs::read(home.path("claude/.claude.json")).unwrap()).unwrap();
    config["mcpServers"]["fetchpath"]["env"] = json!({"TOKEN":"private-secret"});
    fs::write(home.path("claude/.claude.json"), config.to_string()).unwrap();
    let before = fs::read(home.path("claude/.claude.json")).unwrap();
    for args in [
        &["cleanup"][..],
        &["reconcile"][..],
        &["check", "claude-code"][..],
    ] {
        let output = home.run(args);
        assert!(!output.status.success());
        assert!(!String::from_utf8_lossy(&output.stderr).contains("private-secret"));
        assert_eq!(before, fs::read(home.path("claude/.claude.json")).unwrap());
    }
    assert_eq!(home.inventory()["records"].as_array().unwrap().len(), 1);
    home.ok(&["add", "claude-code", "--replace"]);
    home.ok(&["cleanup"]);
}

#[test]
fn malformed_duplicate_key_and_missing_host_cases_never_mutate() {
    let home = Home::new();
    for text in [
        r#"{"mcpServers":{},"mcpServers":{"fetchpath":{"command":"secret"}}}"#,
        "secret is not JSON",
        "[]",
    ] {
        fs::write(home.path("claude/.claude.json"), text).unwrap();
        let output = home.run(&["add", "claude-code"]);
        assert!(!output.status.success());
        assert!(!String::from_utf8_lossy(&output.stderr).contains("secret"));
        assert_eq!(
            text,
            fs::read_to_string(home.path("claude/.claude.json")).unwrap()
        );
        let status: Value = serde_json::from_slice(&home.ok(&["status", "--json"]).stdout).unwrap();
        assert_eq!(status["claude_code"]["available"], true);
        assert_eq!(status["claude_code"]["connection"], "configuration-error");
    }
    let output = home
        .command(&["add", "codex"])
        .env("PATH", home.path("missing"))
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(!home.path("codex/config.toml").exists());
    assert!(
        !home
            .run(&["add", "codex", "--adopt", "--replace"])
            .status
            .success()
    );
}

#[test]
fn reconcile_migrates_owned_command_paths_without_duplicates() {
    let home = Home::new();
    home.ok(&["add", "claude-code"]);
    let mut config: Value =
        serde_json::from_slice(&fs::read(home.path("claude/.claude.json")).unwrap()).unwrap();
    let mut inventory = home.inventory();
    let old = home.path("old install with spaces/fetchpath.exe");
    config["mcpServers"]["fetchpath"]["command"] = json!(old);
    inventory["records"][0]["expected"] = config["mcpServers"]["fetchpath"].clone();
    fs::write(home.path("claude/.claude.json"), config.to_string()).unwrap();
    fs::write(home.path("inventory/inventory.json"), inventory.to_string()).unwrap();
    home.ok(&["reconcile"]);
    assert_eq!(
        fs::canonicalize(PathBuf::from(
            home.entry("claude-code")["command"].as_str().unwrap()
        ))
        .unwrap(),
        fs::canonicalize(EXE).unwrap()
    );
    assert_eq!(home.inventory()["records"].as_array().unwrap().len(), 1);
}

#[test]
fn interrupted_publication_finalizes_ownership_and_interrupted_cleanup_recovers() {
    let home = Home::new();
    home.ok(&["add", "claude-code"]);
    let inventory = home.inventory();
    let record = inventory["records"][0].clone();
    let pending_add = json!({"version":1,"records":[],"pending":{"previous":null,"next":record,"before_digest":null}});
    fs::write(
        home.path("inventory/inventory.json"),
        pending_add.to_string(),
    )
    .unwrap();
    home.ok(&["status", "--json"]);
    assert_eq!(home.inventory()["records"].as_array().unwrap().len(), 1);
    let pending_remove = json!({"version":1,"records":[record],"pending":{"previous":record,"next":null,"before_digest":null}});
    fs::write(
        home.path("inventory/inventory.json"),
        pending_remove.to_string(),
    )
    .unwrap();
    fs::write(home.path("claude/.claude.json"), "{}").unwrap();
    home.ok(&["cleanup"]);
    assert_eq!(home.inventory()["records"], json!([]));
    assert!(home.inventory()["pending"].is_null());
}

#[test]
fn interrupted_before_write_cancels_and_conflicting_recovery_preserves_inventory() {
    let home = Home::new();
    home.ok(&["add", "claude-code"]);
    let inventory = home.inventory();
    let record = inventory["records"][0].clone();
    let pending = json!({"version":1,"records":[],"pending":{"previous":null,"next":record,"before_digest":null}});
    fs::write(home.path("inventory/inventory.json"), pending.to_string()).unwrap();
    fs::remove_file(home.path("claude/.claude.json")).unwrap();
    home.ok(&["status", "--json"]);
    assert_eq!(home.inventory()["records"], json!([]));
    fs::write(home.path("inventory/inventory.json"), pending.to_string()).unwrap();
    fs::write(
        home.path("claude/.claude.json"),
        r#"{"mcpServers":{"fetchpath":{"command":"edited"}}}"#,
    )
    .unwrap();
    let before = fs::read(home.path("inventory/inventory.json")).unwrap();
    assert!(!home.run(&["cleanup"]).status.success());
    assert_eq!(
        before,
        fs::read(home.path("inventory/inventory.json")).unwrap()
    );
}

#[test]
fn cleanup_failure_keeps_ownership_and_file_lock_is_released_by_process_exit() {
    let home = Home::new();
    home.ok(&["add", "claude-code"]);
    fs::remove_file(home.path("claude/.claude.json")).unwrap();
    fs::create_dir(home.path("claude/.claude.json")).unwrap();
    assert!(!home.run(&["cleanup"]).status.success());
    assert_eq!(home.inventory()["records"].as_array().unwrap().len(), 1);
    fs::remove_dir(home.path("claude/.claude.json")).unwrap();
    home.ok(&["cleanup"]);
    assert_eq!(home.inventory()["records"], json!([]));
}

#[test]
fn keep_config_explicitly_relinquishes_edited_registration_for_uninstall() {
    let home = Home::new();
    home.ok(&["add", "claude-code"]);
    fs::write(
        home.path("claude/.claude.json"),
        r#"{"mcpServers":{"fetchpath":{"command":"my-custom-server"}}}"#,
    )
    .unwrap();
    let before = fs::read(home.path("claude/.claude.json")).unwrap();
    assert!(!home.run(&["cleanup"]).status.success());
    home.ok(&["remove", "claude-code", "--keep-config"]);
    home.ok(&["cleanup"]);
    assert_eq!(before, fs::read(home.path("claude/.claude.json")).unwrap());
    assert_eq!(home.inventory()["records"], json!([]));
}

#[test]
fn keep_config_can_relinquish_an_interrupted_operation_with_edited_entry() {
    let home = Home::new();
    home.ok(&["add", "claude-code"]);
    let mut inventory = home.inventory();
    let record = inventory["records"][0].clone();
    inventory["pending"] = json!({"previous":record,"next":null,"before_digest":null});
    fs::write(home.path("inventory/inventory.json"), inventory.to_string()).unwrap();
    fs::write(
        home.path("claude/.claude.json"),
        r#"{"mcpServers":{"fetchpath":{"command":"my-custom-server"}}}"#,
    )
    .unwrap();
    let before = fs::read(home.path("claude/.claude.json")).unwrap();
    assert!(!home.run(&["cleanup"]).status.success());
    home.ok(&["remove", "claude-code", "--keep-config"]);
    home.ok(&["cleanup"]);
    assert_eq!(before, fs::read(home.path("claude/.claude.json")).unwrap());
    assert_eq!(home.inventory()["records"], json!([]));
    assert!(home.inventory()["pending"].is_null());
}

#[cfg(windows)]
#[test]
#[allow(
    clippy::permissions_set_readonly_false,
    reason = "Windows-only test restores the Windows read-only file attribute"
)]
fn failed_atomic_replacement_keeps_original_and_journal_recovers_for_retry() {
    let home = Home::new();
    let path = home.path("codex/config.toml");
    fs::write(&path, "# preserved\nmodel = 'mine'\n").unwrap();
    let before = fs::read(&path).unwrap();
    let mut permissions = fs::metadata(&path).unwrap().permissions();
    permissions.set_readonly(true);
    fs::set_permissions(&path, permissions).unwrap();
    assert!(!home.run(&["add", "codex"]).status.success());
    assert_eq!(before, fs::read(&path).unwrap());
    assert!(!home.inventory()["pending"].is_null());
    let mut permissions = fs::metadata(&path).unwrap().permissions();
    permissions.set_readonly(false);
    fs::set_permissions(&path, permissions).unwrap();
    home.ok(&["add", "codex"]);
    assert!(home.inventory()["pending"].is_null());
    home.ok(&["cleanup"]);
}

#[test]
fn inventory_lock_blocks_concurrent_commands_without_stale_lock_after_release() {
    let home = Home::new();
    home.ok(&["add", "codex"]);
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(home.path("inventory/lock"))
        .unwrap();
    lock.try_lock().unwrap();
    assert!(!home.run(&["cleanup"]).status.success());
    assert!(!home.entry("codex").is_null());
    drop(lock);
    home.ok(&["cleanup"]);
}

#[cfg(windows)]
#[test]
fn owned_stdio_command_passes_mcp_handshake_and_tool_discovery_without_grants() {
    let home = Home::new();
    home.ok(&["add", "codex"]);
    let output = home.ok(&["check", "codex"]);
    assert!(String::from_utf8_lossy(&output.stdout).contains("tool discovery passed"));
}
