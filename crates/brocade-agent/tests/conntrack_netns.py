#!/usr/bin/env python3
"""Run Agent NOTRACK checks in private user/network namespaces with a veth peer.

Usage: python3 crates/brocade-agent/tests/conntrack_netns.py TEST_BINARY
No host routes, rules, sysctls, or interfaces are modified.
"""
import os
import select
import selectors
import socket
import subprocess
import sys
import time
from pathlib import Path


def run(*args):
    subprocess.run(args, check=True, timeout=5)


def ancestor_fds():
    return tuple(int(os.environ[f"BROCADE_TEST_ANCESTOR_{name.upper()}_FD"])
                 for name in ("user", "net"))


def guard():
    for name, fd in zip(("user", "net"), ancestor_fds()):
        ancestor = os.readlink(f"/proc/self/fd/{fd}")
        if not ancestor.startswith(name + ":["):
            raise RuntimeError("invalid ancestor namespace descriptor")
        if os.readlink(f"/proc/self/ns/{name}") == ancestor:
            raise RuntimeError("requires private user and network namespaces")


def peer():
    guard()
    if sys.stdin.readline().strip() != "configured":
        raise RuntimeError("peer network was not configured")
    selector = selectors.DefaultSelector()
    sockets = []
    for family, address, port in (
        (socket.AF_INET, "192.0.2.17", 20001),
        (socket.AF_INET, "192.0.2.17", 20002),
        (socket.AF_INET, "192.0.2.18", 20001),
        (socket.AF_INET6, "2001:db8:1::17", 20003),
        (socket.AF_INET6, "2001:db8:1::17", 20004),
    ):
        listener = socket.socket(family, socket.SOCK_STREAM)
        listener.bind((address, port))
        listener.listen(16)
        selector.register(listener, selectors.EVENT_READ, "tcp")
        sockets.append(listener)
    udp = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    udp.bind(("192.0.2.17", 20001))
    selector.register(udp, selectors.EVENT_READ, "udp")
    selector.register(sys.stdin, selectors.EVENT_READ, "control")
    sockets.append(udp)
    print("ready", flush=True)
    deadline = time.monotonic() + 50
    try:
        while time.monotonic() < deadline:
            for key, _ in selector.select(timeout=1):
                if key.data == "control":
                    return
                if key.data == "tcp":
                    if len(sockets) >= 32:
                        raise RuntimeError("test connection bound exceeded")
                    accepted, _ = key.fileobj.accept()
                    sockets.append(accepted)
                else:
                    data, address = key.fileobj.recvfrom(32)
                    key.fileobj.sendto(data, address)
        raise TimeoutError("peer lifetime exceeded")
    finally:
        for sock in sockets:
            sock.close()
        selector.close()


def tracking_query(args):
    # Reuse the repository's strict bounded CT_GET reader. This also works on kernels
    # without CONFIG_NF_CONNTRACK_PROCFS; errors never become an empty-success result.
    sys.dont_write_bytecode = True
    import importlib.util
    import ipaddress
    import struct
    helper = Path(__file__).resolve().parents[3] / "scripts/tests/xray-manual-handoff/wan-conntrack.py"
    spec = importlib.util.spec_from_file_location("ct_dump", helper)
    ct = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(ct)
    protocol, source, source_port, destination, destination_port = args
    source, destination = ipaddress.ip_address(source), ipaddress.ip_address(destination)
    if source.version != destination.version:
        raise ValueError("address families differ")
    protocol_number = {"tcp": 6, "udp": 17}[protocol]
    family = socket.AF_INET if source.version == 4 else socket.AF_INET6
    def tuple_fields(data):
        fields = ct.attributes(data)
        addresses = ct.attributes(fields[1])
        proto = ct.attributes(fields[2])
        if ct.number(proto[1], 1) != protocol_number:
            return None
        src, dst = (1, 2) if source.version == 4 else (3, 4)
        return {"source": str(ipaddress.ip_address(addresses[src])),
                "destination": str(ipaddress.ip_address(addresses[dst])),
                "source_port": ct.number(proto[2], 2),
                "destination_port": ct.number(proto[3], 2)}
    def entry(payload, **scope):
        if len(payload) < 4:
            raise ValueError("short nfgenmsg")
        observed_family, version, _ = struct.unpack_from("!BBH", payload)
        if observed_family != family:
            return None
        if version:
            raise ValueError("unsupported nfgenmsg version")
        fields = ct.attributes(payload[4:])
        original = tuple_fields(fields[1])
        if original is None or original != {"source": str(source), "destination": str(destination),
                                           "source_port": int(source_port), "destination_port": int(destination_port)}:
            return None
        return {"original": original, "reply": tuple_fields(fields[2]),
                "zone": ct.number(fields[18], 2) if 18 in fields else 0}
    ct.entry = entry
    with socket.socket(socket.AF_NETLINK, socket.SOCK_RAW, 12) as sock:
        sock.bind((0, 0))
        port = sock.getsockname()[0]
        sequence = ct.secrets.randbelow(0xffffffff) + 1
        body = struct.pack("!BBH", family, 0, 0)
        request = ct.HEADER.pack(ct.HEADER.size + 4, ct.CT_GET, ct.NLM_F_REQUEST_DUMP, sequence, port) + body
        if sock.sendto(request, (0, 0)) != len(request):
            raise RuntimeError("short read-only conntrack request")
        result = ct.collect(sock, sequence, port, {}, 4, 50000)
    print("present" if result["matched"] else "absent")
    return 0


