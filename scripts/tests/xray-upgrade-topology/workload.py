#!/usr/bin/env python3
"""Guest workload shared by the disposable Docker and KVM topology runners."""
import concurrent.futures
import hashlib
import http.client
import http.server
import ipaddress
import json
import os
import pathlib
import socket
import socketserver
import ssl
import struct
import subprocess
import sys
import threading
import time
import traceback
import uuid

ROOT = pathlib.Path("/lab")
INPUTS, RESULTS = ROOT / "inputs", ROOT / "results"
IPS = {"client": "10.234.77.10", "entry": "10.234.77.20", "exit-a": "10.234.77.30", "exit-b": "10.234.77.40"}
IP6 = {role: "fd42:b10c:ade::" + address.rsplit(".", 1)[1] for role, address in IPS.items()}
USER = "4510f783-de00-4f17-a78b-9d5ecb720186"
EMAIL = "topology-test@example.invalid"
PAYLOAD = bytes(range(256)) * 1024


def save(path, value):
    temporary = path.with_suffix(".tmp")
    temporary.write_text(json.dumps(value, indent=2) + "\n")
    temporary.replace(path)


def wait(port, process=None, host="127.0.0.1"):
    deadline = time.monotonic() + 10
    while time.monotonic() < deadline:
        if process is not None and process.poll() is not None:
            raise RuntimeError("Xray exited during startup")
        try:
            with socket.create_connection((host, port), timeout=0.2):
                return
        except OSError:
            time.sleep(0.05)
    raise RuntimeError("listener startup timed out")


def stop(process):
    if process.poll() is None:
        process.terminate()
        try:
            process.wait(timeout=3)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait(timeout=3)


def impair(role):
    subprocess.run(["ip", "link", "set", "dev", "eth0", "mtu", "1280"], check=True, timeout=5)
    subprocess.run(["tc", "qdisc", "replace", "dev", "eth0", "root", "netem", "limit", "1000",
                    "delay", "20ms", "5ms", "loss", "1%", "rate", "100mbit"], check=True, timeout=5)
    qdisc = subprocess.check_output(["tc", "qdisc", "show", "dev", "eth0"], text=True, timeout=5)
    link = json.loads(subprocess.check_output(["ip", "-j", "link", "show", "eth0"], text=True, timeout=5))
    if "netem" not in qdisc or "loss 1%" not in qdisc or link[0]["mtu"] != 1280:
        raise RuntimeError("network impairment did not take effect")
    save(RESULTS / (role + "-netem.json"), {"qdisc": qdisc.strip(), "mtu": link[0]["mtu"]})


def tls_settings(server=False):
    return ({"certificates": [{"certificateFile": "/lab/inputs/cover.pem", "keyFile": "/lab/inputs/cover.key"}]}
            if server else {"serverName": "cover.test", "certificates": [{"certificateFile": "/lab/inputs/cover.pem", "usage": "verify"}]})


