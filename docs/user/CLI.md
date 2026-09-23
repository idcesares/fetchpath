# The `fetchpath` command

The installer adds `fetchpath` to your PATH. Open a **new** terminal after
installing so it's found.

```powershell
fetchpath download LINK [DESTINATION] [--sha256 HEX] [--json] [--quiet]
fetchpath --version
fetchpath --help
```

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
| 2 | Bad input: a missing or invalid link, destination or option |
| 3 | A file already exists at the destination |
| 4 | Network or server problem |
| 5 | The file didn't match `--sha256`; nothing was saved |
| 6 | Couldn't write to the destination |
| 130 | Cancelled with Ctrl+C |

## Not in this version

The command line downloads one file at a time and doesn't share the desktop
app's queue. Use the desktop app for queues, schedules, pause and resume,
video and audio, and links from your browser.
