# Linux VPS validation and follow-on plan

Proposed 10 November 2026. **Deferred:** Isaac has no VPS, so checklist A is future work (FP-128, blocked, owner Isaac). Nothing in checklist A has been run. Task state lives in [the backlog](../tasks/backlog.json). A pass is recorded in [SHARE-RELAY.md](../development/SHARE-RELAY.md) only after the commands below were actually run, and that record omits the enrollment key, any share URL, and anything after `#`.

**Goal.** Prove the relay that is already on main (FP-118) on a Linux VPS the owner already has, with a real Let's Encrypt certificate and a phone on mobile data. Then, only if he accepts them, add the Linux pieces that make that setup repeatable and let a Linux engine enroll.

**Constraints.** No new paid service. The project runs no relay. HTTP-01 to Let's Encrypt, ports 80 and 443, on a Debian or Ubuntu VPS he already controls. A DNS name he already controls points straight at that VPS (no proxy answering port 80). Security before convenience. Do not invent a pass.

**Placeholders.** `SHARE_HOST` is the DNS name. `VPS` is the SSH target he already uses. `EMAIL` is the Let's Encrypt account address. Keep them out of git.

## Recommended order

1. Owner runs checklist A (FP-128). That is the real gate. No new code.
2. If a step fails, fix that failure. Leave FP-129 and later proposed.
3. After A passes, accept FP-129 then FP-130 so the next boot is a reviewed unit, not a hand-written one.
4. Accept FP-131 when a tarball or `.deb` should replace `cargo build` on the VPS.
5. Linux client only if he wants one: FP-132, then FP-133, then FP-134, then checklist B. FP-135 (desktop) stays optional.
6. FP-119 (Direct mode) stays last. It opens the home PC. The VPS relay does not need it.

`node tools/tasks.mjs next` stays empty until he sets a proposed ticket to `todo` or starts FP-119 / FP-128.

## Open backlog

### FP-119 Direct mode — `todo`, ready, P3, strong review

Needs a domain on the home connection, ACME on the Windows engine, a Windows Firewall rule for the share port only, and an exposure warning. No UPnP. A DNS API token is a secret and is not the default path. Depends on FP-117 (done).

Risk: the home PC is on the internet. High. Secret handling needs the strong review.

Validate: gate X7 on that home connection with a phone on mobile data. From outside, only the share port answers. Do this after FP-128, and only if he still wants it.

### FP-126 Inbox recovery notice — `done`

Shipped. Desktop and CLI show the notice from FP-122. Nothing left to validate.

### FP-128 Gate X7 on a real VPS — `todo`, ready, P3

Needs the VPS, the DNS name, Let's Encrypt, the Windows engine, a phone on mobile data, and a click through Settings → Storage and sharing → Public links. Depends on FP-118 (done). Owned by Isaac.

Risk: ports 80 and 443 are public; the enrollment key enrolls any engine that has it; the relay terminates TLS and could serve a modified page. That last point is accepted for a relay he runs himself ([RELAY.md](../user/RELAY.md)).

Validate: checklist A. Record the commit SHA, the date, and pass or fail per step.

### FP-120 LAN send — `blocked`, P3, strong review

Needs an explicit send to a paired device, sender-attested manifest, per-send agent approval. Depends on FP-115 (done) and FP-091 (blocked).

Risk: shared pairing identity. Stays blocked until the FP-091 review finishes.

Validate: paired send works; unpaired device refused; agent waits; revoke mid-transfer stops it.

### FP-091 Remote hub design review — `blocked`, P2, strong review

The design is written (`docs/architecture/specs/2026-10-03-instance-access-and-remote-hub-design.md`). What remains is the independent review of authentication, authorization, replay, and browser boundaries. Depends on FP-047 and FP-067 (done).

Risk: high if implementation starts before that review. This task builds nothing.

Validate: the review exists and its findings are applied. Then it can be marked done.

### FP-092 Remote control listener — `blocked`, P3, strong review

Needs FP-091's review, then the private-gateway listener, device enrollment, and the small web client. Default install still exposes nothing.

Risk: high (remote credentials). Blocked on FP-091.

Validate: spec gates G4–G7 and G9, after the review.

### FP-093 Retrieval and automatic delivery — `blocked`, P3, strong review

Design only, after FP-092. Depends on FP-091 and FP-092.

Risk: high (file authorization). Blocked until then.

Validate: the design walks through retrieval, expiry, and revocation. No service is deployed.

## Proposed Linux tickets

Status `proposed` means not accepted. Set one to `todo` to make it ready. Details and file lists are on the task.

