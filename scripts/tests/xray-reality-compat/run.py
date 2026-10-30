#!/usr/bin/env python3
"""Exercise REALITY admission through real local server/client processes."""

import argparse
import concurrent.futures
import hashlib
import http.server
import json
import pathlib
import socket
import socketserver
import ssl
import subprocess
import tempfile
import threading
import time

PAYLOAD = b"brocade-reality-compatibility-proof\n"
USER = "4510f783-de00-4f17-a78b-9d5ecb720186"


class OriginHandler(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        self.send_response(200)
        self.send_header("Content-Length", str(len(PAYLOAD)))
        self.end_headers()
        self.wfile.write(PAYLOAD)

    def log_message(self, *_args):
        pass


def unused_port():
    with socket.socket() as listener:
        listener.bind(("127.0.0.1", 0))
        return listener.getsockname()[1]


def wait_listener(port, process):
    deadline = time.monotonic() + 5
    while time.monotonic() < deadline:
        if process.poll() is not None:
            raise RuntimeError("Xray exited during startup")
        try:
            with socket.create_connection(("127.0.0.1", port), timeout=0.1):
                return
        except OSError:
            time.sleep(0.025)
    raise RuntimeError("Xray listener startup timed out")


def stop(process):
    if process.poll() is None:
        process.terminate()
        try:
            process.wait(timeout=2)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait(timeout=2)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", required=True, type=pathlib.Path)
    parser.add_argument("--client-binary", type=pathlib.Path)
    parser.add_argument("--label", required=True)
    parser.add_argument("--strict", action="store_true")
    parser.add_argument("--empty-cover", action="store_true")
    parser.add_argument("--report", type=pathlib.Path)
    args = parser.parse_args()
    args.binary = args.binary.resolve(strict=True)
    client_binary = (args.client_binary or args.binary).resolve(strict=True)
    directory = pathlib.Path(tempfile.mkdtemp(prefix="brocade-reality-compat-"))
    certificate, key = directory / "cover.pem", directory / "cover.key"
    subprocess.run(
        ["openssl", "req", "-x509", "-newkey", "ec", "-pkeyopt",
         "ec_paramgen_curve:P-256", "-noenc", "-keyout", str(key), "-out",
         str(certificate), "-days", "1", "-subj", "/CN=compat.example",
         "-addext", "subjectAltName=DNS:compat.example"],
        check=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=10,
    )
    context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    context.minimum_version = context.maximum_version = ssl.TLSVersion.TLSv1_3
    context.load_cert_chain(str(certificate), str(key))
    if not args.empty_cover:
        context.set_alpn_protocols(["h2", "http/1.1"])

    class CoverHandler(socketserver.BaseRequestHandler):
        def handle(self):
            try:
                # A five-second cover timeout races REALITY's five-second cold
                # record detector and can kill an already authenticated client.
                self.request.settimeout(20)
                with context.wrap_socket(self.request, server_side=True) as connection:
                    connection.recv(4096)
                    connection.sendall(
                        b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                    )
            except OSError:
                pass  # Wrong credentials deliberately fall back to the cover.

    class CoverServer(socketserver.ThreadingTCPServer):
        daemon_threads = True

    generated = subprocess.run(
        [str(args.binary), "x25519"], check=True, capture_output=True, text=True, timeout=5,
    )
    fields = dict(line.split(":", 1) for line in generated.stdout.splitlines() if ":" in line)
    private = fields.get("PrivateKey", "").strip()
    public = fields.get("Password (PublicKey)", fields.get("Password", fields.get("PublicKey", ""))).strip()
    if not private or not public:
        raise RuntimeError("x25519 did not produce a key pair")

    with CoverServer(("127.0.0.1", 0), CoverHandler) as cover, http.server.ThreadingHTTPServer(
        ("127.0.0.1", 0), OriginHandler
    ) as origin:
        for server in (cover, origin):
            threading.Thread(target=server.serve_forever, daemon=True).start()

        def check(case):
            fingerprint, invalid = case
            folder = directory / (fingerprint + ("-" + invalid if invalid else ""))
            folder.mkdir()
            server_port, socks_port = unused_port(), unused_port()
            settings = {
                "target": f"127.0.0.1:{cover.server_address[1]}",
                "serverNames": ["compat.example"], "privateKey": private,
                "shortIds": ["0123456789abcdef"],
            }
            # Omission exercises Brocade's compatibility default for existing snapshots.
            if args.strict:
                settings["allowLegacyX25519"] = False
            server_config = {
                "log": {"loglevel": "info"},
                "inbounds": [{"listen": "127.0.0.1", "port": server_port, "protocol": "vless",
                              "settings": {"clients": [{"id": USER}], "decryption": "none"},
                              "streamSettings": {"network": "tcp", "security": "reality", "realitySettings": settings}}],
                "outbounds": [{"protocol": "freedom", "settings": {"finalRules": [{"action": "allow", "ip": ["127.0.0.0/8"]}]}}],
            }
            client_config = {
                "log": {"loglevel": "info"},
                "inbounds": [{"listen": "127.0.0.1", "port": socks_port, "protocol": "socks", "settings": {"udp": False}}],
                "outbounds": [{"protocol": "vless", "settings": {"vnext": [{"address": "127.0.0.1", "port": server_port,
                              "users": [{"id": USER, "encryption": "none"}]}]},
                              "streamSettings": {"network": "tcp", "security": "reality", "realitySettings": {
                                  "serverName": "wrong.example" if invalid == "sni" else "compat.example",
                                  "fingerprint": fingerprint, "publicKey": public,
                                  "shortId": "0000000000000000" if invalid == "shortid" else "0123456789abcdef"}}}],
            }
            processes, logs = [], []
            result = {"fingerprint": fingerprint, "invalid": invalid, "ok": False}
            try:
                for role, config, binary, port in (
                    ("server", server_config, args.binary, server_port),
                    ("client", client_config, client_binary, socks_port),
                ):
                    path = folder / (role + ".json")
                    path.write_text(json.dumps(config))
                    path.chmod(0o600)
                    log = (folder / (role + ".log")).open("wb")
                    logs.append(log)
                    process = subprocess.Popen([str(binary), "run", "-config", str(path)], stdout=log, stderr=subprocess.STDOUT)
                    processes.append(process)
                    wait_listener(port, process)
                # REALITY's cold cover detector can consume five seconds of this budget.
                response = subprocess.run(
                    ["curl", "--silent", "--show-error", "--noproxy", "", "--max-time", "12",
                     "--socks5-hostname", f"127.0.0.1:{socks_port}", f"http://127.0.0.1:{origin.server_port}/payload"],
                    capture_output=True, timeout=15,
                )
                result.update(ok=response.returncode == 0 and response.stdout == PAYLOAD,
                              curl_exit=response.returncode, error=response.stderr.decode(errors="replace").strip())
            except Exception as error:
                result["error"] = str(error)
            finally:
                for process in reversed(processes):
                    stop(process)
                for log in logs:
                    log.close()
            return result

        cases = [(fingerprint, "") for fingerprint in
                 ("edge", "ios", "qq", "chrome", "firefox", "safari", "random", "randomized", "android", "360")]
        cases += [(fingerprint, invalid) for fingerprint in ("edge", "chrome") for invalid in ("shortid", "sni")]
        try:
            with concurrent.futures.ThreadPoolExecutor(max_workers=4) as pool:
                results = list(pool.map(check, cases))
        finally:
            origin.shutdown()
            cover.shutdown()

    failures = []
    for result in results:
        fingerprint = result["fingerprint"]
        if "curl_exit" not in result:
            failures.append(result)
        elif fingerprint not in {"random", "randomized"}:
            expected = (not result["invalid"] and fingerprint not in {"android", "360"}
                        and (not args.strict or fingerprint not in {"edge", "ios", "qq"}))
            if result["ok"] != expected:
                failures.append(result)
    report = {
        "label": args.label, "strict": args.strict, "empty_cover": args.empty_cover,
        "binary_sha256": hashlib.sha256(args.binary.read_bytes()).hexdigest(),
        "client_sha256": hashlib.sha256(client_binary.read_bytes()).hexdigest(),
        "cases": results, "failures": failures, "logs": str(directory),
    }
    (args.report or directory / "report.json").write_text(json.dumps(report, indent=2) + "\n")
    print(json.dumps({"cases": len(results), "failures": failures, "logs": str(directory)}, indent=2), flush=True)
    if failures:
        raise SystemExit(1)


if __name__ == "__main__":
    main()
