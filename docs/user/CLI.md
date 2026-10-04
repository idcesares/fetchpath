# The `fetchpath` command

The installer adds `fetchpath` to your PATH. Open a **new** terminal after
installing so it's found.

```powershell
fetchpath [--plain]                      # the interactive terminal
fetchpath download LINK [DESTINATION] [--sha256 HEX] [--json] [--quiet]
fetchpath add LINK... [--to FOLDER|FILE] [--sha256 HEX] [--quality Q] [--at TIME] [--upload] [--wait]
fetchpath torrent MAGNET|HTTPS_LINK|TORRENT_FILE [--to NEW_FOLDER] [--upload] [--wait] [--json]
fetchpath batch FILE|- [--to FOLDER] [--at TIME] [--wait]
fetchpath ls | show | pause | resume | cancel | retry | rm | watch | history | folder
fetchpath inspect LINK | settings [NAME [VALUE]] | rules ... | engine status | engine stop [--for-update]
fetchpath approvals | approve JOB... | deny JOB... | agents ...
fetchpath mcp [--agent NAME]
fetchpath tools [install [--yes] | use FOLDER]
fetchpath cache [status | clear]
fetchpath lan [status | on | off | pair | join ... | unpair KEY] | fetch-verified ...
fetchpath --version
fetchpath --help
```

Every command works on one shared queue, kept by the Fetchpath engine, a
background process that starts by itself when a command needs it and stops
about a minute after the last download finishes and the last command ends.
Closing a terminal does not stop downloads.

## Downloading

```powershell
# Where a rule says, else into your Downloads folder, named after the link
fetchpath download https://example.com/tools/archive.zip

# Into a folder you choose
fetchpath download https://example.com/tools/archive.zip D:\Installers\

# As a specific file
fetchpath download https://example.com/tools/archive.zip D:\Installers\tools.zip
```

