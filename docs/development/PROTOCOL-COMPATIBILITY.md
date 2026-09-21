# FP-016 FTP, FTPS, and SFTP compatibility

Date: 21 September 2026
Acceptance: A07, A11

## Delivered behavior

`fetchpath-http` exposes a synchronous, sequential `transfer_compatibility` transport API for `ftp://`, `ftps://`, and `sftp://` URLs. The existing HTTP and adaptive-transfer APIs are unchanged.

- Credentials are supplied through `CompatibilityContext`, never URL userinfo. Passwords and private-key passphrases use `CredentialSecret`, whose debug representation is always `[REDACTED]`.
- FTP and implicit FTPS use the packaged static libcurl build with FTP enabled. Password authentication, UTF-8 paths, bounded 64 KiB delivery, cancellation, connection/transfer timeouts, and REST-based resume are supported.
- FTPS always verifies the certificate chain and host. A caller may add a PEM CA bundle for a private server; disabling verification is not supported.
- SFTP uses Russh and Russh SFTP with either password authentication or an OpenSSH-compatible private key. An explicit OpenSSH `known_hosts` path is required, and an absent, changed, unreadable, or unmatched host key fails closed.
- SFTP paths are percent-decoded as UTF-8. Resume seeks to the requested remote offset and refuses offsets beyond the reported file size.
- Every successful call reports the protocol, requested resume offset, transferred byte count, and resulting total byte count. Chunks are emitted in ascending order for integration with the existing staging/checkpoint layer.

`CompatibilityCapabilities::detect` reports the three protocols compiled into this build. This is a packaging capability report, not a claim that every server, authentication extension, proxy, or SSH algorithm is interoperable.

## Failure contract

The compatibility path returns the shared `TransferError` with protocol-specific categories:

| Category | Meaning |
| --- | --- |
| `InvalidUrl` | Unsupported scheme, URL credentials, missing host/file path, or invalid path encoding |
| `Authentication` | Wrong credentials, incompatible authentication type, or an unreadable private key |
| `Certificate` | FTPS certificate or CA validation failed |
| `HostKey` | The SFTP key was absent from, changed in, or could not be checked against `known_hosts` |
| `ResumeRejected` | The requested offset exceeds the SFTP object or the server rejected the seek |
| `Cancelled` | The caller cancelled between bounded reads or through libcurl progress handling |
| `Transport` | Connection, negotiation, remote-path, timeout, truncation, or protocol I/O failure |
| `Sink` | The destination callback rejected a chunk |

Errors do not contain supplied passwords or passphrases. FTP/FTPS server text and SSH library diagnostics are deliberately collapsed at credential and trust boundaries to avoid leaking sensitive context.

## Real-fixture verification

Run the loopback compatibility matrix from the repository root:

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File tests/compatibility/run.ps1
```

The harness creates an ignored Python virtual environment under `work/`, installs the pinned fixture dependencies, and starts real loopback servers on ephemeral ports. The three tests run in the normal parallel test mode and cover:

- FTP password success/failure, a percent-encoded Unicode path, full transfer, and resume from byte 65,537;
- implicit FTPS rejection of an untrusted certificate, acceptance through a supplied private CA, full transfer, and resume;
- SFTP password and RSA private-key authentication, wrong-password rejection, strict correct/wrong host-key handling, a Unicode path, full transfer, and resume.

The deterministic fixture is 256 KiB and the assertions compare every output byte. Keeping the protocol tests parallel also guards the Windows regression where the former native SSH backend corrupted the process heap while FTP/FTPS ran concurrently.

## Limits carried forward

This slice is the transport contract and real compatibility evidence; wiring these schemes into the file-job CLI and desktop orchestration is separate work. FTP is plaintext. `ftps://` currently means implicit FTPS; explicit FTP upgrade (`AUTH TLS`) is not exposed. SFTP supports password and file-based private-key authentication, not agents, keyboard-interactive login, SSH certificates, jump hosts, or proxy commands.

Resume proves correct byte placement against the fixture's unchanged object, but FTP and SFTP do not provide an HTTP-style strong entity validator here. The publication/checkpoint layer must not infer publisher authenticity or remote immutability from a successful protocol transfer alone. No broad public-server compatibility claim is made beyond the tested matrix above.
