from __future__ import annotations

import argparse
import asyncio
import datetime as dt
import json
import os
import pathlib
import socket
import ssl
import threading
import time
import urllib.parse

import asyncssh
from cryptography import x509
from cryptography.hazmat.primitives import hashes, serialization
from cryptography.hazmat.primitives.asymmetric import rsa
from cryptography.x509.oid import NameOID


HOST = "127.0.0.1"
USERNAME = "fixture-user"
PASSWORD = "fixture-password"
REMOTE_PATH = "/unicodé folder/payload.bin"


def create_tls_material(directory: pathlib.Path) -> tuple[pathlib.Path, pathlib.Path]:
    key = rsa.generate_private_key(public_exponent=65537, key_size=2048)
    subject = issuer = x509.Name([x509.NameAttribute(NameOID.COMMON_NAME, HOST)])
    now = dt.datetime.now(dt.UTC)
    certificate = (
        x509.CertificateBuilder()
        .subject_name(subject)
        .issuer_name(issuer)
        .public_key(key.public_key())
        .serial_number(x509.random_serial_number())
        .not_valid_before(now - dt.timedelta(minutes=1))
        .not_valid_after(now + dt.timedelta(days=1))
        .add_extension(x509.SubjectAlternativeName([x509.IPAddress(__import__("ipaddress").ip_address(HOST))]), False)
        .add_extension(x509.BasicConstraints(ca=True, path_length=None), True)
        .sign(key, hashes.SHA256())
    )
    key_path = directory / "ftps-key.pem"
    cert_path = directory / "ftps-cert.pem"
    key_path.write_bytes(
        key.private_bytes(
            serialization.Encoding.PEM,
            serialization.PrivateFormat.TraditionalOpenSSL,
            serialization.NoEncryption(),
        )
    )
    cert_path.write_bytes(certificate.public_bytes(serialization.Encoding.PEM))
    return cert_path, key_path


class FtpFixture:
    def __init__(self, payload: bytes, tls_context: ssl.SSLContext | None = None):
        self.payload = payload
        self.tls_context = tls_context
        self.listener = socket.socket()
        self.listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        self.listener.bind((HOST, 0))
        self.listener.listen()
        self.port = self.listener.getsockname()[1]
        threading.Thread(target=self._accept, daemon=True).start()

    def _accept(self) -> None:
        while True:
            client, _ = self.listener.accept()
            threading.Thread(target=self._serve, args=(client,), daemon=True).start()

    @staticmethod
    def _send(stream, message: str) -> None:
        stream.write((message + "\r\n").encode("utf-8"))

    def _serve(self, client: socket.socket) -> None:
        if self.tls_context is not None:
            try:
                client = self.tls_context.wrap_socket(client, server_side=True)
            except ssl.SSLError:
                client.close()
                return
        stream = client.makefile("rwb", buffering=0)
        data_listener: socket.socket | None = None
        offset = 0
        authenticated = False
        private_data = self.tls_context is None
        try:
            self._send(stream, "220 Fetchpath protocol fixture")
            while line := stream.readline():
                decoded = line.decode("utf-8", "strict").rstrip("\r\n")
                command, _, argument = decoded.partition(" ")
                command = command.upper()
                if command == "USER":
                    self._send(stream, "331 Password required")
                elif command == "PASS":
                    authenticated = argument == PASSWORD
                    self._send(stream, "230 Logged in" if authenticated else "530 Login incorrect")
                elif command in {"SYST", "CLNT"}:
                    self._send(stream, "215 UNIX Type: L8" if command == "SYST" else "200 Client accepted")
                elif command == "FEAT":
                    self._send(stream, "211-Features")
                    self._send(stream, " REST STREAM")
                    self._send(stream, " SIZE")
                    self._send(stream, " UTF8")
                    self._send(stream, "211 End")
                elif command in {"TYPE", "OPTS", "PBSZ", "NOOP"}:
                    self._send(stream, "200 OK")
                elif command == "PROT":
                    private_data = argument.upper() == "P"
                    self._send(stream, "200 Protection set")
                elif command == "PWD":
                    self._send(stream, '257 "/" is current directory')
                elif command == "CWD":
                    self._send(stream, "250 Directory changed")
                elif command == "SIZE":
                    self._send(stream, f"213 {len(self.payload)}" if authenticated else "530 Login first")
                elif command == "REST":
                    offset = int(argument)
                    self._send(stream, f"350 Restarting at {offset}")
                elif command in {"EPSV", "PASV"}:
                    if data_listener is not None:
                        data_listener.close()
                    data_listener = socket.socket()
                    data_listener.bind((HOST, 0))
                    data_listener.listen(1)
                    port = data_listener.getsockname()[1]
                    if command == "EPSV":
                        self._send(stream, f"229 Entering Extended Passive Mode (|||{port}|)")
                    else:
                        self._send(stream, f"227 Entering Passive Mode (127,0,0,1,{port // 256},{port % 256})")
                elif command == "RETR":
                    path = urllib.parse.unquote(argument)
                    if not authenticated:
                        self._send(stream, "530 Login first")
                    elif not path.endswith("payload.bin"):
                        self._send(stream, "550 File unavailable")
                    elif data_listener is None:
                        self._send(stream, "425 Use PASV first")
                    else:
                        self._send(stream, "150 Opening binary data connection")
                        data, _ = data_listener.accept()
                        if self.tls_context is not None and private_data:
                            data = self.tls_context.wrap_socket(data, server_side=True)
                        data.sendall(self.payload[offset:])
                        if isinstance(data, ssl.SSLSocket):
                            data = data.unwrap()
                        data.close()
                        data_listener.close()
                        data_listener = None
                        offset = 0
                        self._send(stream, "226 Transfer complete")
                elif command == "QUIT":
                    self._send(stream, "221 Goodbye")
                    return
                else:
                    self._send(stream, "502 Command not implemented")
        finally:
            if data_listener is not None:
                data_listener.close()
            stream.close()
            client.close()


