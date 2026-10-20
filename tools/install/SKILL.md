---
name: install-fetchpath
description: Install Fetchpath (a Windows download manager with an MCP server) on a Windows PC with no GUI, for a person or an agent on a remote computer, and connect Codex or Claude Code to it.
---

# Install Fetchpath headlessly

Windows 11 x64, current user, no administrator rights, nothing shown or asked.

1. Run the installer script (PowerShell 5.1 or 7). It downloads the latest
   release, checks it against the release's `SHA256SUMS.txt` (that shows the
   download matches what was published, not who published it), and installs
   the command line and the agent command only:

   ```powershell
   irm https://raw.githubusercontent.com/idcesares/fetchpath/main/tools/install/install.ps1 -OutFile $env:TEMP\fp-install.ps1
   # Read it, then:
   powershell -NoProfile -ExecutionPolicy Bypass -File $env:TEMP\fp-install.ps1 -AgentHosts claude-code -MediaTools
   ```

   A rerun without `-Components` keeps what is installed. Options: `-Version v0.3.0`, `-Components cli,mcp,torrent` (or `full`),
   `-AgentHosts codex,claude-code`, `-MediaTools`, `-InstallDir`, or offline
   `-SetupFile PATH -Sha256 HASH`. Exit codes: 2 bad arguments; 3 checksum or release problem; 10 setup refused the component list; 11 an agent connection failed (Fetchpath stays installed); other values come from setup itself, so a message that starts "Setup exited with code" is setup's, even for 2 or 3.

2. Restart the agent host, then check `/mcp`. Verify without a model call:
   `fetchpath agent-setup check claude-code`.
3. The connection grants no download access. Ask the person for a folder, then
   `fetchpath agents grant NAME FOLDER`. Requests outside a grant wait in the
   queue for their approval.

Connect later, with no reinstall: `fetchpath agent-setup add codex|claude-code`.
Never edit the person's agent configuration by hand when `agent-setup` can do it.
