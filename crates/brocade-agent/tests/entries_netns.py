#!/usr/bin/env python3
"""End-to-end entry data path in private user/mount/network namespaces.

Usage: python3 crates/brocade-agent/tests/entries_netns.py TEST_BINARY

Topology (all private): client -- entry machine == WireGuard underlay == container
host -- "cube-dev" -- guest. The entry machine and the host each run one Agent
convergence of a fixture desired state. Nothing on the real host is modified.
"""
import json
import os
import socket
import subprocess
import sys
import tempfile
import threading
import time

BINARY = None
WORK = None


def sh(*args, ns=None, check=True, timeout=20, capture=True):
    command = (["ip", "netns", "exec", ns] if ns else []) + list(args)
    result = subprocess.run(command, check=False, timeout=timeout,
                            capture_output=capture, text=True)
    if check and result.returncode != 0:
        raise RuntimeError(f"{' '.join(command)} failed: {result.stderr.strip()}")
    return result


def converge(ns, desired, root):
    path = os.path.join(WORK, f"{ns}-desired.json")
    with open(path, "w") as handle:
        json.dump(desired, handle)
    env = dict(os.environ, BROCADE_ENTRY_FIXTURE=path, BROCADE_ENTRY_ROOT=root)
    result = subprocess.run(
        ["ip", "netns", "exec", ns, BINARY, "--ignored", "--exact",
         "cvm::entries::tests::converge_fixture", "--nocapture", "--test-threads", "1"],
        env=env, capture_output=True, text=True, timeout=120)
    for line in result.stdout.splitlines():
        if "ENTRY-REPORT " in line:
            return json.loads(line.split("ENTRY-REPORT ", 1)[1])
    raise RuntimeError(f"no report from {ns}: {result.stdout}\n{result.stderr}")


def keypair():
    private = sh("wg", "genkey").stdout.strip()
    public = subprocess.run(["wg", "pubkey"], input=private, capture_output=True,
                            text=True, check=True).stdout.strip()
    return private, public


GUEST = r"""
import socket, threading, sys
def tcp(address, port):
    server = socket.socket(); server.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    server.bind((address, port)); server.listen(16)
    while True:
        connection, peer = server.accept()
        connection.sendall(f"{peer[0]} {address}:{port}".encode()); connection.close()
def udp(address, port):
    server = socket.socket(socket.AF_INET, socket.SOCK_DGRAM); server.bind((address, port))
    while True:
        data, peer = server.recvfrom(2048); server.sendto(data + f" {peer[0]}".encode(), peer)
for target in (lambda: tcp("192.168.0.7", 8022), lambda: udp("192.168.0.7", 8022),
               lambda: tcp("192.168.0.9", 80), lambda: tcp("192.168.0.8", 8022)):
    threading.Thread(target=target, daemon=True).start()
print("ready", flush=True)
threading.Event().wait()
"""


def tcp(ns, address, port, timeout=3):
    code = (f"import socket;s=socket.create_connection(('{address}',{port}),timeout={timeout});"
            "print(s.recv(200).decode())")
    result = sh("python3", "-c", code, ns=ns, check=False, timeout=timeout + 5)
    return result.stdout.strip() if result.returncode == 0 else None


def udp(ns, address, port):
    code = (f"import socket;s=socket.socket(socket.AF_INET,socket.SOCK_DGRAM);s.settimeout(3);"
            f"s.sendto(b'probe',('{address}',{port}));print(s.recv(200).decode())")
    result = sh("python3", "-c", code, ns=ns, check=False, timeout=10)
    return result.stdout.strip() if result.returncode == 0 else None


def tunnel(slot, side, entry, target, key, peer, endpoint):
    return {"id": f"entry-tunnel-{slot}", "side": side, "entry_node": entry,
            "target_node": target,
            "offer": {"listen_side": "provider", "kind": "wireguard", "endpoint": endpoint,
                      "port_start": 40000, "port_end": 40010, "mtu": 1420},
            "port": 40000 + slot, "slot": slot, "credential": key, "peer_credential": peer}


