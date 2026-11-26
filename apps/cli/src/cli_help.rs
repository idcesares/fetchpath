//! Organized CLI help, per-command help topics, typo suggestions,
//! direct-link shorthand detection, and shell completion generation.

use crate::download::EXIT_USAGE;
use std::path::Path;

pub const HELP: &str = "Fetchpath — fast, reliable downloads from the command line.

Usage:
  fetchpath [--plain]                       Open the interactive terminal (in a terminal only)
  fetchpath LINK [OPTIONS]                  Quick shorthand for `fetchpath add LINK`
  fetchpath <COMMAND> [ARGS...]             Run a command (use `fetchpath help <COMMAND>`)
  fetchpath --version | --help

Downloading:
  add LINK... [--to PATH] [--sha256 HEX] [--quality Q] [--at TIME] [--upload] [--wait]
  download LINK [DESTINATION] [--sha256 HEX] [--json] [--quiet]
  torrent MAGNET|HTTPS_TORRENT|FILE [--to NEW_FOLDER] [--upload] [--wait]
  batch FILE|- [--to FOLDER] [--at TIME] [--wait]
  inspect LINK                              Inspect media formats before downloading

Queue & Control:
  status [--json]                           One-glance engine and queue summary
  ls [--active|--paused|--failed|--finished|--approvals]
  show JOB...                               Full details for one or more downloads
  watch [JOB]                               Follow live queue events or one download
  pause | resume | retry JOB...|all         Control downloads (use `all` for bulk)
  cancel JOB...                             Cancel active downloads
  rm JOB...|all                             Remove finished jobs from the list (keeps files)
  clean [--failed]                          Remove finished/cancelled jobs from the list
  folder JOB                                Reveal a download in File Explorer
  history [TEXT] [--limit N]                Search finished downloads

Setup, Rules & Engine:
  setup | doctor [--json]                   Check all requirements (media, JS runtime, agents)
  setup --all [--grant FOLDER] [--yes]      Install media tools and connect detected AI agents
  tools [install [--yes] | use FOLDER]      Manage yt-dlp and ffmpeg helpers
  settings [NAME [VALUE]]                   View or change engine settings
  rules [list | add ... | rm ID | test LINK]  Route downloads by site, type, or size
  cache [status | clear]                    Manage checksum-verified cache
  hub [on | off]                            Keep engine running in the background
  engine status | stop                      Inspect or stop the local engine
  completions powershell|bash|zsh|fish      Generate shell tab completions

AI Agents & Approvals:
  mcp [--agent NAME] [--tools SET]          Serve Fetchpath over MCP (stdio)
  agent-setup status | add HOST|--detected | snippet [FORMAT] | remove | check
  agents [grant NAME FOLDER... | revoke ... | limit ... | auto NAME on|off]
  approvals                                 List agent requests waiting for approval
  approve | deny JOB...|all                 Answer pending agent requests
  inbox [accept OFFER [--to FOLDER] | decline OFFER]

Web UI, Sharing & LAN:
  web on | off | open | sign-out            Local loopback web UI (off by default)
  share [setup URL | test | on | off | create FILE | revoke HANDLE|all]
  lan [status | on | off | pair | join ADDRESS CODE [NAME] | unpair KEY]
  fetch-verified --sha256 HEX --size BYTES [--peer ADDRESS=KEY]... LINK DESTINATION

Notes:
  JOB is a download's number from `fetchpath ls`, or the start of its id.
  Most queue commands accept `--json` for machine-readable output.
  Fetchpath never overwrites an existing file.

Exit codes:
  0 saved   1 engine unavailable   2 bad input   3 file already exists
  4 network or server   5 checksum mismatch   6 could not write   130 cancelled

Run `fetchpath help <COMMAND>` or `fetchpath <COMMAND> --help` for details.
The full guide is docs\\CLI.md in the Fetchpath install folder.";

/// Canonical CLI subcommands (for typo suggestions and shell completions).
pub const COMMANDS: &[&str] = &[
    "add",
    "download",
    "torrent",
    "batch",
    "inspect",
    "status",
    "ls",
    "list",
    "show",
    "watch",
    "pause",
    "resume",
    "cancel",
    "retry",
    "rm",
    "remove",
    "clean",
    "folder",
    "history",
    "setup",
    "doctor",
    "tools",
    "settings",
    "rules",
    "cache",
    "hub",
    "engine",
    "completions",
    "mcp",
    "agent-setup",
    "agents",
    "approvals",
    "approve",
    "deny",
    "inbox",
    "web",
    "share",
    "lan",
    "fetch-verified",
    "help",
    "version",
];

