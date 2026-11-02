#!/usr/bin/env python3
"""Real HY2 TCP/UDP exchanges through inherited namespace-born UDP sockets."""
import hashlib
import ipaddress
import json
import os
import pathlib
import re
import select
import socket
import struct
import subprocess
import sys
import threading
import time

from workload import CLIENT, PUBLIC, at, check, receive_socket, run

SELF = str(pathlib.Path(__file__).resolve())


def exact(sock, count):
    result = bytearray()
    while len(result) < count:
        part = sock.recv(count - len(result))
        check(bool(part), "truncated stream")
        result.extend(part)
    return bytes(result)


def target():
    tcp = socket.socket(socket.AF_INET6, socket.SOCK_STREAM)
    tcp.bind(("::", 18080))
    tcp.listen(16)
    udp = socket.socket(socket.AF_INET6, socket.SOCK_DGRAM)
    udp.bind(("::", 18081))
    print("ready", flush=True)
    deadline = time.monotonic() + (600 if os.environ.get("BROCADE_HY2_TCG") else 180)
    while time.monotonic() < deadline:
        for sock in select.select([tcp, udp, sys.stdin], [], [], 1)[0]:
            if sock is sys.stdin:
                return
            if sock is udp:
                data, peer = udp.recvfrom(65535)
                udp.sendto(data, peer)
            else:
                accepted, _ = tcp.accept()
                with accepted:
                    accepted.settimeout(30)
                    while True:
                        prefix = accepted.recv(4)
                        if not prefix:
                            break
                        size = int.from_bytes(prefix + exact(accepted, 4 - len(prefix)), "big")
                        check(0 < size <= 1024 * 1024, "oversized echo request")
                        message = exact(accepted, size)
                        accepted.sendall(size.to_bytes(4, "big") + message)
    raise TimeoutError("echo target lifetime exceeded")


def address_wire(address, port):
    parsed = ipaddress.ip_address(address)
    return bytes([1 if parsed.version == 4 else 4]) + parsed.packed + struct.pack("!H", port)


def socks(command, address, port):
    sock = socket.create_connection(("127.0.0.1", 1080), timeout=15)
    sock.sendall(b"\x05\x01\x00")
    check(exact(sock, 2) == b"\x05\x00", "SOCKS negotiation failed")
    sock.sendall(b"\x05" + bytes([command, 0]) + address_wire(address, port))
    header = exact(sock, 4)
    check(header[:3] == b"\x05\x00\x00", f"SOCKS connect failed: {header!r}")
    size = {1: 4, 4: 16}[header[3]]
    bind_address = str(ipaddress.ip_address(exact(sock, size)))
    bind_port = int.from_bytes(exact(sock, 2), "big")
    return sock, bind_address, bind_port


def request(version, hopping=False):
    message = bytes(range(256)) * 1024
    sock, _, _ = socks(1, CLIENT[version], 18080)
    with sock:
        for i in range(4 if hopping else 1):
            if i:
                time.sleep(5.5)
            sock.sendall(len(message).to_bytes(4, "big") + message)
            size = int.from_bytes(exact(sock, 4), "big")
            check(size == len(message) and exact(sock, size) == message, "HY2 TCP payload corruption")
    control, _, relay_port = socks(3, "0.0.0.0", 0)
    with control, socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as udp:
        udp.bind(("127.0.0.1", 0))
        udp.settimeout(10)
        header = b"\x00\x00\x00" + address_wire(CLIENT[version], 18081)
        for i in range(8):
            payload = (b"hy2-netns-udp-" + bytes([i])).ljust(1000, b"x")
            udp.sendto(header + payload, ("127.0.0.1", relay_port))
            response, _ = udp.recvfrom(65535)
            check(response == header + payload, "HY2 UDP payload/header corruption")
    print(json.dumps({"tcp_bytes_each_direction": len(message) * (4 if hopping else 1), "udp_datagrams": 8,
                      "udp_bytes_each_direction": 8000, "sha256": hashlib.sha256(message).hexdigest()}))