def main():
    global BINARY, WORK
    BINARY = os.path.abspath(sys.argv[1])
    WORK = tempfile.mkdtemp(prefix="entries-netns-")
    sh("mount", "-t", "tmpfs", "tmpfs", "/run")
    os.makedirs("/run/netns", exist_ok=True)
    for ns in ("client", "edge", "host", "host2", "guest", "guest2"):
        sh("ip", "netns", "add", ns)
        sh("ip", "link", "set", "lo", "up", ns=ns)
    def veth(a, a_name, a_addr, b, b_name, b_addr):
        sh("ip", "link", "add", a_name, "netns", a, "type", "veth", "peer", "name", b_name, "netns", b)
        for ns, name, address in ((a, a_name, a_addr), (b, b_name, b_addr)):
            sh("ip", "address", "add", address, "dev", name, ns=ns)
            sh("ip", "link", "set", name, "up", ns=ns)
    veth("client", "c0", "203.0.113.100/24", "edge", "up0", "203.0.113.1/24")
    for address in ("203.0.113.12/32", "203.0.113.30/32"):
        sh("ip", "address", "add", address, "dev", "lo", ns="edge")
    veth("edge", "u1", "198.51.100.1/24", "host", "u1", "198.51.100.2/24")
    # The second host is also an entry machine for its own container.
    veth("client", "c1", "203.0.114.100/24", "host2", "up0", "203.0.114.1/24")
    sh("ip", "address", "add", "203.0.114.20/32", "dev", "lo", ns="host2")
    veth("edge", "u2", "198.51.101.1/24", "host2", "u2", "198.51.101.2/24")
    # Both hosts use the same sandbox network; the guests even share an address.
    veth("host", "cube-dev", "192.168.0.1/24", "guest", "eth0", "192.168.0.7/24")
    sh("ip", "address", "add", "192.168.0.9/24", "dev", "eth0", ns="guest")
    veth("host2", "cube-dev", "192.168.0.1/24", "guest2", "eth0", "192.168.0.7/24")
    sh("ip", "address", "add", "192.168.0.8/24", "dev", "eth0", ns="guest2")
    for ns in ("guest", "guest2"):
        sh("ip", "route", "add", "default", "via", "192.168.0.1", ns=ns)
    for ns in ("edge", "host", "host2"):
        sh("sysctl", "-qw", "net.ipv4.ip_forward=1", ns=ns)
    guests = [subprocess.Popen(["ip", "netns", "exec", ns, "python3", "-u", "-c", GUEST],
                               stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True)
              for ns in ("guest", "guest2")]
    try:
        for guest in guests:
            assert guest.stdout.readline().strip() == "ready"
        scenario()
    except BaseException:
        diagnose()
        raise
    finally:
        for guest in guests:
            guest.kill()
            guest.wait()


def diagnose():
    for ns in ("edge", "host", "host2"):
        for command in (("nft", "list", "ruleset"), ("wg", "show"), ("ip", "-4", "rule", "show"),
                        ("ip", "-4", "route", "show", "table", "all"), ("ip", "-4", "address", "show"),
                        ("conntrack", "-L")):
            result = sh(*command, ns=ns, check=False)
            print(f"--- {ns}: {' '.join(command)}\n{result.stdout}{result.stderr}", file=sys.stderr)


