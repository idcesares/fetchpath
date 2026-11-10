# Running a share relay on a VPS

A relay lets Fetchpath on your computer give out `https://your-domain/s/...` links without a tunnel vendor. It runs on a small Linux server you own. It stores no files, has no accounts, and only accepts links from engines that hold its enrollment key. The file travels encrypted end to end: the key to open it stays in the part of the link after `#`, which browsers never send to the relay.

What you need: a Linux VPS with a public IP, a domain name pointing at it, and ports 80 and 443 open. Port 80 is used only to get and renew the certificate.

## Build and enroll

On the VPS (Rust 1.98 or newer and a C compiler):

```bash
git clone https://github.com/idcesares/fetchpath && cd fetchpath
cargo build --release -p fetchpath-relay --features serve
sudo install target/release/fetchpath-relay /usr/local/bin/
```

Enroll as the `fetchpath-relay` user, in the service steps below. That prints the enrollment key (64 hex characters) once and keeps it in `enrollment.key` (mode 0600). Keep it: each Fetchpath engine needs it. `--rotate` makes a new key; engines holding the old one are refused from then on.

## Run

```bash
fetchpath-relay serve --state /var/lib/fetchpath-relay \
  --domain share.example.org --email you@example.org --accept-acme-terms
```

`--accept-acme-terms` agrees to the Let's Encrypt subscriber terms for you; the relay refuses to start without it. The first start gets a certificate (add `--staging` to rehearse without rate limits; staging and production keep separate accounts and certificates, so dropping the flag later gets a real certificate) and then renews it every 30 days; a failed renewal keeps serving the old certificate and logs the failure. To use your own certificate instead, pass `--cert chain.pem --key key.pem`. Ports can be changed with `--listen` and `--http-listen`.

The unit shipped with the relay is `crates/fetchpath-relay/fetchpath-relay.service`. It runs as the user `fetchpath-relay`, keeps state in `/var/lib/fetchpath-relay` (mode 0700), and binds ports 80 and 443 with `CAP_NET_BIND_SERVICE` only.

```bash
sudo useradd --system --home-dir /var/lib/fetchpath-relay --shell /usr/sbin/nologin fetchpath-relay
sudo install -d -o fetchpath-relay -g fetchpath-relay -m 0700 /var/lib/fetchpath-relay
sudo -u fetchpath-relay fetchpath-relay enroll --state /var/lib/fetchpath-relay
sudo install -m 0600 /dev/null /etc/fetchpath-relay.env
```

Put two lines in that env file (it stays on the server, mode 0600, and it is not committed):

```text
SHARE_HOST=share.example.org
ACME_EMAIL=you@example.org
```

```bash
sudo cp crates/fetchpath-relay/fetchpath-relay.service /etc/systemd/system/
sudo systemctl daemon-reload
sudo systemctl enable --now fetchpath-relay
```

Before `ufw enable`, allow your real SSH port and keep the current session open. Then allow 80 and 443. If the provider has its own firewall, open those two ports there as well.

`journalctl -u fetchpath-relay` should show fixed event names (`relay: listening`, `relay: certificate issued`, `relay: served`). A line that contains the enrollment key or a share URL is a bug: stop the service and rotate the key.

`MemoryDenyWriteExecute` is not set. The relay's crypto library was not run under that restriction, so the unit does not guess. `AF_UNIX` stays allowed so name lookup can reach the system resolver; that path was not proven with a real certificate. If the first certificate attempt fails, systemd retries at most 3 times in 5 minutes, then stops.

## What the relay can and cannot see

It terminates TLS, so it sees the request line and headers of each visit and the bytes of the encrypted file, and it logs none of them: log lines are fixed event names (`served`, `engine enrolled`, `no engine`, ...). It never sees the key in the link. Because it terminates TLS it could serve a modified download page, so run it yourself and keep the server patched. It cannot read the file, and engines that do not hold the enrollment key are refused without a reason.

## Connect Fetchpath to it

On the computer that runs Fetchpath, pipe the key in (never put it on the command line):

```powershell
Get-Content key.txt | fetchpath share relay https://share.example.org
```

or run `fetchpath share relay https://share.example.org` and paste the key when asked. Fetchpath keeps the key sealed with your Windows account and never shows it again; run the command again with a new key after `--rotate`. It then enrolls, makes a test link and waits for you to open it. Open it on a phone using mobile data (Wi-Fi off); `fetchpath share on` is allowed only after that download worked. File links work as in the tunnel guide (`fetchpath share create FILE`), and Fetchpath listens on no local port while relay mode is on. `fetchpath share relay off` goes back to a tunnel and keeps the key; `fetchpath share relay URL --stored-key` uses it again. Changing the address, the mode or the key turns links off and ends the live ones.

If the status says the relay refused the key, the key is wrong or was rotated. Anything else (`not reachable`) is the network, the domain or the relay being down.

## Status

Built and tested on loopback, including the engine. Linux CI compiles and tests the relay. The systemd unit is checked with `systemd-analyze verify`. Not yet tried on a real server: Let's Encrypt, a phone on mobile data, and the desktop click are deferred until a VPS exists (FP-128, Isaac). In the desktop app, Settings, Public links, "Use my own relay instead of a tunnel" does the same: save the relay's address in the address field, then enter the key. The key field is cleared as soon as you press the button.