/// Returns true when the first positional argument looks like a download link
/// or local `.torrent` file, enabling `fetchpath https://...` shorthand.
pub fn looks_like_direct_link(arg: &str) -> bool {
    let lower = arg.to_ascii_lowercase();
    if lower.starts_with("http://")
        || lower.starts_with("https://")
        || lower.starts_with("magnet:")
        || lower.starts_with("hf://")
    {
        return true;
    }
    lower.ends_with(".torrent") && Path::new(arg).is_file()
}

/// Focused help text for a single CLI subcommand or alias.
pub fn command_help(name: &str) -> Option<&'static str> {
    Some(match name {
        "add" => "Usage: fetchpath add LINK... [OPTIONS]
       fetchpath LINK... [OPTIONS]

Add one or more links (HTTP/HTTPS files, media pages, Hugging Face repos,
or magnet/torrent links) to the Fetchpath download queue.

Options:
  --to FOLDER|FILE  Destination folder or file path (never overwrites)
  --sha256 HEX      Require an exact SHA-256 checksum before saving
  --quality Q       Media quality: best, audio, 1080p, 720p, or format id
  --at TIME         Schedule start (e.g. 22:00, +30m, 2026-12-01T09:00)
  --upload          Allow torrent peer upload (off by default)
  --wait            Stay attached and follow progress until completion
  --json            Emit protocol JSON records
  -q, --quiet       Print only the saved path (with --wait)

Examples:
  fetchpath add https://example.com/archive.tar.gz --wait
  fetchpath https://youtu.be/VIDEO_ID --quality 1080p --to D:\\Videos",

        "download" => "Usage: fetchpath download LINK [DESTINATION] [OPTIONS]

Download a single file directly through the engine and wait until it finishes.

Options:
  --sha256 HEX      Verify downloaded bytes against this SHA-256 hex digest
  --json            Print one JSON result on stdout (no progress output)
  -q, --quiet       Print only the saved path on success",

        "torrent" => "Usage: fetchpath torrent MAGNET|HTTPS_TORRENT|TORRENT_FILE [OPTIONS]

Add a torrent download. Peer discovery is enabled for interactive users;
uploading stays disabled unless `--upload` is passed.

Options:
  --to NEW_FOLDER   Folder to place the torrent contents into
  --upload          Allow uploading to peers during this download
  --wait            Wait until the torrent settles
  --json            Print protocol JSON",

        "batch" => "Usage: fetchpath batch FILE|- [OPTIONS]

Queue multiple links from a text file (or `-` for stdin), one link per line.
Lines starting with `#` and blank lines are ignored. Optional per-line SHA-256:
  https://example.com/file.bin  sha256=<64-hex>

Options:
  --to FOLDER       Destination folder for all queued links
  --at TIME         Schedule start time for the batch
  --wait            Wait for all queued downloads to finish
  --json            Print one JSON record per queued job",

        "inspect" => "Usage: fetchpath inspect LINK [--json]

Inspect a media page (video/audio) and list available formats and qualities
without starting a download.",

        "status" => "Usage: fetchpath status [--json]

Show a concise summary of the Fetchpath engine and queue: active count,
combined download speed, queued/paused/failed/completed counts, and pending
agent approvals.",

        "ls" | "list" => "Usage: fetchpath ls [FILTER] [--json]

List downloads in the Fetchpath queue.

Filters:
  --all             Show all downloads (default)
  --active          Show running, queued, verifying, and paused downloads
  --paused          Show paused downloads only
  --failed          Show failed downloads only
  --finished        Show completed downloads only (alias: --completed)
  --approvals       Show agent requests waiting for your approval
  --json            Print protocol JSON",

        "show" => "Usage: fetchpath show JOB... [--json]

Show full details for one or more downloads (id, state, link, destination,
progress, checksums, timestamps, and error details).

JOB is a download's number from `fetchpath ls` or the start of its id.",

        "watch" => "Usage: fetchpath watch [JOB] [--json]

Without JOB, stream durable queue events until Ctrl+C.
With JOB, follow that download's progress bar until it settles.",

        "pause" => "Usage: fetchpath pause JOB...|all [--json]

Pause one or more active/queued downloads, or use `all` / `--all` to pause
all active downloads.",

        "resume" => "Usage: fetchpath resume JOB...|all [--json]

Resume paused or scheduled downloads, or use `all` / `--all` to resume all
paused and scheduled downloads.",

        "cancel" => "Usage: fetchpath cancel JOB... [--json]

Cancel one or more downloads. Explicit JOB references are required.",

        "retry" => "Usage: fetchpath retry JOB...|all [--json]

Retry failed downloads, or use `all` / `--all` to retry all failed jobs.",

        "rm" | "remove" => "Usage: fetchpath rm JOB...|all [--json]

Remove finished, cancelled, or failed jobs from the queue list.
Saved files on disk are never deleted. Use `all` / `--all` to clear all
terminal jobs.",

        "clean" => "Usage: fetchpath clean [--failed] [--json]

Remove completed and cancelled jobs from the queue list without deleting
any downloaded files. Pass `--failed` (or `--all`) to also clear failed jobs.",

        "folder" => "Usage: fetchpath folder JOB

Reveal a download's saved file in File Explorer.",

        "history" => "Usage: fetchpath history [TEXT] [--limit N] [--json]

Search finished downloads by name or URL.",

        "setup" | "doctor" => "Usage: fetchpath setup [--json]
       fetchpath doctor [--json]
       fetchpath setup [--media] [--agents] [--all] [--grant FOLDER] [--yes]

Check every Fetchpath requirement (engine, CLI PATH, download folder, yt-dlp,
ffmpeg, JS runtime, torrent helper, browser bridge, AI agent hosts & grants).

Opt-in setup actions:
  --media           Install pinned yt-dlp and ffmpeg (asks first unless --yes)
  --agents          Connect all detected AI agent hosts in user scope
  --all             Run both --media and --agents
  --grant FOLDER    Explicitly grant FOLDER to connected AI agent hosts
  --yes, -y         Skip confirmation prompt when installing media tools",

        "tools" => "Usage: fetchpath tools [status | install [--yes] | use FOLDER] [--json]

Check or set up yt-dlp and ffmpeg used for video and audio downloads.",

        "settings" => "Usage: fetchpath settings [NAME [VALUE]] [--json]

List all engine settings, inspect one setting, or update its value.",

        "rules" => "Usage: fetchpath rules [list] [--json]
       fetchpath rules add [--name N] [--host H] [--type EXT] [--min-size S] [--max-size S] [--folder DIR] [--require-checksum]
       fetchpath rules rm ID
       fetchpath rules test LINK

Route downloads automatically by host, extension, or file size, and inspect
which rule matches a link.",

        "cache" => "Usage: fetchpath cache [status | clear] [--json]

Inspect or clear Fetchpath's local checksum-verified file cache.",

        "hub" => "Usage: fetchpath hub [on | off] [--json]

Turn background hub mode on or off so the engine keeps running between CLI calls.",

        "engine" => "Usage: fetchpath engine status [--json]
       fetchpath engine stop

Check the local Fetchpath engine status or shut it down gracefully.",

        "completions" => "Usage: fetchpath completions powershell|bash|zsh|fish

Print shell tab-completion script to stdout.

Examples:
  PowerShell: fetchpath completions powershell | Out-String | Invoke-Expression
  Bash:       eval \"$(fetchpath completions bash)\"
  Zsh:        eval \"$(fetchpath completions zsh)\"
  Fish:       fetchpath completions fish | source",

        "mcp" => "Usage: fetchpath mcp [--agent NAME] [--tools all|core|repository|inbox]

Serve Fetchpath over the Model Context Protocol (MCP) on stdio for AI agents.
When --agent is omitted, the host name is derived from MCP clientInfo.name.",

        "agent-setup" => "Usage: fetchpath agent-setup status [--json]
       fetchpath agent-setup add HOST|--detected [--adopt|--replace]
       fetchpath agent-setup snippet [json|vscode|toml|claude|codex|deeplink-vscode|deeplink-cursor]
       fetchpath agent-setup remove HOST [--keep-config] | cleanup | reconcile
       fetchpath agent-setup check HOST

Register, verify, or remove Fetchpath's MCP server across AI agent hosts.",

        "agents" => "Usage: fetchpath agents [--json]
       fetchpath agents mode NAME|--all ask|scoped|auto|full
       fetchpath agents grant NAME FOLDER...
       fetchpath agents revoke NAME [FOLDER...]
       fetchpath agents limit NAME --size SIZE --per-hour N
       fetchpath agents auto NAME on|off

Control permission modes, granted folders, and rate/size limits for each AI agent.
Modes:
  ask     Wait for your approval before every download
  scoped  Allow inside granted folders up to size/hourly limits (default)
  auto    Allow inside granted folders with no size, hourly, or peer holds
  full    Full access: skip all download approvals anywhere on disk
          (still never overwrites files or exposes settings/credentials)",

        "approvals" => "Usage: fetchpath approvals [--json]

List downloads requested by AI agents that are waiting for your approval.",

        "approve" => "Usage: fetchpath approve JOB...|all [--json]

Approve one or more pending agent download requests.",

        "deny" => "Usage: fetchpath deny JOB...|all [--json]

Deny one or more pending agent download requests (they end cancelled).",

        "inbox" => "Usage: fetchpath inbox [list] [--json]
       fetchpath inbox accept OFFER [--to FOLDER] [--json]
       fetchpath inbox decline OFFER [--json]

List, accept, or decline files offered to you by an AI agent.",

        "web" => "Usage: fetchpath web on | off | open | sign-out

Control Fetchpath's local loopback web UI (disabled until turned on).",

        "share" => "Usage: fetchpath share [setup URL | test | on | off | create FILE | revoke HANDLE|all] [--json]

Manage public download links served through your own configured tunnel.",

        "lan" => "Usage: fetchpath lan [status | on | off | pair | join ADDRESS CODE [NAME] | unpair KEY] [--json]

Pair devices on your local network to share checksum-verified public files.",

        "fetch-verified" => "Usage: fetchpath fetch-verified --sha256 HEX --size BYTES [--peer ADDRESS=KEY]... LINK DESTINATION

Advanced helper: fetch a checksum-verified file from paired LAN peers first,
falling back to LINK, and print JSON.",

        "help" => "Usage: fetchpath help [COMMAND]

Show general usage or focused help for one command.",

        "version" => "Usage: fetchpath --version

Print the installed Fetchpath version.",

        _ => return None,
    })
}