class PasswordAndKeyServer(asyncssh.SSHServer):
    def __init__(self, accepted_key: asyncssh.SSHKey):
        self.accepted_key = accepted_key

    def begin_auth(self, username: str) -> bool:
        return True

    def password_auth_supported(self) -> bool:
        return True

    def validate_password(self, username: str, password: str) -> bool:
        return username == USERNAME and password == PASSWORD

    def public_key_auth_supported(self) -> bool:
        return True

    def validate_public_key(self, username: str, key: asyncssh.SSHKey) -> bool:
        return (
            username == USERNAME
            and key.export_public_key() == self.accepted_key.export_public_key()
        )


class SftpFixture:
    def __init__(self, root: pathlib.Path, directory: pathlib.Path):
        self.host_key = asyncssh.generate_private_key("ssh-rsa")
        self.client_key = asyncssh.generate_private_key("ssh-rsa")
        self.client_key_path = directory / "sftp-client-key.pem"
        self.client_key.write_private_key(self.client_key_path, "pkcs1-pem")
        self.root = root
        self.ready = threading.Event()
        self.failure: Exception | None = None
        threading.Thread(target=self._run, daemon=True).start()
        if not self.ready.wait(10):
            raise RuntimeError("timed out starting SFTP fixture")
        if self.failure is not None:
            raise self.failure

    def _run(self) -> None:
        loop = asyncio.new_event_loop()
        asyncio.set_event_loop(loop)

        async def start() -> None:
            listener = await asyncssh.listen(
                HOST,
                0,
                server_factory=lambda: PasswordAndKeyServer(self.client_key),
                server_host_keys=[self.host_key],
                password_auth=True,
                public_key_auth=True,
                sftp_factory=lambda channel: asyncssh.SFTPServer(
                    channel, chroot=os.fsencode(self.root)
                ),
            )
            self.port = listener.get_port()
            self.ready.set()

        try:
            loop.run_until_complete(start())
            loop.run_forever()
        except Exception as error:
            self.failure = error
            self.ready.set()


def write_known_host(path: pathlib.Path, port: int, key: asyncssh.SSHKey) -> None:
    public = key.export_public_key().decode("ascii").strip()
    path.write_text(f"[{HOST}]:{port} {public}\n", encoding="utf-8")


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--state", required=True, type=pathlib.Path)
    args = parser.parse_args()
    directory = args.state.parent.resolve()
    directory.mkdir(parents=True, exist_ok=True)
    root = directory / "root"
    payload_path = root / "unicodé folder" / "payload.bin"
    payload_path.parent.mkdir(parents=True, exist_ok=True)
    payload = bytes(range(256)) * 1024
    payload_path.write_bytes(payload)

    cert_path, key_path = create_tls_material(directory)
    tls_context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    tls_context.load_cert_chain(cert_path, key_path)
    ftp = FtpFixture(payload)
    ftps = FtpFixture(payload, tls_context)
    sftp = SftpFixture(root, directory)
    known_hosts = directory / "known_hosts"
    wrong_known_hosts = directory / "wrong_known_hosts"
    write_known_host(known_hosts, sftp.port, sftp.host_key)
    write_known_host(wrong_known_hosts, sftp.port, asyncssh.generate_private_key("ssh-rsa"))

    state = {
        "username": USERNAME,
        "password": PASSWORD,
        "payload_path": str(payload_path),
        "ftp_url": f"ftp://{HOST}:{ftp.port}{urllib.parse.quote(REMOTE_PATH)}",
        "ftps_url": f"ftps://{HOST}:{ftps.port}{urllib.parse.quote(REMOTE_PATH)}",
        "sftp_url": f"sftp://{HOST}:{sftp.port}{urllib.parse.quote(REMOTE_PATH)}",
        "ca_path": str(cert_path),
        "known_hosts": str(known_hosts),
        "wrong_known_hosts": str(wrong_known_hosts),
        "private_key": str(sftp.client_key_path),
    }
    temporary = args.state.with_suffix(".tmp")
    temporary.write_text(json.dumps(state), encoding="utf-8")
    os.replace(temporary, args.state)
    while True:
        time.sleep(1)


if __name__ == "__main__":
    main()
