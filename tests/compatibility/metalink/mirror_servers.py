"""Loopback Metalink mirror fixtures for FP-019.

Starts four real HTTP mirrors on ephemeral loopback ports and writes two
Metalink 4 documents plus a small state file describing them:

* ``healthy``  - serves the correct bytes and honours byte ranges;
* ``corrupt``  - serves one damaged piece, so a trusted piece hash localizes it;
* ``slow``     - stalls between small chunks until the client gives up;
* ``offline``  - a released loopback port, so connections are refused outright.

Only the Python standard library is used, so this harness needs no virtual
environment and no package downloads.
"""

from __future__ import annotations

import argparse
import hashlib
import http.server
import json
import pathlib
import socket
import threading
import time

HOST = "127.0.0.1"
PAYLOAD_BYTES = 512 * 1024
PIECE_LENGTH = 64 * 1024
DAMAGED_PIECE = 3
SLOW_CHUNK_BYTES = 256
SLOW_CHUNK_DELAY_SECONDS = 0.5


def build_payload() -> bytes:
    return bytes((index * 37 + index // 251) % 256 for index in range(PAYLOAD_BYTES))


def damage(payload: bytes, piece: int, within: int = 11, mask: int = 0xFF) -> bytes:
    """Flips one byte inside one piece, leaving every other piece intact."""
    damaged = bytearray(payload)
    damaged[piece * PIECE_LENGTH + within] ^= mask
    return bytes(damaged)


def piece_hashes(payload: bytes) -> list[str]:
    return [
        hashlib.sha256(payload[start : start + PIECE_LENGTH]).hexdigest()
        for start in range(0, len(payload), PIECE_LENGTH)
    ]


class MirrorHandler(http.server.BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"
    body = b""
    chunk_bytes = 64 * 1024
    chunk_delay = 0.0

    def log_message(self, *_args) -> None:  # noqa: D102 - keep fixtures quiet
        return

    def do_GET(self) -> None:  # noqa: N802 - BaseHTTPRequestHandler contract
        body = type(self).body
        start, end = 0, len(body) - 1
        status = 200
        header_range = self.headers.get("Range")
        if header_range and header_range.startswith("bytes="):
            raw_start, _, raw_end = header_range[len("bytes=") :].partition("-")
            try:
                start = int(raw_start)
                end = int(raw_end) if raw_end else len(body) - 1
            except ValueError:
                self.send_error(416)
                return
            if start > end or end >= len(body):
                self.send_error(416)
                return
            status = 206
        selected = body[start : end + 1]

        self.send_response(status)
        self.send_header("Accept-Ranges", "bytes")
        self.send_header("Content-Length", str(len(selected)))
        if status == 206:
            self.send_header("Content-Range", f"bytes {start}-{end}/{len(body)}")
        self.send_header("Connection", "close")
        self.end_headers()
        step = type(self).chunk_bytes
        delay = type(self).chunk_delay
        try:
            for offset in range(0, len(selected), step):
                if delay:
                    time.sleep(delay)
                self.wfile.write(selected[offset : offset + step])
                self.wfile.flush()
        except (BrokenPipeError, ConnectionResetError, OSError):
            return


def start_mirror(body: bytes, chunk_bytes: int, chunk_delay: float) -> str:
    handler = type(
        "BoundMirrorHandler",
        (MirrorHandler,),
        {"body": body, "chunk_bytes": chunk_bytes, "chunk_delay": chunk_delay},
    )
    server = http.server.ThreadingHTTPServer((HOST, 0), handler)
    server.daemon_threads = True
    threading.Thread(target=server.serve_forever, daemon=True).start()
    host, port = server.server_address[:2]
    return f"http://{host}:{port}/payload.bin"


def reserve_offline_url() -> str:
    """A loopback port with nothing behind it, so connections are refused.

    The probe socket is closed: a bound-but-unlistening Windows socket makes the
    client wait instead of refusing, which is the slow-mirror case, not the
    offline one. This is called only after every real mirror holds its port.
    """
    probe = socket.socket()
    probe.bind((HOST, 0))
    address = probe.getsockname()
    probe.close()
    return f"http://{address[0]}:{address[1]}/payload.bin"


def metalink_document(
    name: str,
    size: int,
    mirrors: list[tuple[str, int]],
    whole_file_sha256: str,
    pieces: list[str] | None,
) -> str:
    lines = [
        '<?xml version="1.0" encoding="UTF-8"?>',
        '<metalink xmlns="urn:ietf:params:xml:ns:metalink">',
        f'  <file name="{name}">',
        f"    <size>{size}</size>",
        f'    <hash type="sha-256">{whole_file_sha256}</hash>',
    ]
    for url, priority in mirrors:
        lines.append(f'    <url priority="{priority}">{url}</url>')
    if pieces is not None:
        lines.append(f'    <pieces length="{PIECE_LENGTH}" type="sha-256">')
        lines.extend(f"      <hash>{value}</hash>" for value in pieces)
        lines.append("    </pieces>")
    lines.extend(["  </file>", "</metalink>", ""])
    return "\n".join(lines)


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--state", required=True)
    arguments = parser.parse_args()
    state_path = pathlib.Path(arguments.state)
    fixture_dir = state_path.parent
    fixture_dir.mkdir(parents=True, exist_ok=True)

    payload = build_payload()
    payload_path = fixture_dir / "payload.bin"
    payload_path.write_bytes(payload)
    whole_file = hashlib.sha256(payload).hexdigest()
    pieces = piece_hashes(payload)

    corrupt_url = start_mirror(damage(payload, DAMAGED_PIECE), 64 * 1024, 0.0)
    slow_url = start_mirror(payload, SLOW_CHUNK_BYTES, SLOW_CHUNK_DELAY_SECONDS)
    healthy_url = start_mirror(payload, 64 * 1024, 0.0)
    # Damaged in the *same* piece as the corrupt mirror, so no mirror pair can
    # repair it between them.
    hopeless_url = start_mirror(
        damage(payload, DAMAGED_PIECE, within=21, mask=0x0F), 64 * 1024, 0.0
    )
    # Reserved only once every real mirror holds its port.
    offline_url = reserve_offline_url()

    # Priorities deliberately put the bad, slow, and offline mirrors first, so
    # measured behavior - not the advisory ordering - has to carry the transfer.
    mixed = metalink_document(
        "payload.bin",
        len(payload),
        [(offline_url, 1), (slow_url, 2), (corrupt_url, 3), (healthy_url, 4)],
        whole_file,
        pieces,
    )
    mixed_path = fixture_dir / "mixed-mirrors.meta4"
    mixed_path.write_text(mixed, encoding="utf-8")

    # The same mirror set without a piece map: a mismatch localizes nothing and
    # must produce a conservative restart, never a repair claim.
    final_only = metalink_document(
        "payload.bin",
        len(payload),
        [(corrupt_url, 1), (healthy_url, 2)],
        whole_file,
        None,
    )
    final_only_path = fixture_dir / "final-hash-only.meta4"
    final_only_path.write_text(final_only, encoding="utf-8")

    # Every mirror damaged: nothing may reach a destination.
    hopeless = metalink_document(
        "payload.bin",
        len(payload),
        [(corrupt_url, 1), (hopeless_url, 2)],
        whole_file,
        pieces,
    )
    hopeless_path = fixture_dir / "all-mirrors-damaged.meta4"
    hopeless_path.write_text(hopeless, encoding="utf-8")

    state = {
        "payload_path": str(payload_path),
        "payload_sha256": whole_file,
        "payload_bytes": len(payload),
        "piece_length": PIECE_LENGTH,
        "piece_count": len(pieces),
        "damaged_piece": DAMAGED_PIECE,
        "mixed_metalink": str(mixed_path),
        "final_hash_only_metalink": str(final_only_path),
        "all_damaged_metalink": str(hopeless_path),
        "healthy_url": healthy_url,
        "corrupt_url": corrupt_url,
        "slow_url": slow_url,
        "offline_url": offline_url,
        "hopeless_url": hopeless_url,
    }
    state_path.write_text(json.dumps(state, indent=2), encoding="utf-8")

    while True:
        time.sleep(3600)


if __name__ == "__main__":
    main()