| ID | What | Depends | Who after acceptance | Review |
|---|---|---|---|---|
| FP-129 | Linux CI: relay tests and release build | none | agent | standard |
| FP-130 | systemd unit and VPS notes in the user guide | FP-129 | agent; owner applies it on the VPS | strong |
| FP-131 | linux-x64 tarball and `.deb` for the relay | FP-130 | agent | standard |
| FP-132 | Secret Service for the enrollment key; refuse with no keyring | none | agent | strong |
| FP-133 | Linux engine and `fetchpath share` over a Unix socket | FP-132 | agent | strong |
| FP-134 | Linux engine enrolls; owner repeats the phone test | FP-128, FP-133 | agent, then owner | strong |
| FP-135 | Linux desktop AppImage or `.deb` | FP-134 | agent, then owner | standard |

Acceptance, one line each:

- **FP-129.** Ubuntu 24.04 CI, Rust 1.98.1, `cargo test -p fetchpath-relay --features serve --locked` and the release build are green. That is not X7.
- **FP-130.** `systemd-analyze verify` passes. The unit matches the flags in `fetchpath-relay`'s usage. Journal text must not grow a key or a URL. `MemoryDenyWriteExecute` stays off until a real serve shows aws-lc still runs.
- **FP-131.** Tarball plus SHA256, and a `.deb` that installs the unit. Package removal leaves `/var/lib/fetchpath-relay`. No AppImage for the relay. No release is published.
- **FP-132.** Secret Service round-trip. No keyring means an error and no file. No cleartext path. Windows DPAPI stays. Browser-inbox sealing stays Windows-only.
- **FP-133.** Socket directory mode 0700, `SO_PEERCRED`, same handshake. Linux CI runs the relay tests and the share-relay loopback test. Desktop, browser host, and media helpers stay out. A publication-fence gap stops the ticket.
- **FP-134.** Same CLI sentences as Windows (`Enrolled with the relay`, refusal text below). Done only after checklist B's phone download.
- **FP-135.** Optional. Public links panel enrolls on Ubuntu 24.04. The relay scenario does not need it.

Packaging choice to confirm: relay gets a static-enough tarball and a `.deb`. AppImage waits for the desktop (FP-135). Key storage follows [PLATFORMS.md](../product/PLATFORMS.md): Secret Service, and refuse when no keyring runs.

## Checklist A — relay that exists today

Owner. An agent can later wrap A2's build and the unit file (FP-129, FP-130). DNS, the phone, and the desktop click stay with the owner.

Replace `SHARE_HOST`, `VPS`, and `EMAIL`. Rust on the VPS is 1.98.1, the version CI installs. `aws-lc-sys` 0.45 needs CMake.

### A1. Name and ports

1. **DNS (owner).** After the A record exists:

   ```bash
   dig +short A SHARE_HOST
   ```

   Pass: the VPS public IPv4, and no other address. Stop if it is anything else. Do not start a certificate attempt.

2. **SSH and toolchain (owner).**

   ```bash
   ssh VPS
   . /etc/os-release && echo "$PRETTY_NAME"
   sudo apt-get update
   sudo apt-get install -y build-essential cmake pkg-config git curl ca-certificates
   curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain 1.98.1 --profile minimal
   . "$HOME/.cargo/env"
   rustc --version
   sudo ss -lntup | grep -E ':80 |:443 ' || echo '80 and 443 are free'
   ```

   Pass: `rustc 1.98.1` or newer, and the ports are free. Stop if something else owns 80 or 443.

3. **Firewall (owner).** Keep this SSH session open. If SSH is not port 22, allow that port before `ufw enable`. If a firewall is already in place, keep it and only add 80 and 443. Also open 80 and 443 in the provider panel if the VPS has one.

   ```bash
   sudo apt-get install -y ufw
   sudo ufw default deny incoming
   sudo ufw default allow outgoing
   sudo ufw allow OpenSSH
   sudo ufw allow 80/tcp
   sudo ufw allow 443/tcp
   sudo ufw --force enable
   sudo ufw status
   ```

   Pass: a second SSH session still connects, and 22 (or the real SSH port), 80, and 443 are ALLOW. If the second session fails, use the provider console and disable ufw. Do not close the first session until the second works.

### A2. Build, key, certificate

4. **Build and test (owner now; FP-129 later).**

   ```bash
   git clone https://github.com/idcesares/fetchpath
   cd fetchpath
   git rev-parse HEAD
   cargo test -p fetchpath-relay --features serve --locked
   cargo build --release -p fetchpath-relay --features serve --locked
   sudo install -m 0755 target/release/fetchpath-relay /usr/local/bin/fetchpath-relay
   ```

   Pass: tests pass and the install exists. Write down the commit SHA. A failed test is a stop.