def configurations(identity):
    def hop(role):
        address = IPS[role] if role == "exit-a" else IP6[role]
        return {"tag": role, "protocol": "vless", "settings": {"address": address, "port": 20000,
                "id": USER, "encryption": "mlkem768x25519plus.native.0rtt." + identity["public"]},
                "mux": {"enabled": True, "concurrency": 4, "xudpConcurrency": 4, "xudpProxyUDP443": "allow",
                        "workerPool": {"prewarmWorkers": 1, "reuseThreshold": 2, "maxProbingWorkers": 1,
                                       "probeIntervalMs": 2000, "probeTimeoutMs": 500, "idleTtlMs": 5000, "maxRequestsPerWorker": 64}}}
    reality = {"network": "tcp", "security": "reality", "realitySettings": {
        "target": IPS["exit-a"] + ":8443", "serverNames": ["cover.test"], "privateKey": identity["private"],
        "shortIds": ["0123456789abcdef"]}}
    configs = {"entry": {"inbounds": [
        {"tag": "vless", "listen": "::", "port": 24443, "protocol": "vless",
         "settings": {"clients": [{"id": USER, "email": EMAIL, "flow": "xtls-rprx-vision"}], "decryption": "none"}, "streamSettings": reality},
        {"tag": "anytls", "listen": "::", "port": 24444, "protocol": "anytls",
         "settings": {"users": [{"password": USER, "email": "anytls@" + EMAIL}]},
         "streamSettings": {"network": "tcp", "security": "tls", "tlsSettings": tls_settings(True)}},
        {"tag": "hy2", "listen": "::", "port": 24443, "protocol": "hysteria",
         "settings": {"version": 2, "users": [{"auth": USER, "email": "hy2@" + EMAIL}]},
         "streamSettings": {"network": "hysteria", "security": "tls", "tlsSettings": tls_settings(True), "hysteriaSettings": {"version": 2}}},
    ], "outbounds": [hop("exit-a"), hop("exit-b")],
        "routing": {"domainStrategy": "AsIs", "rules": [{"type": "field", "domain": ["full:b.test"], "outboundTag": "exit-b"}]}}}
    for role in ("exit-a", "exit-b"):
        configs[role] = {"inbounds": [{"tag": "relay", "listen": "::", "port": 20000, "protocol": "vless",
            "settings": {"clients": [{"id": USER, "email": role + "@" + EMAIL}],
                         "decryption": "mlkem768x25519plus.native.600s." + identity["private"]}}],
            "dns": {"servers": [{"address": IPS[role], "port": 1053, "queryStrategy": "UseIP"}]},
            "outbounds": [{"protocol": "freedom", "settings": {"domainStrategy": "UseIP"}}]}
    for config in configs.values():
        config["log"] = {"loglevel": "info"}
        config["api"] = {"tag": "api", "listen": "0.0.0.0:10085", "services": ["HandlerService", "StatsService"]}
        config["stats"] = {}
        config["policy"] = {"levels": {"0": {"statsUserUplink": True, "statsUserDownlink": True}}}
    return configs