def wait_ready(process, log):
    deadline = time.monotonic() + 10
    while time.monotonic() < deadline:
        if "BROCADE_NETNS_XRAY_READY" in log.read_text(errors="replace"):
            return
        check(process.poll() is None, "Xray startup failed; see " + str(log))
        time.sleep(.05)
    raise TimeoutError("Xray startup readiness")


def stop(process):
    if process.poll() is None:
        process.terminate()
        try:
            process.wait(timeout=5)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait(timeout=5)


def ct(pid, *args):
    command = ["conntrack", "-L", "-o", "extended", *args]
    if pid:
        command = ["nsenter", f"--net=/proc/{pid}/ns/net", "--", *command]
    completed = subprocess.run(command, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, timeout=5)
    check(completed.returncode == 0, f"conntrack query failed: {completed.stderr}")
    return completed.stdout.splitlines()


def exercise(client_pid, hy2_pid, output, main_namespace):
    binary = pathlib.Path(os.environ["BROCADE_LAB_XRAY"])
    inputs = binary.parent
    env = os.environ.copy()
    # Re-enable CT explicitly so isolation and main-namespace outbounds have positive
    # controls even on hosts where nothing else uses stateful netfilter rules.
    run("nft", "-f", "-", input="""table inet real_hy2_control {
 chain pre { type filter hook prerouting priority filter; ct state new,established counter; }
 chain out { type filter hook output priority filter; ct state new,established counter; }
}
""")
    sockets = [receive_socket(hy2_pid, family) for family in (socket.AF_INET, socket.AF_INET6)]
    processes, logs, cases = [], [], []
    auth = "hy2-netns-disposable-test"
    server_config = {"log": {"loglevel": "info"}, "inbounds": [],
                     "outbounds": [{"protocol": "freedom"}]}
    for version, sock in zip((4, 6), sockets):
        env[f"BROCADE_TEST_UDP{version}_FD"] = str(sock.fileno())
        server_config["inbounds"].append({"listen": "0.0.0.0" if version == 4 else "::", "port": 30000,
            "protocol": "hysteria", "settings": {"version": 2, "users": [{"auth": auth}]},
            "streamSettings": {"network": "hysteria", "security": "tls", "hysteriaSettings": {"version": 2},
                "tlsSettings": {"certificates": [{"certificateFile": str(inputs / "cover.pem"),
                                                   "keyFile": str(inputs / "cover.key")}]}}})
    config_path = output / "hy2-server.json"
    config_path.write_text(json.dumps(server_config))
    try:
        server_log = output / "hy2-server.log"
        log = server_log.open("wb")
        logs.append(log)
        server = subprocess.Popen([str(binary), str(config_path)], env=env,
                                  pass_fds=tuple(s.fileno() for s in sockets), stdout=log, stderr=subprocess.STDOUT)
        processes.append(server)
        wait_ready(server, server_log)
        for sock in sockets:
            sock.close()
        check(os.readlink(f"/proc/{server.pid}/ns/net") == main_namespace, "Xray moved into socket namespace")
        (output / "real-hy2-sockets.txt").write_text(at(hy2_pid, "ss", "-ulnp") + "\n")
        target_process = subprocess.Popen(["nsenter", f"--net=/proc/{client_pid}/ns/net", "--",
                                           sys.executable, SELF, "--target"], stdin=subprocess.PIPE,
                                          stdout=subprocess.PIPE, text=True)
        processes.append(target_process)
        check(bool(select.select([target_process.stdout], [], [], 5)[0]) and
              target_process.stdout.readline().strip() == "ready", "HY2 echo target startup failed")
        for version in (4, 6):
            for port in (30000, 30037, 30099, None):
                hopping = port is None
                label = f"hy2-ipv{version}-" + ("live-port-hopping" if hopping else f"port-{port}")
                client_config = {"log": {"loglevel": "info"},
                    "inbounds": [{"listen": "127.0.0.1", "port": 1080, "protocol": "socks", "settings": {"udp": True}}],
                    "outbounds": [{"protocol": "hysteria", "settings": {"version": 2, "address": PUBLIC[version], "port": port or 30000},
                        "streamSettings": {"network": "hysteria", "security": "tls", "hysteriaSettings": {"version": 2, "auth": auth},
                            "tlsSettings": {"serverName": "cover.test", "certificates": [{"usage": "verify", "certificateFile": str(inputs / "cover.pem")}]}}}]}
                if hopping:
                    client_config["outbounds"][0]["streamSettings"]["finalMask"] = {
                        "quicParams": {"udpHop": {"ports": "30000-30099", "interval": 5}}}
                family = "ipv4" if version == 4 else "ipv6"
                selector = ("-f", family, "-p", "udp", "--orig-src", CLIENT[version],
                            "--orig-dst", PUBLIC[version])
                if not hopping:
                    selector += ("--dport", str(port))
                def port_pairs(rows):
                    return {tuple(map(int, re.search(r"sport=(\d+) dport=(\d+)", row).groups())) for row in rows}
                baseline = port_pairs(ct(hy2_pid, *selector))
                path = output / (label + ".json")
                path.write_text(json.dumps(client_config))
                log_path = output / (label + ".log")
                client_log = log_path.open("wb")
                try:
                    client = subprocess.Popen(["nsenter", f"--net=/proc/{client_pid}/ns/net", "--", str(binary), str(path)],
                                              stdout=client_log, stderr=subprocess.STDOUT)
                    processes.append(client)
                    wait_ready(client, log_path)
                    exchange = json.loads(at(client_pid, sys.executable, SELF, "--request", version,
                                             int(hopping), timeout=40))
                    host_flows = {p for p in port_pairs(ct(None, *selector)) if 30000 <= p[1] <= 30099}
                    new_flows = port_pairs(ct(hy2_pid, *selector)) - baseline
                    check(not host_flows, "real HY2 outer UDP tracked in main namespace")
                    check(bool(new_flows), "real HY2 outer UDP missing in socket namespace")
                    if hopping:
                        ports = sorted({p[1] for p in new_flows})
                        check(len(ports) >= 2 and all(30000 <= p <= 30099 for p in ports), "no live destination-port hopping observed")
                        exchange["observed_hop_ports"] = ports
                    outgoing = ("-f", family, "-p", "tcp", "--orig-src", PUBLIC[version],
                                "--orig-dst", CLIENT[version], "--dport", "18080")
                    check(bool(ct(None, *outgoing)), "Xray TCP outbound not created in main namespace")
                    check(not ct(hy2_pid, "-f", family, "-p", "tcp"), "Xray TCP outbound leaked into HY2 namespace")
                    check(os.readlink(f"/proc/{server.pid}/ns/net") == main_namespace, "Xray namespace changed")
                    cases.append({"case": label, "host_hy2_udp_ct": 0, "socket_namespace_hy2_udp_ct": True,
                                  "tcp_outbound_namespace": "main", **exchange})
                finally:
                    if "client" in locals():
                        stop(client)
                    client_log.close()
        (output / "real-hy2-host-conntrack.txt").write_text("\n".join(ct(None)) + "\n")
        (output / "real-hy2-socket-conntrack.txt").write_text("\n".join(ct(hy2_pid)) + "\n")
        return cases
    finally:
        for process in reversed(processes):
            stop(process)
        for sock in sockets:
            sock.close()
        for log in logs:
            log.close()


if __name__ == "__main__":
    if sys.argv[1:] == ["--target"]:
        target()
    elif len(sys.argv) == 4 and sys.argv[1] == "--request":
        request(int(sys.argv[2]), bool(int(sys.argv[3])))
    else:
        raise RuntimeError("expected --target or --request VERSION")