5. **User and key (owner).** The key prints once. Put it in a password manager. Do not put it in the shell history, this repo, or a chat.

   ```bash
   sudo useradd --system --home-dir /var/lib/fetchpath-relay --shell /usr/sbin/nologin fetchpath-relay
   sudo install -d -o fetchpath-relay -g fetchpath-relay -m 0700 /var/lib/fetchpath-relay
   sudo -u fetchpath-relay fetchpath-relay enroll --state /var/lib/fetchpath-relay
   sudo -u fetchpath-relay stat -c '%a %n' /var/lib/fetchpath-relay /var/lib/fetchpath-relay/enrollment.key
   ```

   Pass: one line of 64 hex characters, then `700` and `600`.

6. **Certificate (owner).** Write `/etc/fetchpath-relay.env` as root, mode `0600`:

   ```text
   SHARE_HOST=share.example.org
   ACME_EMAIL=you@example.org
   ```

   Write `/etc/systemd/system/fetchpath-relay.service` (this copy is for the check; FP-130 replaces it with a reviewed file):

   ```ini
   [Unit]
   Description=Fetchpath share relay
   After=network-online.target
   Wants=network-online.target

   [Service]
   Type=simple
   User=fetchpath-relay
   Group=fetchpath-relay
   EnvironmentFile=/etc/fetchpath-relay.env
   StateDirectory=fetchpath-relay
   StateDirectoryMode=0700
   ExecStart=/usr/local/bin/fetchpath-relay serve --state /var/lib/fetchpath-relay --domain ${SHARE_HOST} --email ${ACME_EMAIL} --accept-acme-terms --staging
   Restart=on-failure
   RestartSec=10
   AmbientCapabilities=CAP_NET_BIND_SERVICE
   CapabilityBoundingSet=CAP_NET_BIND_SERVICE
   NoNewPrivileges=yes
   ProtectSystem=strict
   ProtectHome=yes
   PrivateTmp=yes
   PrivateDevices=yes
   ProtectKernelTunables=yes
   ProtectKernelModules=yes
   ProtectControlGroups=yes
   RestrictAddressFamilies=AF_INET AF_INET6
   LockPersonality=yes
   RestrictRealtime=yes

   [Install]
   WantedBy=multi-user.target
   ```

   Leave `MemoryDenyWriteExecute` unset. aws-lc may need writable executable pages; setting it without a passing serve would hide a working relay.

   ```bash
   sudo systemctl daemon-reload
   sudo systemctl enable --now fetchpath-relay
   sudo journalctl -u fetchpath-relay -n 40 --no-pager
   ```

   Pass for staging: the journal contains `relay: listening` and `relay: certificate issued`, and it does not contain the enrollment key. From a laptop:

   ```bash
   echo | openssl s_client -connect SHARE_HOST:443 -servername SHARE_HOST 2>/dev/null | openssl x509 -noout -issuer -ext subjectAltName -dates
   curl -sS -o /dev/null -w 'http %{http_code}\n' --max-time 20 "http://SHARE_HOST/"
   ```

   Pass: issuer contains `STAGING`, the name is `SHARE_HOST`, and HTTP prints `http 404`.

   Then take `--staging` out of `ExecStart`, and:

   ```bash
   sudo systemctl daemon-reload
   sudo systemctl restart fetchpath-relay
   sudo journalctl -u fetchpath-relay -n 40 --no-pager
   echo | openssl s_client -connect SHARE_HOST:443 -servername SHARE_HOST 2>/dev/null | openssl x509 -noout -issuer -ext subjectAltName -dates
   curl -sS -o /dev/null -w 'https %{http_code}\n' --max-time 20 "https://SHARE_HOST/"
   sudo -u fetchpath-relay stat -c '%a %n' /var/lib/fetchpath-relay /var/lib/fetchpath-relay/enrollment.key
   ```

   Pass: journal has `relay: certificate issued` or `relay: certificate loaded`, plus `relay: listening`. Issuer contains `Let's Encrypt` and does not contain `STAGING`. SAN is `SHARE_HOST`. `notAfter` is in the future. curl prints `https 404` and exits 0. Modes are still `700` and `600`. One failed validation is a stop; do not loop. Let's Encrypt's failed-authorization limit is easy to hit.

### A3. Windows engine, phone, desktop

7. **Enroll and phone (owner).** On the Windows PC, key from a file you delete, never as an argument. Wi-Fi off on the phone before opening the link.

   ```powershell
   Get-Content -Raw key.txt | fetchpath share relay https://SHARE_HOST
   Remove-Item key.txt
   ```

   The command waits up to 15 minutes. On the phone, open the printed link and press Download.

   Pass, on the PC: `Enrolled with the relay at https://SHARE_HOST.` then `The test link was delivered. File links are still off: run fetchpath share on to turn them on.`

   Pass, on the VPS: `sudo journalctl -u fetchpath-relay -n 20 --no-pager` contains `relay: engine enrolled name=fetchpath` and `relay: served`, and contains neither the key nor the link.

   Then:

   ```powershell
   fetchpath share on
   fetchpath share
   ```

   Pass: `File links are on.` Status shows `Enrolled with the relay.`, `A relay key is stored`, `Test link: delivered`, `File links: on`.