def main():
    if sys.argv[1:2] == ["--tracked"]:
        return tracking_query(sys.argv[2:])
    if sys.argv[1:] == ["--peer"]:
        peer()
        return 0
    if len(sys.argv) == 2:
        # Keep real namespace descriptors across unshare. A child user namespace cannot
        # necessarily read /proc/1/ns, and an environment string is not namespace evidence.
        fds = tuple(os.open(f"/proc/self/ns/{name}", os.O_RDONLY) for name in ("user", "net"))
        try:
            env = os.environ.copy()
            for name, fd in zip(("user", "net"), fds):
                env[f"BROCADE_TEST_ANCESTOR_{name.upper()}_FD"] = str(fd)
            return subprocess.run(
                ["unshare", "-Urn", "--", sys.executable, str(Path(__file__).resolve()),
                 "--inside", sys.argv[1]],
                env=env, pass_fds=fds, timeout=50,
            ).returncode
        finally:
            for fd in fds:
                os.close(fd)
    if len(sys.argv) != 3 or sys.argv[1] != "--inside":
        raise RuntimeError("expected the compiled Agent test binary")
    guard()
    if [name for _, name in socket.if_nameindex()] != ["lo"]:
        raise RuntimeError("test namespace must initially contain only loopback")
    test_binary = str(Path(sys.argv[2]).resolve(strict=True))
    peer_process = subprocess.Popen(
        ["unshare", "--net", "--", sys.executable, str(Path(__file__).resolve()), "--peer"],
        stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True, pass_fds=ancestor_fds(),
    )
    try:
        deadline = time.monotonic() + 3
        while os.readlink(f"/proc/{peer_process.pid}/ns/net") == os.readlink("/proc/self/ns/net"):
            if peer_process.poll() is not None or time.monotonic() >= deadline:
                raise RuntimeError("peer did not enter its private namespace")
            time.sleep(0.01)
        run("ip", "link", "set", "lo", "up")
        run("ip", "link", "add", "ctlocal", "type", "veth", "peer", "name", "ctpeer")
        run("ip", "link", "set", "ctpeer", "netns", str(peer_process.pid))
        run("ip", "addr", "add", "192.0.2.1/24", "dev", "ctlocal")
        run("ip", "-6", "addr", "add", "2001:db8:1::1/64", "dev", "ctlocal", "nodad")
        run("ip", "link", "set", "ctlocal", "up")
        prefix = ("nsenter", "--target", str(peer_process.pid), "--net", "--", "ip")
        run(*prefix, "link", "set", "lo", "up")
        for address in ("192.0.2.17/24", "192.0.2.18/24"):
            run(*prefix, "addr", "add", address, "dev", "ctpeer")
        run(*prefix, "-6", "addr", "add", "2001:db8:1::17/64", "dev", "ctpeer", "nodad")
        run(*prefix, "link", "set", "ctpeer", "up")
        peer_process.stdin.write("configured\n")
        peer_process.stdin.flush()
        if not select.select([peer_process.stdout], [], [], 5)[0] or peer_process.stdout.readline().strip() != "ready":
            raise RuntimeError("peer listeners did not become ready")
        env = os.environ.copy()
        env["BROCADE_TEST_CONNTRACK_PEER"] = "ready"
        env["BROCADE_TEST_CONNTRACK_FIXTURE"] = str(Path(__file__).resolve())
        return subprocess.run(
            [test_binary, "--exact", "conntrack::tests::installed_health_mux_policy_in_private_namespace",
             "--ignored", "--nocapture", "--test-threads=1"],
            env=env, pass_fds=ancestor_fds(), timeout=40,
        ).returncode
    finally:
        peer_process.stdin.close()
        try:
            peer_process.wait(timeout=3)
        except subprocess.TimeoutExpired:
            peer_process.kill()
            peer_process.wait(timeout=3)
        peer_process.stdout.close()


if __name__ == "__main__":
    sys.exit(main())
