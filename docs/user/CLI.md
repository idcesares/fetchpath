# The `fetchpath` command

The installer adds `fetchpath` to your PATH. Open a **new** terminal after
installing so it's found.

```powershell
fetchpath download LINK [DESTINATION] [--sha256 HEX] [--json] [--quiet]
fetchpath add LINK... [--to FOLDER|FILE] [--sha256 HEX] [--quality Q] [--at TIME] [--wait]
fetchpath ls | show | pause | resume | cancel | retry | rm | watch | history
fetchpath inspect LINK | batch FILE | settings [NAME [VALUE]] | engine status | engine stop
fetchpath --version
fetchpath --help
```

Every command works on one shared queue, kept by the Fetchpath engine, a
background process that starts by itself when a command needs it and stops
about a minute after the last download finishes and the last command ends.
Closing a terminal does not stop downloads.

## Downloading

```powershell
# Into your Downloads folder, named after the link
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

## Checking a checksum

```powershell
fetchpath download https://example.com/tool.zip --sha256 3f1c…e9a2
```

The file is saved only if its SHA-256 matches. If it doesn't, nothing is saved
and Fetchpath exits with code 5, printing both values.

Without `--sha256`, Fetchpath prints the SHA-256 of what it received. That
fingerprint was computed on your computer. It says which bytes you have, not
who published them.

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

# Video or audio: see the formats, then pick one (or best, or audio)
fetchpath inspect https://video.example/watch/123
fetchpath add https://video.example/watch/123 --quality 720p

# One link per line, optionally followed by a destination; # starts a comment
fetchpath batch links.txt --to D:\Downloads\
Get-Content links.txt | fetchpath batch -
```

`--to` works like `download`'s destination; without it, downloads go to the
default folder from `fetchpath settings default-destination-dir`, or to
Downloads. `--wait` stays until the downloads end and exits with their code.

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
```

A download is named by its number in `fetchpath ls` (up to four digits) or by
the start of its id, which must match only one.

### Settings

`fetchpath settings` lists every setting, `fetchpath settings NAME` shows one
and `fetchpath settings NAME VALUE` changes it. Switches take `on` or `off`,
folders take a path or `none`. The engine keeps values in range and prints the
value it applied. The desktop app shows the same settings.

### The engine

`fetchpath engine status` says whether it is running; `fetchpath engine stop`
stops it after saving every download's progress, and the next command starts
it again and carries on.

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

## Paired devices (advanced)

Two of your own computers can share files they have already downloaded and
checked, so the second one does not fetch them again. Nothing is shared until
you turn it on, only with devices you pair yourself, and only files that were
downloaded without a sign-in, cookie or private link. The receiving computer
checks every byte against the SHA-256 you give it before saving anything.

```powershell
# On the computer that has the files: shows a code for two minutes
fetchpath lan pair-host 0.0.0.0:47631

# On the other computer, within those two minutes
fetchpath lan pair-join 192.168.1.20:47631 ABCDE-FGHJK laptop

# On the computer that has the files: turn sharing on, then share while it runs
fetchpath lan enable
fetchpath lan serve 0.0.0.0:47631

# On the other computer: ask the paired device first, then the link
fetchpath fetch-verified --sha256 HEX --size BYTES --peer 192.168.1.20:47631=KEY LINK DESTINATION
```

`pair-join` prints the other device's `key`. `fetchpath lan peers` lists paired
devices and `fetchpath lan unpair KEY` removes one; a running `serve` stops
serving it within two seconds. `fetchpath lan disable` stops sharing the same
way. Devices are not discovered automatically; you give the address. Windows
Firewall may ask whether to allow Fetchpath on your network the first time you
run `serve`.

These commands print JSON and are meant for people comfortable with a
terminal. The desktop app does not show them yet.
