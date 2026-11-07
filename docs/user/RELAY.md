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

This prints the enrollment key (64 hex characters) once per call and keeps it in `enrollment.key` in the state directory (mode 0600). Keep it: it is what each engine will need once Fetchpath can connect to a relay. `--rotate` makes a new key; engines holding the old one are refused from then on.

## Run

```bash
fetchpath-relay serve --state /var/lib/fetchpath-relay \
  --domain share.example.org --email you@example.org --accept-acme-terms
```

`--accept-acme-terms` agrees to the Let's Encrypt subscriber terms for you; the relay refuses to start without it. The first start gets a certificate (add `--staging` to rehearse without rate limits; staging and production keep separate accounts and certificates, so dropping the flag later gets a real certificate) and then renews it every 30 days; a failed renewal keeps serving the old certificate and logs the failure. To use your own certificate instead, pass `--cert chain.pem --key key.pem`. Ports can be changed with `--listen` and `--http-listen`.

A systemd unit that runs it as an unprivileged user needs `AmbientCapabilities=CAP_NET_BIND_SERVICE` to bind ports 80 and 443.

## What the relay can and cannot see

It terminates TLS, so it sees the request line and headers of each visit and the bytes of the encrypted file, and it logs none of them: log lines are fixed event names (`served`, `engine enrolled`, `no engine`, ...). It never sees the key in the link. Because it terminates TLS it could serve a modified download page, so run it yourself and keep the server patched. It cannot read the file, and engines that do not hold the enrollment key are refused without a reason.

## Status

The relay side is built. Fetchpath itself cannot connect to a relay yet (the engine side of FP-118 is still to come), so there is no test link to open. Once it lands, the check is a phone on mobile data (Wi-Fi off) opening a link from Fetchpath, the same gate as for tunnels.