def scenario():
    edge_key, edge_public = keypair()
    host_key, host_public = keypair()
    host2_key, host2_public = keypair()
    l4 = {"kind": "l4", "protocol": "tcp_udp", "listen_port": 30022, "target_port": 8022}
    edge = {
        "node_id": "edge", "generation": 1,
        "rules": [
            {"id": "ssh", "address": "203.0.113.12", "mode": l4, "target": "192.168.0.7",
             "tunnel": "entry-tunnel-3"},
            {"id": "web", "address": "203.0.113.30", "mode": {"kind": "l3"},
             "target": "192.168.0.9", "tunnel": "entry-tunnel-3"},
            # Same container address, other host: only the tunnel tells them apart.
            {"id": "other", "address": "203.0.113.12",
             "mode": {"kind": "l4", "protocol": "tcp", "listen_port": 30023, "target_port": 8022},
             "target": "192.168.0.7", "tunnel": "entry-tunnel-4"},
        ],
        "deliveries": [],
        "tunnels": [
            tunnel(3, "entry", "edge", "host", edge_key, host_public, "198.51.100.1"),
            tunnel(4, "entry", "edge", "host2", edge_key, host2_public, "198.51.101.1"),
        ],
    }
    host = {
        "node_id": "host", "generation": 1, "rules": [],
        "deliveries": [
            {"id": "ssh", "instance_id": "cvm-web", "mode": l4, "target": "192.168.0.7",
             "tunnel": "entry-tunnel-3"},
            {"id": "web", "instance_id": "cvm-web", "mode": {"kind": "l3"},
             "target": "192.168.0.9", "tunnel": "entry-tunnel-3"},
        ],
        "tunnels": [tunnel(3, "target", "edge", "host", host_key, edge_public, "198.51.100.1")],
    }
    host2 = {
        "node_id": "host2", "generation": 1, "rules": [],
        "deliveries": [
            {"id": "other", "instance_id": "cvm-api",
             "mode": {"kind": "l4", "protocol": "tcp", "listen_port": 30023, "target_port": 8022},
             "target": "192.168.0.7", "tunnel": "entry-tunnel-4"},
        ],
        "tunnels": [tunnel(4, "target", "edge", "host2", host2_key, edge_public, "198.51.101.1")],
    }
    roots = {ns: tempfile.mkdtemp(prefix=f"{ns}-", dir=WORK) for ns in ("edge", "host", "host2")}
    for ns, desired in (("edge", edge), ("host", host), ("host2", host2)):
        report = converge(ns, desired, roots[ns])
        assert report["error"] is None, (ns, report)
    # The host side dials; give WireGuard a moment to complete its handshake.
    deadline = time.monotonic() + 20
    reply = None
    while time.monotonic() < deadline and reply is None:
        reply = tcp("client", "203.0.113.12", 30022, timeout=2)
    assert reply == "192.168.0.1 192.168.0.7:8022", reply
    assert udp("client", "203.0.113.12", 30022) == "probe 192.168.0.1"
    assert tcp("client", "203.0.113.30", 80) == "192.168.0.1 192.168.0.9:80"
    sh("ping", "-c", "1", "-W", "3", "203.0.113.30", ns="client")
    # The other host's guest has the same address but answers with its own listener set.
    reply = None
    deadline = time.monotonic() + 20
    while time.monotonic() < deadline and reply is None:
        reply = tcp("client", "203.0.113.12", 30023, timeout=2)
    assert reply == "192.168.0.1 192.168.0.7:8022", reply
    assert tcp("client", "203.0.113.12", 30024, timeout=2) is None
    # Reports prove connected tunnels and configured items.
    report = converge("edge", edge, roots["edge"])
    assert all(t["connected"] for t in report["tunnels"]), report
    assert all(r["configured"] for r in report["rules"]), report
    # A rule without a target keeps the port fenced instead of reaching the machine.
    stopped = json.loads(json.dumps(edge))
    stopped["generation"] = 2
    stopped["rules"][0]["target"] = None
    assert converge("edge", stopped, roots["edge"])["error"] is None
    assert tcp("client", "203.0.113.12", 30022, timeout=2) is None
    # Re-pointing keeps the address: the same entry now reaches the second host.
    moved = json.loads(json.dumps(edge))
    moved["generation"] = 3
    moved["rules"][0]["tunnel"] = "entry-tunnel-4"
    moved["rules"][0]["mode"] = {"kind": "l4", "protocol": "tcp", "listen_port": 30022,
                                 "target_port": 8022}
    moved["rules"][0]["target"] = "192.168.0.8"
    host2_moved = json.loads(json.dumps(host2))
    host2_moved["generation"] = 2
    host2_moved["deliveries"].append({"id": "ssh", "instance_id": "cvm-api",
                                      "mode": moved["rules"][0]["mode"],
                                      "target": "192.168.0.8", "tunnel": "entry-tunnel-4"})
    assert converge("host2", host2_moved, roots["host2"])["error"] is None
    assert converge("edge", moved, roots["edge"])["error"] is None
    assert tcp("client", "203.0.113.12", 30022) == "192.168.0.1 192.168.0.8:8022"
    # Same-machine delivery: no tunnel, straight to the primary address via cube-dev.
    local = json.loads(json.dumps(host2_moved))
    local["generation"] = 3
    local["rules"] = [{"id": "local", "address": "203.0.114.20",
                       "mode": {"kind": "l4", "protocol": "tcp", "listen_port": 30080,
                                "target_port": 8022},
                       "target": "192.168.0.8", "tunnel": None}]
    assert converge("host2", local, roots["host2"])["error"] is None
    assert tcp("client", "203.0.114.20", 30080) == "192.168.0.1 192.168.0.8:8022"
    assert tcp("client", "203.0.113.12", 30022) == "192.168.0.1 192.168.0.8:8022"
    # Withdrawal removes the table contents, tunnel devices and policy rules.
    for ns in ("edge", "host", "host2"):
        empty = {"node_id": ns, "generation": 9, "rules": [], "deliveries": [], "tunnels": []}
        assert converge(ns, empty, roots[ns])["error"] is None
        links = json.loads(sh("ip", "-j", "link", "show", ns=ns).stdout)
        assert not any(str(l.get("ifalias", "")).startswith("brocade-entry:") for l in links), ns
        rules = json.loads(sh("ip", "-4", "-j", "rule", "show", ns=ns).stdout)
        assert not any(r.get("priority") == 31700 for r in rules), ns
    assert tcp("client", "203.0.113.12", 30023, timeout=2) is None
    print("entries netns: L4 TCP/UDP, L3 TCP/ICMP, repeated container addresses across hosts, "
          "fence without target, re-point across hosts, same-machine delivery and full withdrawal passed")


if __name__ == "__main__":
    if os.environ.get("BROCADE_ENTRY_NETNS_CHILD") != "1":
        os.execvpe("unshare", ["unshare", "--user", "--map-root-user", "--net", "--mount",
                               "--fork", sys.executable, os.path.abspath(__file__)] + sys.argv[1:],
                   dict(os.environ, BROCADE_ENTRY_NETNS_CHILD="1"))
    main()
