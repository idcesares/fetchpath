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

```bash
fetchpath-relay enroll --state /var/lib/fetchpath-relay
```

This prints the enrollment key (64 hex characters) once per call and keeps it in `enrollment.key` in the state directory (mode 0600). Keep it: each Fetchpath engine needs it to enroll (see below). `--rotate` makes a new key; engines holding the old one are refused from then on.

## Run

```bash
fetchpath-relay serve --state /var/lib/fetchpath-relay \
  --domain share.example.org --email you@example.org --accept-acme-terms
```

`--accept-acme-terms` agrees to the Let's Encrypt subscriber terms for you; the relay refuses to start without it. The first start gets a certificate (add `--staging` to rehearse without rate limits; staging and production keep separate accounts and certificates, so dropping the flag later gets a real certificate) and then renews it every 30 days; a failed renewal keeps serving the old certificate and logs the failure. To use your own certificate instead, pass `--cert chain.pem --key key.pem`. Ports can be changed with `--listen` and `--http-listen`.

A systemd unit that runs it as an unprivileged user needs `AmbientCapabilities=CAP_NET_BIND_SERVICE` to bind ports 80 and 443.

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

Built and tested on loopback, including the engine. Not yet tried on a real server: the Let's Encrypt certificate, a Linux build and a phone on mobile data are the owner's last check. The desktop app has no relay screen yet; use the command line.