/// Handles `fetchpath help [COMMAND]`.
pub fn run_help(args: &[String]) -> i32 {
    match args {
        [] => {
            println!("{HELP}");
            0
        }
        [topic] => {
            let clean = topic.trim_start_matches('/').to_ascii_lowercase();
            match command_help(&clean) {
                Some(text) => {
                    println!("{text}");
                    0
                }
                None => {
                    if let Some(did_you_mean) = suggest(&clean, COMMANDS.iter().copied()) {
                        eprintln!(
                            "fetchpath: no help topic for {topic}. Did you mean `{did_you_mean}`?\nRun `fetchpath --help` for all commands."
                        );
                    } else {
                        eprintln!(
                            "fetchpath: no help topic for {topic}.\nRun `fetchpath --help` for all commands."
                        );
                    }
                    EXIT_USAGE
                }
            }
        }
        _ => {
            eprintln!("usage: fetchpath help [COMMAND]");
            EXIT_USAGE
        }
    }
}

/// Finds the closest candidate to `input` for typo recovery ("did you mean ...?").
pub fn suggest<'a>(input: &str, candidates: impl IntoIterator<Item = &'a str>) -> Option<&'a str> {
    let needle = input.trim().to_ascii_lowercase();
    if needle.is_empty() {
        return None;
    }
    let max_dist = if needle.len() <= 3 { 1 } else { 2 };
    let mut best: Option<(&'a str, usize)> = None;
    for candidate in candidates {
        let lower = candidate.to_ascii_lowercase();
        let dist = levenshtein(&needle, &lower);
        let prefix_bonus = lower.starts_with(&needle) && needle.len() >= 2;
        if dist <= max_dist || prefix_bonus {
            let score = if prefix_bonus { dist.min(1) } else { dist };
            match best {
                None => best = Some((candidate, score)),
                Some((_, best_score)) if score < best_score => best = Some((candidate, score)),
                _ => {}
            }
        }
    }
    best.map(|(candidate, _)| candidate)
}