- **Destination:** a folder (it exists, or ends in `\`) gets the file name
  from the link. If the link doesn't name a file, for example
  `https://example.com/`, the file is called `download`. Anything else is
  treated as a file path.
- **No overwriting:** an existing file is never replaced. Fetchpath stops with
  exit code 3.
- **Progress:** while it runs in a terminal, Fetchpath shows percent, size,
  speed and time left. Percent and time left appear only when the server
  states the file's size.
- **Result:** the saved path goes to standard output, so it can be used in
  scripts. The summary and SHA-256 go to standard error.
- **Ctrl+C** cancels. Nothing is saved.
- **Shared queue:** the download appears in `fetchpath ls` and the history
  like any other, and can be paused or cancelled from another terminal.
- **Automatic retries:** if the queue retries a failure by itself (the
  `auto-retry` setting, on by default), `download` waits for those tries and
  says so on standard error. `fetchpath settings auto-retry off` makes the
  first failure the last.

Links must start with `http://` or `https://`.

### Torrents and magnets

Install the torrent component (included in Full setup), then add one magnet, HTTPS
`.torrent` link, or local `.torrent` file with `fetchpath torrent SOURCE`.
`--to` is optional: without it the torrent goes into a
folder named after it inside your default download folder (or the matching
rule's folder), and a number is added if that name is taken. With `--to
D:\Downloads\NewFolder` the folder is exactly that. `fetchpath add SOURCE --to
D:\Downloads\NewFolder` and the terminal's `/add` detect
these torrent sources too. Fetchpath never replaces or merges into an existing folder. Peer discovery is on, so peers can see your IP address (`--discover-peers` is accepted and changes nothing). Uploading is off unless you pass `--upload`. Cancelling stops all peer activity; torrents cannot be paused. The job appears in the shared
queue and can be cancelled or retried there. An agent must request peer discovery and obtain your approval, unless you
explicitly enabled automatic mode inside its granted folders. The transfer has a 32-peer limit and bounded
download and upload rates. For a local `.torrent`, Fetchpath saves a private
copy of its metadata so the job can survive moving or deleting the original.

## Checking a checksum

```powershell
fetchpath download https://example.com/tool.zip --sha256 3f1c…e9a2
```

The file is saved only if its SHA-256 matches. If it doesn't, nothing is saved
and Fetchpath exits with code 5, printing both values.

Without `--sha256`, Fetchpath prints the SHA-256 of what it received. That
fingerprint was computed on your computer. It says which bytes you have, not
who published them.

### The cache

A download with `--sha256` (`fetchpath download`, `add`, the terminal or the
desktop) leaves a verified copy in Fetchpath's cache. The same file
queued again, even from another link, is copied from the cache and checked
against its checksum again instead of being downloaded; `ls` shows it as
`from cache`. `fetchpath cache` shows how much the cache holds,
`fetchpath cache clear` empties it without touching saved files, and
`fetchpath settings cache-quota-bytes BYTES` sets its size (256 MiB to
256 GiB, 2 GiB to start). The oldest copies go first when it is full.
Agents' downloads never use the cache.

## Models and datasets from Hugging Face

```powershell
fetchpath add hf://openai-community/gpt2 --to D:\Models
fetchpath add hf://datasets/owner/name@v1.0/train --to D:\Data
fetchpath add https://huggingface.co/owner/name/tree/main/onnx --to D:\Models
```

A Hugging Face link, as `hf://OWNER/NAME[@REVISION][/PATH]` or a
`huggingface.co` address, is looked up first: Fetchpath asks Hugging Face
which commit the branch or tag points at now and pins every file to that
commit, so the download cannot mix two versions if the repository changes
meanwhile. The files keep the repository's layout, in a folder named after
it inside `--to`. Every large file is saved only if it matches the SHA-256
Hugging Face states for it, and the same file downloaded again comes from
[the cache](#the-cache). A path after the name takes one file or folder.
Public repositories only: a gated or private one is refused, because
Fetchpath has no Hugging Face sign-in yet. The same links work in the
terminal's `/add` and in the desktop's Add download.

## For scripts

`--json` prints one JSON object on standard output and no progress:

```json
{"result":"downloaded_observed","destination":"D:\\Installers\\tools.zip","bytes":1048576,"observed_sha256":"…","checksum_matched":false,"job_id":"…","staging_cleanup_pending":null}
```

A failure prints `{"result":"failed","error_code":"…","detail":"…"}`, and a
cancellation prints `{"result":"cancelled",…}`.

`--quiet` prints only the saved path.

### Exit codes

| Code | Meaning |
| --- | --- |
| 0 | Saved |
| 1 | The Fetchpath engine could not be started or reached |
| 2 | Bad input: a missing or invalid link, destination, option or download reference |
| 3 | A file already exists at the destination |
| 4 | Network or server problem |
| 5 | The file didn't match `--sha256`; nothing was saved |
| 6 | Couldn't write to the destination |
| 130 | Cancelled, or Ctrl+C while waiting |

The queue commands below use the same codes.

## The queue

```powershell
# Add without waiting; prints the new download's short id
fetchpath add https://example.com/a.zip https://example.com/b.zip --to D:\Installers\

# Start at a time: 18:30 (the next time the clock shows it), 2026-10-01 08:00,
# +30m, +2h or +1d
fetchpath add https://example.com/big.iso --at 23:00

# Video or audio: see the formats, then pick one (or best, or audio; a height
# such as 720p takes the tallest video up to it). Without --quality, a video
# page gets a rule's quality, else the best up to 1080p; it is never saved as
# the page itself.
fetchpath inspect https://video.example/watch/123
fetchpath add https://video.example/watch/123 --quality 720p

# One link per line, optionally followed by a destination; # starts a comment
fetchpath batch links.txt --to D:\Downloads\
Get-Content links.txt | fetchpath batch -
```

`--to` works like `download`'s destination; without it, a download goes
where a matching rule says, else to the default folder from `fetchpath
settings default-destination-dir`, or to Downloads. `add` looks at each link
first (its headers only), so a rule by size can decide; `batch` does not, so
only rules by site and type apply to it. `--wait` stays until the downloads
end and exits with their code.

```powershell
fetchpath ls                 # every download, newest first; --active, --failed
fetchpath show 2             # one download in full
fetchpath pause 1 3          # several at once
fetchpath resume 3f1c        # the start of its id works too
fetchpath cancel 1
fetchpath retry 2
fetchpath rm 4               # remove a finished or failed download from the list
fetchpath watch              # follow the queue until Ctrl+C
fetchpath watch 1            # follow one download to its end
fetchpath history invoice    # finished and failed downloads matching a word
fetchpath approve 5          # let an agent's request that waits for you run
fetchpath deny 5             # refuse it; it ends cancelled
```

A download is named by its number in `fetchpath ls` (up to four digits) or by
the start of its id, which must match only one.

### Rules

Rules choose where a new download goes and how, by the site it comes from,
its file type or its size. They are tried in order and the first that matches
decides. They apply to every download, whichever program adds it; an agent's
download that a rule sends outside the agent's folders waits for your
approval.

```powershell
# Disc images of 1 GB or more into D:\ISOs, with at most 2 connections
fetchpath rules add --name "Disc images" --type iso,img --min-size 1GB --folder D:\ISOs --connections 2

# Anything from example.com must come with a checksum
fetchpath rules add --domain example.com --require-checksum

# Videos from a site at 720p at most
fetchpath rules add --domain video.example --quality 720p

fetchpath rules                  # list them, in order
fetchpath rules test LINK        # which rule decides for a link, and why
fetchpath rules rm 2             # remove rule 2
```

Conditions: `--domain` (a site also covers its subdomains), `--type`
(extensions, comma separated), `--min-size` and `--max-size` (`500MB`, `2GB`;
units count in 1024s, as File Explorer shows sizes). A size rule never matches
a link whose size is not known. Actions: `--folder`, `--quality` (`best`,
`audio` or a height), `--require-checksum` (a file without `--sha256` is
refused), `--connections` (1 to 8). `--position N` puts the rule at place N
in the order. A rule's folder applies only when you give no `--to`.

### Settings

`fetchpath settings` lists every setting, `fetchpath settings NAME` shows one
and `fetchpath settings NAME VALUE` changes it. Switches take `on` or `off`,
folders take a path or `none`. `disk-reserve-bytes` is the space kept free on
every drive: `auto` (the larger of 5 GB and 5 % of the drive) or a size such
as `10GB`; a download that would cross it waits as `needs disk space` and
starts by itself when there is room. The engine keeps values in range and prints the
value it applied. The desktop app shows the same settings.

### The engine

`fetchpath engine status` says whether it is running and what this Fetchpath
is named; `fetchpath engine stop` stops it after saving every download's
progress, and the next command starts it again and carries on.
Use `fetchpath --version` for the installed application version; engine status
reports the internal engine component version.

The name is the computer's unless you choose one with
`fetchpath settings instance-name NAME` (`none` returns to the computer's
name). Each Fetchpath also keeps an id of its own, and every change a client
sends names it, so a change meant for one Fetchpath is never carried out by
another.

Normally the engine stops about a minute after it has nothing left to do.
`fetchpath hub on` keeps it running in the background instead, with no app
open: it starts when you sign in to Windows and keeps the computer awake
while downloads run, but not while idle. It is not a Windows service, so after
a restart it runs again only once you sign in. `fetchpath hub off` returns to
the normal stop; it leaves the sign-in start on, which you can turn off with
`fetchpath settings start-engine-at-sign-in off`. `fetchpath hub` says which
is in effect.

`fetchpath engine stop --for-update` is what the installer and uninstaller
run: it also keeps a new engine from starting until setup finishes (for at most
ten minutes) and returns only once the engine has exited. While it holds,
commands answer that Fetchpath is being updated.

The desktop app is a client of the same engine, so a download added here
shows in the app, and the other way round.

### For scripts

Every queue command takes `--json` and prints the engine's own records, one
JSON object per line: `ls` and `history` print `{"type":"Jobs","jobs":[…]}`,
`show` and `add` print `{"type":"Job","job":{…}}` per download, `pause` and
`cancel` print `{"type":"Control","outcome":"accepted",…}`, and `watch` prints
each event and progress sample as the engine sends it. A failure prints
`{"error":{"code":"…","message":"…",…}}`. Decide from `code`, never from
`message`.

## The interactive terminal

Run `fetchpath` on its own in a terminal. Downloads in progress, paused,
queued or scheduled appear in a panel above a prompt; finished ones print a
line into the window's history saying where the file went or why it stopped.

- Paste or type a link and press Enter. Fetchpath looks at it first and
  shows a card: for a video, its title and the available qualities (the best
  up to 1080p is chosen; move with the arrow keys); for a file, its name,
  type, size and whether it can resume; for a web page, a warning that it is
  not a file. Each card shows where the file will be saved. Press Enter to
  start or Esc to cancel. Add `--to FOLDER`, `--at TIME`, `--sha256 HEX` or
  `--quality Q` after the link as with `fetchpath add`. When a rule matches,
  the card names it and says why, and uses its folder and quality; a rule
  that needs a checksum keeps the card from starting until the link is given
  again with `--sha256`. `/rules` lists, adds, removes and tests rules with
  the same words as `fetchpath rules`.
- Videos need `yt-dlp` and `ffmpeg`, two free programs that are not part of
  Fetchpath. If they are missing, the terminal says so when it opens, and a
  video card offers to set them up: press **I** to see what will be
  downloaded (each program's version, publisher and licence, and the folder
  it goes to), then Enter to download and set them up. The video is looked
  at again afterwards. `/tools` shows whether they are ready.
- Type `/` to see the commands; keep typing to narrow the list, move with
  the arrow keys or the mouse wheel, and press Enter or click one to run it.
- `/settings` opens a menu of every setting: Enter or a click switches a
  setting on or off or cycles a choice, ←/→ change a number, and a folder
  opens a line to type it (Tab completes). Each change is saved at once.
- `/queue` lists the downloads; choose one to pause, resume, cancel, retry,
  remove or show it. `/pause`, `/resume`, `/cancel`, `/retry`, `/rm` and
  `/show` on their own ask which download.
- **F2** (or `/dashboard`) fills the window with the queue and, beside or
  below it, the chosen download's details: its speed over the last minute
  as a graph, and each connection with how much of its range has arrived.
  Move with the arrow keys, `j`/`k` or the mouse, and act with the keys
  named at the bottom: `p` pause, `r` resume or retry, `c` twice to cancel,
  `o` to open the folder, `x` to remove a finished one. Esc, `q` or F2 go
  back to the prompt, where anything that happened meanwhile is printed.
  `/keys` changes these keys (see below).
- Several links at once (pasted together, or `/add` with more than one)
  show one card listing them all, each with a mark: files and videos start
  marked, web pages and links that could not be read do not. Space marks or
  unmarks the chosen one, A marks all, Enter downloads the marked ones and
  Esc cancels them all.
- Fetchpath never replaces a file. When one with the same name is already
  in the folder (or another link in the same batch has that name), the card
  says so and saves the download as the next free "name (1)". If a file
  appears while a download runs, the download stops and a card asks: Enter
  keeps both, N types another name, Esc leaves it stopped; `/rename JOB
  NAME` (or `/rename` on its own) saves it under another name later.
- On a file's card, **S** takes a SHA-256 checksum (pasted in any case, with
  or without a `sha256:` prefix); the file is saved only if it matches. A
  rule that requires a checksum asks for it here. **N** gives the file
  another name.
- When an agent asks for something its access does not cover (a folder
  outside the ones you allowed, a size over its limit), a card shows which
  agent, the file, its size, the folder and why: A approves, D denies, Esc
  decides later. `/approvals` asks again; `/approve` and `/deny` take a
  download's number, as `fetchpath approve` and `fetchpath deny` do.
- `/folder` (or **Show in folder** in `/queue`) opens File Explorer at the
  download with the file selected; for a download still in progress it opens
  the folder. `fetchpath folder JOB` does the same from the command line.
- The mouse works in lists, menus and cards: click to choose, wheel to move,
  right-click to close. At the plain prompt the terminal keeps its own
  scrolling and text selection.
- Commands start with `/`: `/queue`, `/show`, `/pause`, `/resume`, `/cancel`,
  `/retry`, `/rm`, `/rename`, `/approvals`, `/approve`, `/deny`, `/dashboard`, `/history`, `/forget`, `/settings`, `/rules`, `/agents`, `/hub`, `/tools`, `/theme`, `/keys`, `/alias`, `/engine`, `/help`
  and `/quit`. `/agents` and `/hub` take the same words as `fetchpath agents`
  and `fetchpath hub`, and `/settings` has the name of this Fetchpath and the
  always-on switch.
  A download is its number in the panel or `/queue`, or the start of its id.
- Tab completes commands, downloads, setting names, folders (the ones
  recent downloads went to first) and links you downloaded before; Up and
  Down recall earlier lines, including ones from earlier sessions; Esc clears
  the line; Ctrl+C on an empty line leaves.
- Opening the terminal says what finished or failed since it was last open,
  failures first; `/history` shows the rest.
- The lines you type are kept in `cli-history`, beside `cli.toml`, with every
  link cut to its site and path: sign-ins, `?` queries and `#` fragments are
  never written. The last 500 are kept. `/forget` deletes them; your
  finished downloads stay in `/history` until you remove them.

### Making the terminal yours

The terminal keeps its own settings in `cli.toml`, beside the engine's data
in `%APPDATA%\app.fetchpath.desktop` (or the folder `FETCHPATH_APP_DATA_DIR`
names). You can edit the file, or change it from the prompt:

- `/theme` lists the themes; `/theme high-contrast` (bright colors, at least
  7:1 on a dark background), `/theme light` (for a light background),
  `/theme plain` (no colors) or `/theme default` (your terminal's colors)
  switches at once. `/theme glyphs ascii` (or `unicode`, or `auto`) picks the
  symbols; `/theme density compact` shows fewer rows and no hint line.
- `/keys` lists the keys; `/keys pause z` rebinds one, `/keys pause default`
  puts it back. The dashboard's key must be F1 to F12, since letters type at
  the prompt. Arrows, Enter, Esc, Delete and Ctrl+C always keep their
  meaning.
- `/alias dl add --to D:\Media` makes `/dl LINK` run `/add --to D:\Media
  LINK`; `/alias dl` shows it and `/alias remove dl` deletes it. Aliases
  appear in the `/` list and complete with Tab.

```toml
theme = "high-contrast"     # default, high-contrast, light or plain
glyphs = "auto"             # auto, unicode or ascii
density = "comfortable"     # or compact

[colors]                    # optional: accent, dim, good, bad, warn, check
accent = "#00afff"          # a name such as light-cyan, #rrggbb, or default

[keys]                      # dashboard, pause, resume, cancel, remove,
dashboard = "F5"            # approve, deny, folder, up, down, close
pause = "z"

[aliases]
dl = 'add --to "D:\My Media"'           # single quotes keep backslashes
tidy = ["rm 3", "history --limit 5"]     # a list runs each command in turn
```

An alias only ever runs Fetchpath's own `/` commands (not `/tools`, which
sets up outside programs); a line naming anything else, such as `cmd` or
`powershell`, is refused. A line that cannot be used is reported with its
number when the terminal opens and is skipped; the rest still apply.

Leaving never stops a download. `fetchpath --plain` prints plain lines and
redraws nothing, which suits screen readers; it is chosen automatically when
`NO_COLOR` is set, `TERM` is `dumb`, or Windows reports a screen reader.
Its cards and questions are answered by typing: Enter, `no`, a number, or
on a file's card `sha256 CHECKSUM` and `name NEW-NAME`; a batch takes the
numbers to mark or unmark; an agent's request takes `approve` or `deny`.

## AI agents (MCP)

`fetchpath mcp` lets an AI agent host (Claude Code, Claude Desktop, Codex,
VS Code and others that speak the Model Context Protocol) download through
Fetchpath. The host starts it and talks to it over standard input and
output; you never run it yourself. Give each host its own name with
`--agent`, so you can tell their requests apart and grant them separately.

### Guided setup (Codex and Claude Code)

Select **AI agents (MCP)** in setup, then explicitly tick the hosts to connect.
Both boxes start unchecked. The supported scope is your Windows user, across
projects. To connect after installing, use:

```powershell
fetchpath agent-setup status --json
fetchpath agent-setup add codex
fetchpath agent-setup add claude-code
fetchpath agent-setup check codex
fetchpath agent-setup check claude-code
fetchpath agent-setup remove codex
fetchpath agent-setup remove claude-code
```

Restart the host after connecting. Registration uses the full installed path
and `check` verifies an MCP handshake and tool discovery without a model call.
The host's own policy can still restrict loading; confirm it in the host's `/mcp` view.
Approve Fetchpath's tools in the host when prompted; that host approval is
separate from Fetchpath's folder grants and download approvals. A host running
with a never-approve policy may refuse a download tool even when Fetchpath
would allow it.
Registration grants no download access: choose folders separately with `fetchpath agents
grant NAME FOLDER` or Settings > Agent access. Requests outside a grant wait
for your approval. Automatic mode is a separate choice.

Existing entries are preserved. If `fetchpath` already exists, setup explains
the conflict; `add HOST --adopt` explicitly takes ownership of an identical
entry, and `add HOST --replace` explicitly replaces it. Close the host before
changing its configuration. `reconcile` updates owned executable paths after a
move; `cleanup` removes all still-matching owned entries, even if the host is
no longer installed. Modified entries are preserved and reported for manual
resolution. These commands do not delete downloads or revoke saved grants.
To keep an edited entry and release Fetchpath's ownership, use `fetchpath
agent-setup remove HOST --keep-config`. This leaves the host configuration
unchanged; remove that entry manually if you no longer want it.

Upgrade and repair reconcile owned connections. Removing the AI agents
component or uninstalling removes owned connections even when application
data is retained. Configuration or cleanup failures stop the installer before
removing the executable, so you can resolve them and retry. Recovery metadata
lives separately in `%LOCALAPPDATA%\FetchpathAgentSetup`; it is not part of the
optional application-data deletion.

### Manual setup (other MCP clients)

Claude Desktop, VS Code and generic stdio MCP clients use manual registration
and removal; the guided lifecycle covers Codex and Claude Code only. Manual
entries are not owned by Fetchpath and are not removed automatically.

| Host | Setup |
|---|---|
| Claude Code | `claude mcp add --scope user fetchpath -- fetchpath mcp --agent claude-code` |
| Claude Desktop | in `claude_desktop_config.json`: `"mcpServers": {"fetchpath": {"command": "fetchpath", "args": ["mcp", "--agent", "claude-desktop"]}}` |
| Codex | in `~/.codex/config.toml`: `[mcp_servers.fetchpath]` with `command = "fetchpath"` and `args = ["mcp", "--agent", "codex"]` |
| VS Code | in `.vscode/mcp.json`: `"servers": {"fetchpath": {"type": "stdio", "command": "fetchpath", "args": ["mcp", "--agent", "vscode"]}}` |

**Recommended: give the host the full path to `fetchpath.exe`**, in place of
`fetchpath` in the table. Setup puts it in `%LOCALAPPDATA%\Fetchpath` unless you
chose another folder. For Claude Code:

```powershell
claude mcp add --scope user fetchpath -- "$env:LOCALAPPDATA\Fetchpath\fetchpath.exe" mcp --agent claude-code
```

A full path keeps working whichever components you later choose in Fetchpath
setup. The bare `fetchpath` command needs the PATH entry, which setup adds only
while the Terminal or AI agents component is installed; restart the host after
installing so it sees the new PATH. If you later remove both components, a
manually configured host using the bare command stops starting (it fails closed),
while a manual full-path entry still runs under the same grants and approvals.
Guided owned registrations are removed when AI agents is deselected. To remove
manual registrations, remove only `fetchpath` from Claude Desktop's `mcpServers`
or VS Code's `servers` object; leave other entries intact. A generic MCP client
starts the full executable path with arguments `mcp --agent NAME`; remove that
server from its configuration when finished. Manual Codex and Claude Code
entries can be removed with `codex mcp remove fetchpath` and `claude mcp remove
--scope user fetchpath`. To revoke an agent's download access, use `fetchpath agents revoke` or
Settings > Agent access.

The agent gets these tools: `download` (a file, or a video at a quality;
optionally waiting, with progress), `inspect_link`, `list_downloads`,
`get_download`, `wait_for_download`, `pause`, `resume`, `cancel` and
`search_history`. What it may do is decided by Fetchpath, not by the agent:

- Downloads into folders you granted to that agent start at once. Anything
  else (another folder, a file over its size limit, more than its number of
  downloads an hour) waits for you: the desktop queue and the terminal show
  it with Approve and Deny, `fetchpath approvals` lists what waits, and
  `fetchpath approve JOB` or `fetchpath deny JOB` answers. An agent you have
  not granted anything asks for everything.
- An agent can never send a password or cookies, replace an existing file,
  or change settings, rules, sharing or paired devices; it sees and controls
  only the downloads it asked for.
- It is shown a saved file's path only inside a folder you granted it.
  Names, titles and server messages are handed to it marked as untrusted, so
  a web page cannot pass itself off as instructions from Fetchpath. A
  SHA-256 Fetchpath computes is described as what arrived, never as proof of
  who published the file.

Grant access in the desktop (Settings, AI agents) or from the command line:

```powershell
fetchpath agents                                  # who has access, where, and how much
fetchpath agents grant claude-code D:\AgentDownloads
fetchpath agents limit claude-code --size 500MB --per-hour 10
fetchpath agents auto claude-code on                  # inside its folders, never ask
fetchpath agents revoke claude-code D:\AgentDownloads   # take one folder away
fetchpath agents revoke claude-code                     # take all of its access away
```

`agents auto NAME on` is automatic mode: inside the agent's folders nothing
it downloads waits for you, whatever the size or how many an hour, and a
torrent may find peers. Saving anywhere else still waits for your approval,
and it still cannot use your sign-ins, replace a file, upload to peers or
change Fetchpath's settings. `agents auto NAME off` brings the limits back.

Taking a folder away, or revoking an agent, stops what it has not finished
there: those downloads wait for your approval again, a running one paused
at the point it reached.

These rules defend against an agent that a web page misleads. They do not
defend against a malicious program already running as you, which could read
Fetchpath's data folder like any other of your files.

## Video and audio tools

Saving video or audio needs `yt-dlp` and `ffmpeg`. Fetchpath does not ship
them; it can fetch pinned versions from their publishers and install them
only if they match the checksums recorded in this Fetchpath.

```powershell
fetchpath tools                  # are they set up, and where
fetchpath tools install          # lists what it will download, then asks
fetchpath tools install --yes    # the same without asking, for scripts
fetchpath tools use D:\Tools     # use copies you already have
```

Without a terminal, `tools install` refuses unless given `--yes`. A video
download that fails because the programs are missing says to run
`fetchpath tools install`.

## Paired devices (advanced)

Two of your own computers can share files they have already downloaded and
checked, so the second one does not fetch them again. Nothing is shared until
you turn it on, only with computers you pair yourself, and only files that
were downloaded with a checksum and without a sign-in, cookie or private link.
The receiving computer checks every byte against the SHA-256 you give it
before saving anything. The same controls are in the desktop app under
**Settings → Paired computers**.

```powershell
# On the computer that has the files: show a code, valid once for two minutes
fetchpath lan pair

# On the other computer, with the address and code the first one printed
fetchpath lan join 192.168.1.20:47632 ABCDE-FGHJK desk

# On the computer that has the files: share while Fetchpath runs
fetchpath lan on

# On the other computer: ask the paired computer first, then the link
fetchpath fetch-verified --sha256 HEX --size BYTES --peer 192.168.1.20:47631=KEY LINK DESTINATION
```

A computer that is sharing announces itself on the local network, only in a
form its paired computers can recognize. Any download with a checksum, from
`add`, the terminal or the desktop, then asks it first, checks what it
sends, and shows `from paired` in `ls`. `fetch-verified` asks the addresses
you give instead.

`fetchpath lan` shows this computer's fingerprint, whether it is sharing and
the paired computers with their keys and, when heard, their addresses; compare fingerprints on both screens
after pairing. `fetchpath lan unpair KEY` removes one, and `fetchpath lan off`
stops sharing; both take effect at once. While sharing is on, Fetchpath keeps
running in the background. You type an address only to pair, or for
`fetch-verified`. Windows Firewall may ask whether to allow Fetchpath on your
network the first time you pair or share. `fetch-verified` prints JSON.