8. **Desktop click (owner).** Settings → Storage and sharing → Public links. Put `https://SHARE_HOST` in the address. Open **Use my own relay instead of a tunnel**, paste the key, press **Use the relay**.

   Pass: the key field is empty after the click, and `fetchpath share` still says `Enrolled with the relay.` A failed click is a failed FP-128 even if step 7 passed. Record the sentence on screen.

### A4. Failure and rollback

9. **Wrong key (owner).** This replaces the stored key and ends live links. The real key stays in the password manager.

   ```powershell
   $bad = '0' * 64
   $bad | fetchpath share relay https://SHARE_HOST
   ```

   Pass, within about 10 seconds: `The relay refused this engine's key. Enroll again with the relay's key.` VPS journal contains `relay: engine refused` and no reason and no key.

   Restore:

   ```powershell
   Get-Content -Raw key.txt | fetchpath share relay https://SHARE_HOST
   Remove-Item key.txt
   ```

   Pass: `Enrolled with the relay` again. Run a new phone download if you still need `share on`; the bad key cleared the previous test.

10. **Relay stopped (owner).**

    ```bash
    sudo systemctl stop fetchpath-relay
    ```

    On Windows, `fetchpath share test` (or a fresh `fetchpath share relay https://SHARE_HOST --stored-key`) prints `The relay is not reachable right now.` or `The relay did not answer.` Then:

    ```bash
    sudo systemctl start fetchpath-relay
    sudo journalctl -u fetchpath-relay -n 15 --no-pager
    ```

    Pass: `relay: certificate loaded` (the stored production cert is under 30 days) and `relay: listening`. Windows enrolls again with `--stored-key`.

11. **Leave it or take it down (owner).**

    - Keep it: leave the unit enabled. The state directory holds the key and the certificate. Do not commit either.
    - Stop sharing from the PC: `fetchpath share relay off` (the sealed key stays) or `fetchpath share off`.
    - Key was pasted somewhere unsafe: on the VPS, `sudo -u fetchpath-relay fetchpath-relay enroll --state /var/lib/fetchpath-relay --rotate`, then enroll the PC again with the new line. Old engines are refused.
    - Abandon the name: `sudo systemctl disable --now fetchpath-relay`, remove the A record, and leave the state directory until you are sure you will not restart (deleting it throws away the account and the key).

12. **Write the record (owner).** In `docs/development/SHARE-RELAY.md`, add the commit SHA, the date, and pass or fail for steps 6–10. Omit the key, the link, the fragment, and the QR file. Then FP-128 can be marked done. This plan's existence is not that record.

## Checklist B — Linux engine, after FP-134

Do not run this against a binary that does not exist yet. Owner for the phone. Agent for the CI half of FP-134.

1. Install the Linux CLI from that ticket's artifact. `fetchpath share relay https://SHARE_HOST` with the key on stdin.
2. Pass is the same two sentences as A3 step 7, then a phone on mobile data (Wi-Fi off) downloads the test file, and the VPS journal shows `relay: served`.
3. Wrong key and restore, same commands as A4 step 9, on the Linux machine. Pass is the same refusal sentence.

FP-135, if accepted: install the AppImage or `.deb`, open Public links, press **Use the relay**, and `fetchpath share` says `Enrolled with the relay.`

## What an agent can automate later

- FP-129 and the `systemd-analyze verify` half of FP-130.
- FP-131's tarball, `.deb`, and a clean-container install.
- FP-132 and FP-133 tests.
- The loopback half of FP-134.

DNS, the provider firewall, Let's Encrypt against the real name, the phone, the desktop click, and the SHARE-RELAY record stay with the owner.

## Decisions for Isaac

1. Which VPS and DNS name you already have. This plan assumes Debian or Ubuntu and a direct A record. Say if either is wrong.
2. The Let's Encrypt email. It goes in `/etc/fetchpath-relay.env` on the VPS, not in git.
3. Run checklist A before any proposed ticket. Recommended.
4. Accept FP-129 and FP-130 after A passes. Accept FP-131 only when you want a package.
5. Whether a Linux engine is in scope. If it is, accept FP-132 then FP-133 then FP-134. FP-135 stays optional; the phone test does not need the desktop.
6. Leave FP-119 until after X7, and skip it if the home PC should stay off the internet.
7. Leave FP-091, FP-092, FP-093, and FP-120 blocked. They are the remote-control hub, not this relay.