fn levenshtein(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    if a.is_empty() {
        return b.len();
    }
    if b.is_empty() {
        return a.len();
    }
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut curr = vec![0; b.len() + 1];
    for (i, &ca) in a.iter().enumerate() {
        curr[0] = i + 1;
        for (j, &cb) in b.iter().enumerate() {
            let cost = usize::from(ca != cb);
            curr[j + 1] = (prev[j + 1] + 1).min(curr[j] + 1).min(prev[j] + cost);
        }
        std::mem::swap(&mut prev, &mut curr);
    }
    prev[b.len()]
}

/// Prints a shell completion script for PowerShell, Bash, Zsh, or Fish.
pub fn completions(args: &[String]) -> i32 {
    let shell = match args {
        [shell] => shell.to_ascii_lowercase(),
        _ => {
            eprintln!("usage: fetchpath completions powershell|bash|zsh|fish");
            return EXIT_USAGE;
        }
    };
    let Some(script) = completion_script(&shell) else {
        eprintln!(
            "fetchpath: unsupported shell {shell:?}. Choose one of: powershell, bash, zsh, fish."
        );
        return EXIT_USAGE;
    };
    print!("{script}");
    0
}

pub fn completion_script(shell: &str) -> Option<String> {
    let cmds = COMMANDS.join(" ");
    match shell {
        "powershell" | "pwsh" => {
            let quoted: Vec<String> = COMMANDS.iter().map(|c| format!("'{c}'")).collect();
            Some(format!(
                r#"Register-ArgumentCompleter -Native -CommandName fetchpath -ScriptBlock {{
    param($wordToComplete, $commandAst, $cursorPosition)
    $tokens = $commandAst.CommandElements | ForEach-Object {{ $_.ToString() }}
    $commands = @({cmds})
    $sub = if ($tokens.Count -ge 2) {{ $tokens[1] }} else {{ '' }}
    $choices = switch ($sub) {{
        'ls'          {{ @('--all','--active','--paused','--failed','--finished','--approvals','--json') }}
        'add'         {{ @('--to','--sha256','--quality','--at','--upload','--wait','--json','--quiet') }}
        'setup'       {{ @('--json','--media','--agents','--all','--grant','--yes') }}
        'doctor'      {{ @('--json') }}
        'agent-setup' {{ @('status','add','snippet','remove','cleanup','reconcile','check') }}
        'agents'      {{ @('grant','revoke','limit','auto','--json') }}
        'rules'       {{ @('list','add','rm','test','--json') }}
        'tools'       {{ @('status','install','use','--json') }}
        'web'         {{ @('on','off','open','sign-out') }}
        'hub'         {{ @('on','off','--json') }}
        'engine'      {{ @('status','stop','--json') }}
        'clean'       {{ @('--failed','--all','--json') }}
        'completions' {{ @('powershell','bash','zsh','fish') }}
        'help'        {{ $commands }}
        default       {{ if ($tokens.Count -le 2) {{ $commands }} else {{ @('--help','--json') }} }}
    }}
    $choices | Where-Object {{ $_ -like "$wordToComplete*" }} | ForEach-Object {{
        [System.Management.Automation.CompletionResult]::new($_, $_, 'ParameterValue', $_)
    }}
}}
"#,
                cmds = quoted.join(", ")
            ))
        }
        "bash" => Some(format!(
            r#"_fetchpath_completions() {{
    local cur prev
    cur="${{COMP_WORDS[COMP_CWORD]}}"
    prev="${{COMP_WORDS[COMP_CWORD-1]}}"
    local commands="{cmds}"
    if [[ $COMP_CWORD -eq 1 ]]; then
        COMPREPLY=( $(compgen -W "$commands --help --version --plain" -- "$cur") )
        return 0
    fi
    case "${{COMP_WORDS[1]}}" in
        ls|list) COMPREPLY=( $(compgen -W "--all --active --paused --failed --finished --approvals --json" -- "$cur") ) ;;
        add) COMPREPLY=( $(compgen -W "--to --sha256 --quality --at --upload --wait --json --quiet" -- "$cur") ) ;;
        setup|doctor) COMPREPLY=( $(compgen -W "--json --media --agents --all --grant --yes" -- "$cur") ) ;;
        agent-setup) COMPREPLY=( $(compgen -W "status add snippet remove cleanup reconcile check" -- "$cur") ) ;;
        agents) COMPREPLY=( $(compgen -W "grant revoke limit auto --json" -- "$cur") ) ;;
        rules) COMPREPLY=( $(compgen -W "list add rm test --json" -- "$cur") ) ;;
        tools) COMPREPLY=( $(compgen -W "status install use --json" -- "$cur") ) ;;
        web) COMPREPLY=( $(compgen -W "on off open sign-out" -- "$cur") ) ;;
        hub) COMPREPLY=( $(compgen -W "on off --json" -- "$cur") ) ;;
        engine) COMPREPLY=( $(compgen -W "status stop --json" -- "$cur") ) ;;
        clean) COMPREPLY=( $(compgen -W "--failed --all --json" -- "$cur") ) ;;
        completions) COMPREPLY=( $(compgen -W "powershell bash zsh fish" -- "$cur") ) ;;
        help) COMPREPLY=( $(compgen -W "$commands" -- "$cur") ) ;;
        *) COMPREPLY=( $(compgen -W "--help --json" -- "$cur") ) ;;
    esac
}}
complete -F _fetchpath_completions fetchpath
"#
        )),
        "zsh" => Some(format!(
            r#"#compdef fetchpath
_fetchpath() {{
    local -a commands
    commands=({cmds})
    if (( CURRENT == 2 )); then
        compadd -a commands
        return
    fi
    case "${{words[2]}}" in
        ls|list) compadd -- --all --active --paused --failed --finished --approvals --json ;;
        add) compadd -- --to --sha256 --quality --at --upload --wait --json --quiet ;;
        setup|doctor) compadd -- --json --media --agents --all --grant --yes ;;
        agent-setup) compadd -- status add snippet remove cleanup reconcile check ;;
        agents) compadd -- grant revoke limit auto --json ;;
        rules) compadd -- list add rm test --json ;;
        tools) compadd -- status install use --json ;;
        web) compadd -- on off open sign-out ;;
        hub) compadd -- on off --json ;;
        engine) compadd -- status stop --json ;;
        clean) compadd -- --failed --all --json ;;
        completions) compadd -- powershell bash zsh fish ;;
        help) compadd -a commands ;;
        *) compadd -- --help --json ;;
    esac
}}
compdef _fetchpath fetchpath
"#
        )),
        "fish" => {
            let mut out = String::from("# Fish completions for fetchpath\n");
            for cmd in COMMANDS {
                out.push_str(&format!(
                    "complete -c fetchpath -n '__fish_use_subcommand' -a '{cmd}'\n"
                ));
            }
            out.push_str(
                "complete -c fetchpath -n '__fish_seen_subcommand_from ls list' -l all -l active -l paused -l failed -l finished -l approvals -l json\n",
            );
            out.push_str(
                "complete -c fetchpath -n '__fish_seen_subcommand_from setup doctor' -l json -l media -l agents -l all -l grant -l yes\n",
            );
            out.push_str(
                "complete -c fetchpath -n '__fish_seen_subcommand_from completions' -a 'powershell bash zsh fish'\n",
            );
            Some(out)
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn direct_link_detection_accepts_urls_magnets_hf_and_rejects_words_and_flags() {
        assert!(looks_like_direct_link("https://example.com/file.zip"));
        assert!(looks_like_direct_link("http://127.0.0.1:8080/a.bin"));
        assert!(looks_like_direct_link("magnet:?xt=urn:btih:abc"));
        assert!(looks_like_direct_link("hf://org/model"));
        assert!(!looks_like_direct_link("--help"));
        assert!(!looks_like_direct_link("status"));
        assert!(!looks_like_direct_link("stauts"));
    }

    #[test]
    fn typo_suggestions_find_close_commands() {
        assert_eq!(suggest("stauts", COMMANDS.iter().copied()), Some("status"));
        assert_eq!(suggest("doctro", COMMANDS.iter().copied()), Some("doctor"));
        assert_eq!(suggest("paus", COMMANDS.iter().copied()), Some("pause"));
        assert_eq!(suggest("zzzzzz", COMMANDS.iter().copied()), None);
    }

    #[test]
    fn every_known_command_has_help_and_all_shells_generate_completions() {
        for cmd in COMMANDS {
            assert!(
                command_help(cmd).is_some(),
                "missing per-command help for {cmd}"
            );
        }
        for shell in ["powershell", "bash", "zsh", "fish"] {
            let script = completion_script(shell).expect("shell supported");
            assert!(script.contains("fetchpath"));
            assert!(script.contains("setup"));
        }
    }
}