def serve(role):
    identity = json.loads((INPUTS / "identity.json").read_text())
    servers = []
    if role.startswith("exit-"):
        class HTTP(http.server.BaseHTTPRequestHandler):
            def do_GET(self):
                if self.path == "/generate_204":
                    self.send_response(204)
                    self.end_headers()
                    return
                self.reply(PAYLOAD)

            def do_POST(self):
                length = int(self.headers["Content-Length"])
                if length > 1024 * 1024:
                    raise ValueError("oversized fixture request")
                self.reply(self.rfile.read(length))

            def reply(self, body):
                self.send_response(200)
                self.send_header("Content-Length", str(len(body)))
                self.send_header("X-Lab-Exit", role)
                self.end_headers()
                self.wfile.write(body)

            def log_message(self, *_args):
                pass

        class HTTP6(http.server.ThreadingHTTPServer):
            address_family = socket.AF_INET6
            daemon_threads = True

        for port in (8080, 9443):
            server = HTTP6(("::", port), HTTP)
            if port == 9443:
                context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
                context.load_cert_chain(str(INPUTS / "cover.pem"), str(INPUTS / "cover.key"))
                server.socket = context.wrap_socket(server.socket, server_side=True)
            servers.append(server)

        class UDP(socketserver.BaseRequestHandler):
            def handle(self):
                data, connection = self.request
                connection.sendto(data, self.client_address)

        class DNS(socketserver.BaseRequestHandler):
            def handle(self):
                data, connection = self.request
                index = 12
                while data[index]:
                    index += 1 + data[index]
                end = index + 5
                kind = int.from_bytes(data[index + 1:index + 3], "big")
                address = ipaddress.ip_address(IPS[role] if kind == 1 else IP6[role]).packed
                answer = b"\xc0\x0c" + struct.pack("!HHIH", kind, 1, 30, len(address)) + address if kind in (1, 28) else b""
                response = data[:2] + struct.pack("!HHHHH", 0x8180, 1, bool(answer), 0, 0) + data[12:end] + answer
                connection.sendto(response, self.client_address)

        class UDP6(socketserver.ThreadingUDPServer):
            address_family = socket.AF_INET6
            daemon_threads = True
        servers.extend([socketserver.ThreadingUDPServer(("0.0.0.0", 1053), DNS), UDP6(("::", 9001), UDP)])
        if role == "exit-a":
            context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
            context.minimum_version = context.maximum_version = ssl.TLSVersion.TLSv1_3
            context.load_cert_chain(str(INPUTS / "cover.pem"), str(INPUTS / "cover.key"))
            context.set_alpn_protocols(["h2", "http/1.1"])

            class Cover(socketserver.BaseRequestHandler):
                def handle(self):
                    try:
                        self.request.settimeout(20)
                        with context.wrap_socket(self.request, server_side=True) as connection:
                            connection.recv(4096)
                            connection.sendall(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n")
                    except OSError:
                        pass

            class TLSCover(socketserver.ThreadingTCPServer):
                daemon_threads = True
            servers.append(TLSCover(("0.0.0.0", 8443), Cover))
        for server in servers:
            threading.Thread(target=server.serve_forever, daemon=True).start()
    config = configurations(identity)[role]
    path = RESULTS / (role + ".json")
    save(path, config)
    versions = json.loads((INPUTS / "versions.json").read_text())
    binary = INPUTS / ("xray-old" if versions.get(role) == "old" else "xray")
    with (RESULTS / (role + ".log")).open("wb") as log:
        process = subprocess.Popen([str(binary), "run", "-config", str(path)], stdout=log, stderr=subprocess.STDOUT)
        reverse_process = None
        try:
            wait(10085, process)
            if role in ("entry", "exit-a"):
                reverse = json.loads((INPUTS / ("rendered-reverse-" + role + ".json")).read_text())
                reverse["log"] = {"loglevel": "info"}
                # Keep the compiler's complete reverse account, outbound and route
                # contracts. Unused public frontend and background Internet probes
                # are outside this isolated reverse-only process.
                reverse["inbounds"] = [i for i in reverse["inbounds"] if i["tag"] == "api" or i["tag"].startswith("in:hop:") or i["tag"] == "in:relay/i-relay"]
                for inbound in reverse["inbounds"]:
                    if inbound["tag"] == "in:relay/i-relay":
                        # The original ingress has runtime grants. Supply a test
                        # grant and local plain socket; retain its rendered tag
                        # and routing into the untouched reverse carrier.
                        inbound["port"] = 24453
                        inbound["streamSettings"] = {"network": "tcp"}
                        inbound["settings"]["clients"] = [{"id": USER, "email": EMAIL}]
                reverse.pop("geodata", None)
                if "burstObservatory" in reverse:
                    reverse["burstObservatory"]["pingConfig"]["destination"] = "http://" + IPS["exit-a"] + ":8080/generate_204"
                path = RESULTS / (role + "-reverse.json")
                save(path, reverse)
                with (RESULTS / (role + "-reverse.log")).open("wb") as reverse_log:
                    reverse_process = subprocess.Popen([str(binary), "run", "-config", str(path)], stdout=reverse_log, stderr=subprocess.STDOUT)
                wait(10086, reverse_process)
            save(RESULTS / (role + "-ready.json"), {"role": role, "version": versions.get(role, "new"), "kernel": subprocess.check_output(["uname", "-r"], text=True).strip()})
            while process.poll() is None:
                if reverse_process is not None and reverse_process.poll() is not None:
                    raise RuntimeError("rendered reverse process exited")
                if (RESULTS / "netem-request").exists() and not (RESULTS / (role + "-netem.json")).exists():
                    impair(role)
                if reverse_process is not None:
                    for stage in ("normal", "netem"):
                        snapshot = RESULTS / (role + "-reverse-health-" + stage + ".json")
                        if (RESULTS / ("health-request-" + stage)).exists() and not snapshot.exists():
                            health = json.loads(subprocess.check_output([str(binary), "api", "reversehealth", "--server=127.0.0.1:10086"], text=True, timeout=3))
                            if sum(w["state"] == "READY" and w["acks"] > 0 for w in health["workers"]) >= 2:
                                save(snapshot, health)
                if role == "entry" and (RESULTS / "kernel-tls-request").exists() and not (RESULTS / "entry-kernel-tls.json").exists():
                    path = pathlib.Path("/proc/net/tls_stat")
                    save(RESULTS / "entry-kernel-tls.json", dict(line.split() for line in path.read_text().splitlines()) if path.exists() else {})
                time.sleep(0.1)
            raise RuntimeError("node Xray exited unexpectedly")
        finally:
            if reverse_process is not None:
                stop(reverse_process)
            stop(process)


def receive(connection, length):
    data = b""
    while len(data) < length:
        part = connection.recv(length - len(data))
        if not part:
            raise EOFError("truncated SOCKS or payload")
        data += part
    return data


def socks(host, port, command=1):
    connection = socket.create_connection(("127.0.0.1", 1080), timeout=15)
    connection.sendall(b"\x05\x01\x00")
    if receive(connection, 2) != b"\x05\x00":
        raise RuntimeError("SOCKS method rejected")
    domain = host.encode()
    connection.sendall(b"\x05" + bytes([command]) + b"\x00\x03" + bytes([len(domain)]) + domain + port.to_bytes(2, "big"))
    header = receive(connection, 4)
    if header[1] != 0:
        raise RuntimeError("SOCKS connection rejected")
    length = 4 if header[3] == 1 else 16 if header[3] == 4 else receive(connection, 1)[0]
    address = receive(connection, length)
    bound_port = int.from_bytes(receive(connection, 2), "big")
    return connection, address, bound_port


def request(host, tls=False, upload=False):
    connection, _, _ = socks(host, 9443 if tls else 8080)
    if tls:
        context = ssl.create_default_context(cafile=str(INPUTS / "cover.pem"))
        connection = context.wrap_socket(connection, server_hostname=host)
    with connection:
        method = "POST" if upload else "GET"
        body = PAYLOAD if upload else b""
        sent = f"{method} /payload HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\nContent-Length: {len(body)}\r\n\r\n".encode() + body
        connection.sendall(sent)
        received = [0]

        class CountedFile:
            def __init__(self, raw):
                self.raw = raw

            def read(self, *args):
                data = self.raw.read(*args)
                received[0] += len(data)
                return data

            def readline(self, *args):
                data = self.raw.readline(*args)
                received[0] += len(data)
                return data

            def __getattr__(self, name):
                return getattr(self.raw, name)

        class CountedSocket:
            def makefile(self, *args):
                return CountedFile(connection.makefile(*args))

        response = http.client.HTTPResponse(CountedSocket())
        response.begin()
        actual = response.read(1024 * 1024)
        expected = "exit-a" if host == "a.test" else "exit-b"
        if response.status != 200 or response.getheader("X-Lab-Exit") != expected or actual != PAYLOAD:
            raise RuntimeError("wrong exit, HTTP status or payload")
        return {"uplink": len(sent), "downlink": received[0]}


def user_counters(protocol):
    email = EMAIL if protocol == "vless" else ("anytls@" if protocol == "anytls" else "hy2@") + EMAIL
    prefix = "user>>>" + email + ">>>traffic>>>"
    stats = json.loads(subprocess.check_output([str(INPUTS / "xray"), "api", "statsquery", "--server=" + IPS["entry"] + ":10085", "-pattern", prefix], text=True, timeout=10))
    values = {item["name"]: int(item.get("value", 0)) for item in stats.get("stat", [])}
    return {direction: values.get(prefix + direction, 0) for direction in ("uplink", "downlink")}


def udp(host, impaired=False):
    control, _, port = socks("0.0.0.0", 0, command=3)
    with control, socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as connection:
        connection.settimeout(3 if impaired else 15)
        name = host.encode()
        sent = 64 if impaired else 1
        expected = {index.to_bytes(2, "big") + PAYLOAD[:998] for index in range(sent)}
        for payload in sorted(expected):
            connection.sendto(b"\x00\x00\x00\x03" + bytes([len(name)]) + name + (9001).to_bytes(2, "big") + payload, ("127.0.0.1", port))
            if impaired:
                time.sleep(0.005)
        received = set()
        deadline = time.monotonic() + (3 if impaired else 15)
        while len(received) < sent and time.monotonic() < deadline:
            connection.settimeout(max(0.01, deadline - time.monotonic()))
            try:
                data = connection.recv(4096)
            except TimeoutError:
                break
            offset = 4 + (4 if data[3] == 1 else 16 if data[3] == 4 else 1 + data[4]) + 2
            if data[:3] != b"\0\0\0" or data[offset:] not in expected:
                raise RuntimeError("UDP payload corruption")
            received.add(data[offset:])
        # Datagram loss is expected under injected loss. Verify every delivered
        # payload and a bounded batch loss rate, without retrying the test green.
        if len(received) < (48 if impaired else 1):
            raise RuntimeError(f"excessive UDP loss: {len(received)}/{sent}")
        return {"sent": sent, "received": len(received), "bytes_verified": len(received) * 1000}


def exercise():
    deadline = time.monotonic() + 180
    while not all((RESULTS / (role + "-ready.json")).exists() for role in ("entry", "exit-a", "exit-b")):
        if time.monotonic() > deadline:
            raise RuntimeError("topology nodes did not become ready")
        time.sleep(0.2)
    identity = json.loads((INPUTS / "identity.json").read_text())
    records = []

    def wait_health(stage):
        (RESULTS / ("health-request-" + stage)).write_text("snapshot\n")
        deadline = time.monotonic() + 20
        while not all((RESULTS / (role + "-reverse-health-" + stage + ".json")).exists() for role in ("entry", "exit-a")):
            if time.monotonic() > deadline:
                raise RuntimeError("rendered reverse did not establish two healthy workers on both ends")
            time.sleep(0.1)

    wait_health("normal")

    if (INPUTS / "tls-kernel.test").exists():
        subprocess.run(["modprobe", "tls"], check=True, timeout=10)
        before = dict(line.split() for line in pathlib.Path("/proc/net/tls_stat").read_text().splitlines())
        for name in ("tls-kernel.test", "anytls-kernel.test"):
            with (RESULTS / (name + ".log")).open("w") as log:
                checked = subprocess.run([str(INPUTS / name), "-test.v", "-test.run=^TestKernelTLS", "-test.timeout=90s"], stdout=log, stderr=log, timeout=100)
            output = (RESULTS / (name + ".log")).read_text()
            if checked.returncode or "--- SKIP:" in output or "--- PASS:" not in output:
                raise RuntimeError("real kTLS test failed or skipped: " + name)
        after = dict(line.split() for line in pathlib.Path("/proc/net/tls_stat").read_text().splitlines())
        delta = int(after["TlsTxSw"]) - int(before["TlsTxSw"])
        if delta <= 0:
            raise RuntimeError("kTLS tests did not create a kernel TX connection")
        save(RESULTS / "ktls-kernel-counters.json", {"before": before, "after": after, "tx_software_connections": delta})
        records.append({"case": "kernel-tls-tx-writev-splice-and-resumption", "passed": True, "kernel_tx_connections": delta, "bytes_verified": 0})
        save(RESULTS / "evidence.json", records)

    if (INPUTS / "shared-budget-tests").exists():
        for name, splice in (("traffic_limit_budget::real_xray_test::traffic_limit_real_xray_generations_share_one_budget_for_existing_tcp_and_udp", "on"),
                             ("user_traffic::tests::user_global_native_tcp_udp_forwarding_and_existing_sessions_share_budget", "on"),
                             ("user_traffic::tests::user_global_native_tcp_udp_forwarding_and_existing_sessions_share_budget", "off")):
            path = RESULTS / (name.split("::")[-1] + "-splice-" + splice + ".log")
            before = dict(line.split() for line in pathlib.Path("/proc/net/tls_stat").read_text().splitlines())
            with path.open("w") as log:
                checked = subprocess.run([str(INPUTS / "agent-tests"), "--exact", name, "--ignored", "--nocapture"],
                    env={**os.environ, "BROCADE_XRAY_BIN": str(INPUTS / "xray"), "BROCADE_REQUIRE_KTLS": "1", "BROCADE_E2E_ANYTLS_SPLICE": splice},
                    stdout=log, stderr=log, timeout=150)
            text = path.read_text()
            if checked.returncode or "1 passed; 0 failed; 0 ignored" not in text:
                raise RuntimeError("shared budget/kTLS E2E failed: " + str(path))
            after = dict(line.split() for line in pathlib.Path("/proc/net/tls_stat").read_text().splitlines())
            records.append({"case": name.split("::")[-1] + "-splice-" + splice, "passed": True, "bytes_verified": 0,
                            "kernel_tx_connections": int(after["TlsTxSw"]) - int(before["TlsTxSw"])})
            save(RESULTS / "evidence.json", records)

    def reverse_case(udp_mode, stage):
        began = time.monotonic()
        def exchange(index):
            target = 9001 if udp_mode else 8080
            header = b"\0" + uuid.UUID(USER).bytes + b"\0" + bytes([2 if udp_mode else 1])
            header += target.to_bytes(2, "big") + b"\x01" + socket.inet_aton(IPS["exit-a"])
            with socket.create_connection((IPS["entry"], 24453), timeout=15) as connection:
                connection.settimeout(15)
                if udp_mode:
                    payload = index.to_bytes(2, "big") + PAYLOAD[:998]
                    connection.sendall(header + len(payload).to_bytes(2, "big") + payload)
                    if receive(connection, 2) != b"\0\0":
                        raise RuntimeError("invalid reverse VLESS response")
                    length = int.from_bytes(receive(connection, 2), "big")
                    if receive(connection, length) != payload:
                        raise RuntimeError("reverse UDP corruption")
                else:
                    connection.sendall(header + b"GET /payload HTTP/1.1\r\nHost: a.test\r\nConnection: close\r\n\r\n")
                    if receive(connection, 2) != b"\0\0":
                        raise RuntimeError("invalid reverse VLESS response")
                    response = http.client.HTTPResponse(connection)
                    response.begin()
                    actual = response.read(1024 * 1024)
                    if actual != PAYLOAD or response.getheader("X-Lab-Exit") != "exit-a":
                        raise RuntimeError(f"reverse TCP wrong exit or payload: status={response.status}, exit={response.getheader('X-Lab-Exit')}, bytes={len(actual)}/{len(PAYLOAD)}, sha256={hashlib.sha256(actual).hexdigest()}")
        with concurrent.futures.ThreadPoolExecutor(max_workers=8) as pool:
            list(pool.map(exchange, range(8)))
        records.append({"case": "rendered-reverse-" + ("udp-" if udp_mode else "tcp-") + stage, "passed": True,
                        "bytes_verified": 8 * (1000 if udp_mode else len(PAYLOAD)), "seconds": round(time.monotonic() - began, 3)})
        save(RESULTS / "evidence.json", records)

    def case(protocol, fingerprint, version, impaired=False):
        stream = {"network": "tcp", "security": "tls", "tlsSettings": tls_settings()}
        if protocol == "vless":
            outbound = {"protocol": "vless", "settings": {"address": IPS["entry"], "port": 24443, "id": USER, "encryption": "none", "flow": "xtls-rprx-vision"}}
            stream = {"network": "tcp", "security": "reality", "realitySettings": {"serverName": "cover.test", "fingerprint": fingerprint, "publicKey": identity["public"], "shortId": "0123456789abcdef"}}
        elif protocol == "anytls":
            outbound = {"protocol": "anytls", "settings": {"address": IPS["entry"], "port": 24444, "password": USER}}
        else:
            outbound = {"protocol": "hysteria", "settings": {"version": 2, "address": IPS["entry"], "port": 24443}}
            stream.update(network="hysteria", hysteriaSettings={"version": 2, "auth": USER})
        outbound["streamSettings"] = stream
        config = {"log": {"loglevel": "info"}, "inbounds": [{"listen": "127.0.0.1", "port": 1080, "protocol": "socks", "settings": {"udp": True}}], "outbounds": [outbound]}
        label = f"{version}-{protocol}-{fingerprint}-{'netem' if impaired else 'normal'}"
        path = RESULTS / (label + ".json")
        save(path, config)
        binary = INPUTS / ("xray-old" if version == "old" else "xray")
        with (RESULTS / (label + ".log")).open("wb") as log:
            process = subprocess.Popen([str(binary), "run", "-config", str(path)], stdout=log, stderr=subprocess.STDOUT)
            began = time.monotonic()
            try:
                wait(1080, process)
                datagrams = []
                billing = []
                for host in ("a.test", "b.test"):
                    request(host, tls=True)
                    before = user_counters(protocol)
                    expected = request(host, upload=True)
                    after = user_counters(protocol)
                    measured = {direction: after[direction] - before[direction] for direction in before}
                    if measured != expected:
                        raise RuntimeError(f"{protocol} accounting differs from actual application bytes: {measured} != {expected}")
                    billing.append({"host": host, "expected": expected, "measured": measured})
                    datagrams.append(udp(host, impaired))
                records.append({"case": label, "passed": True, "bytes_verified": 4 * len(PAYLOAD) + sum(d["bytes_verified"] for d in datagrams),
                                "udp": datagrams, "billing": billing, "seconds": round(time.monotonic() - began, 3)})
                save(RESULTS / "evidence.json", records)
            finally:
                stop(process)

    for version in ("new", "old"):
        for fingerprint in ("chrome", "firefox", "safari", "edge", "ios", "qq"):
            case("vless", fingerprint, version)
        case("anytls", "tls", version)
        case("hy2", "quic", version)
    reverse_case(False, "normal")
    reverse_case(True, "normal")
    # Only guest/container interfaces are changed; no host qdisc or routes are touched.
    (RESULTS / "netem-request").write_text("apply\n")
    impair("client")
    deadline = time.monotonic() + 15
    while not all((RESULTS / (role + "-netem.json")).exists() for role in ("entry", "exit-a", "exit-b")):
        if time.monotonic() > deadline:
            raise RuntimeError("network impairment was not confirmed on all nodes")
        time.sleep(0.1)
    wait_health("netem")
    for protocol, fingerprint in (("vless", "chrome"), ("anytls", "tls"), ("hy2", "quic")):
        case(protocol, fingerprint, "new", impaired=True)
    reverse_case(False, "netem")
    reverse_case(True, "netem")
    counters = subprocess.check_output([str(INPUTS / "xray"), "api", "statsquery", "--server=" + IPS["entry"] + ":10085", "-pattern", "user>>>"], text=True, timeout=10)
    stats = json.loads(counters)
    if not any(int(counter.get("value", 0)) > 0 for counter in stats.get("stat", [])):
        raise RuntimeError("no authenticated user traffic counters")
    save(RESULTS / "stats.json", stats)
    performance = json.loads(subprocess.check_output([str(INPUTS / "xray"), "api", "statsquery", "--server=" + IPS["entry"] + ":10085", "-pattern", "anytls>>>performance>>>"], text=True, timeout=10))
    save(RESULTS / "anytls-performance.json", performance)
    if (INPUTS / "tls-kernel.test").exists():
        counters = {item["name"]: int(item.get("value", 0)) for item in performance.get("stat", [])}
        if counters.get("anytls>>>performance>>>ktls_authenticated_connections", 0) <= 0:
            raise RuntimeError("real AnyTLS ingress never authenticated a kTLS connection")
        (RESULTS / "kernel-tls-request").write_text("snapshot\n")
        deadline = time.monotonic() + 5
        while not (RESULTS / "entry-kernel-tls.json").exists():
            if time.monotonic() > deadline:
                raise RuntimeError("entry kernel TLS counters were not captured")
            time.sleep(0.05)
        if int(json.loads((RESULTS / "entry-kernel-tls.json").read_text()).get("TlsTxSw", 0)) <= 0:
            raise RuntimeError("AnyTLS ingress did not create a kernel TX connection")
    # The production Rust accounting reader checks exact byte totals, the
    # unsampled shutdown tail and stable journal identity across old -> new.
    with (RESULTS / "accounting.log").open("w") as log:
        checked = subprocess.run([str(INPUTS / "agent-tests"), "native_xray_restarts_keep_unsampled_tail_and_counter_identity", "--ignored", "--test-threads=1"],
                                 env={**os.environ, "BROCADE_TEST_XRAY": str(INPUTS / "xray"), "BROCADE_TEST_XRAY_OLD": str(INPUTS / "xray-old")},
                                 stdout=log, stderr=log, timeout=90)
    if checked.returncode:
        raise RuntimeError("exact cross-version accounting check failed; inspect accounting.log")
    records.append({"case": "accounting-old-to-new-exact-tail-and-identity", "passed": True, "bytes_verified": 4608})
    save(RESULTS / "evidence.json", records)
    save(RESULTS / "result.json", {"passed": True, "cases": len(records), "bytes_verified": sum(r["bytes_verified"] for r in records), "client_kernel": subprocess.check_output(["uname", "-r"], text=True).strip()})


if __name__ == "__main__":
    try:
        if sys.argv[1] == "client":
            exercise()
        else:
            serve(sys.argv[1])
    except Exception:
        (RESULTS / (sys.argv[1] + "-failure.txt")).write_text(traceback.format_exc())
        raise
