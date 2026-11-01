#!/usr/bin/env python3
"""Disposable dual-stack port-range/conntrack/SCM_RIGHTS experiment.

The lab host, client and HY2 socket each have a private network namespace.
The host process receives a UDP FD born in the HY2 namespace and never enters it.
This is a transport prototype, not an Agent or Xray production implementation.
"""
import array
import concurrent.futures
import datetime
import json
import os
import pathlib
import select
import socket
import subprocess
import sys
import threading
import time
import traceback


SELF = str(pathlib.Path(__file__).resolve())
PUBLIC = {4: "198.18.0.1", 6: "2001:db8:100::1"}
CLIENT = {4: "198.18.0.2", 6: "2001:db8:100::2"}


def run(*args, input=None, timeout=15):
    try:
        return subprocess.check_output([str(a) for a in args], input=input, text=True,
                                       stderr=subprocess.STDOUT, timeout=timeout).strip()
    except subprocess.CalledProcessError as error:
        raise RuntimeError(f"{args}: {error.output}") from error


def at(pid, *args, **kwargs):
    return run("nsenter", f"--net=/proc/{pid}/ns/net", "--", *args, **kwargs)


def stamp():
    return datetime.datetime.now(datetime.timezone.utc).isoformat()


def check(condition, detail):
    if not condition:
        raise AssertionError(detail)


def hold_namespace():
    print(os.readlink("/proc/self/ns/net"), flush=True)
    sys.stdin.read(1)


def namespace():
    child = subprocess.Popen(["unshare", "--net", "--", sys.executable, SELF, "--hold"],
                             stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True)
    if not select.select([child.stdout], [], [], 5)[0]:
        child.kill()
        raise TimeoutError("network namespace startup")
    observed = child.stdout.readline().strip()
    check(observed.startswith("net:[") and observed != os.readlink("/proc/self/ns/net"),
          "namespace isolation did not occur")
    return child


def pass_socket(fd, family):
    channel = socket.socket(fileno=fd)
    udp = socket.socket(family, socket.SOCK_DGRAM)
    if family == socket.AF_INET6:
        udp.setsockopt(socket.IPPROTO_IPV6, socket.IPV6_V6ONLY, 1)
    udp.bind(("0.0.0.0" if family == socket.AF_INET else "::", 30000))
    channel.sendmsg([os.readlink("/proc/self/ns/net").encode()],
                    [(socket.SOL_SOCKET, socket.SCM_RIGHTS, array.array("i", [udp.fileno()]))])
    udp.close()
    channel.close()


def receive_socket(pid, family):
    parent, child = socket.socketpair()
    try:
        subprocess.run(["nsenter", f"--net=/proc/{pid}/ns/net", "--", sys.executable,
                        SELF, "--socket", str(child.fileno()), str(family)],
                       pass_fds=(child.fileno(),), check=True, timeout=5)
        parent.settimeout(5)
        payload, ancillary, flags, _ = parent.recvmsg(128, socket.CMSG_SPACE(4))
        check(not flags, "truncated SCM_RIGHTS response")
        descriptors = array.array("i")
        for level, kind, data in ancillary:
            check((level, kind) == (socket.SOL_SOCKET, socket.SCM_RIGHTS), "unexpected control data")
            descriptors.frombytes(data)
        check(len(descriptors) == 1, "expected exactly one socket descriptor")
        check(payload.decode() == os.readlink(f"/proc/{pid}/ns/net"), "wrong socket birth namespace")
        return socket.socket(fileno=descriptors[0])
    finally:
        parent.close()
        child.close()


def probe(version, port, source_port, protocol, expected=True, payload_size=128):
    family = socket.AF_INET if version == 4 else socket.AF_INET6
    kind = socket.SOCK_DGRAM if protocol == "udp" else socket.SOCK_STREAM
    with socket.socket(family, kind) as sock:
        sock.settimeout(3 if expected else .25)
        sock.bind((CLIENT[version], source_port))
        message = json.dumps({"family": version, "source_port": source_port, "port": port,
                              "protocol": protocol}).encode().ljust(payload_size, b" ")
        if protocol == "tcp":
            sock.connect((PUBLIC[version], port))
            sock.sendall(message)
            reply = sock.recv(4096)
        else:
            sock.sendto(message, (PUBLIC[version], port))
            try:
                reply, peer = sock.recvfrom(65535)
            except (TimeoutError, ConnectionRefusedError):
                check(not expected, "expected UDP response was missing")
                return {"no_reply": True}
            check(peer[:2] == (PUBLIC[version], port), f"reply source port/address changed: {peer}")
        check(expected, "out-of-range UDP unexpectedly reached a listener")
        result = json.loads(reply)
        check(result["peer"][:2] == [CLIENT[version], source_port], "client source address/port changed")
        check(result["received_bytes"] == len(message), "truncated UDP payload")
        return result


def tracked(pid, protocol, version, source_port, dest_port):
    prefix = ["nsenter", f"--net=/proc/{pid}/ns/net", "--"] if pid else []
    command = prefix + ["conntrack", "-L", "-f", "ipv4" if version == 4 else "ipv6", "-p", protocol,
                        "--orig-src", CLIENT[version], "--orig-dst", PUBLIC[version],
                        "--sport", str(source_port), "--dport", str(dest_port), "-o", "extended"]
    completed = subprocess.run(command, text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=5)
    check(completed.returncode == 0, f"conntrack query failed: {completed.stderr}")
    lines = completed.stdout.splitlines()
    check(all(protocol in line for line in lines), "unexpected conntrack output")
    return lines


def main_lab(output):
    start, started = time.monotonic(), stamp()
    output.mkdir(parents=True, exist_ok=True)
    ancestor = int(os.environ["BROCADE_LAB_ANCESTOR_NET_FD"])
    check(os.readlink(f"/proc/self/fd/{ancestor}") != os.readlink("/proc/self/ns/net"),
          "refuse to operate in caller network namespace")
    check([name for _, name in socket.if_nameindex()] == ["lo"], "lab must start with only loopback")
    children, listeners, cases, errors = [], [], [], []
    stop = threading.Event()
    records = []
    root_ns = os.readlink("/proc/self/ns/net")
    result = {"started": started, "passed": False, "kernel": run("uname", "-r"), "cases": cases}
    try:
        client, hy2 = namespace(), namespace()
        children.extend((client, hy2))
        run("ip", "link", "set", "lo", "up")
        for local, remote, child, local4, remote4, local6, remote6 in (
            ("wan", "client", client, "198.18.0.1/24", "198.18.0.2/24", "2001:db8:100::1/64", "2001:db8:100::2/64"),
            ("hyhost", "hy2", hy2, "169.254.200.1/30", "169.254.200.2/30", "fd00:200::1/64", "fd00:200::2/64"),
        ):
            run("ip", "link", "add", local, "type", "veth", "peer", "name", remote)
            run("ip", "link", "set", remote, "netns", child.pid)
            run("ip", "addr", "add", local4, "dev", local)
            run("ip", "-6", "addr", "add", local6, "dev", local, "nodad")
            run("ip", "link", "set", local, "up")
            at(child.pid, "ip", "link", "set", "lo", "up")
            at(child.pid, "ip", "addr", "add", remote4, "dev", remote)
            at(child.pid, "ip", "-6", "addr", "add", remote6, "dev", remote, "nodad")
            at(child.pid, "ip", "link", "set", remote, "up")
        at(hy2.pid, "ip", "route", "add", "default", "via", "169.254.200.1")
        at(hy2.pid, "ip", "-6", "route", "add", "default", "via", "fd00:200::1")
        mac_hy = json.loads(at(hy2.pid, "ip", "-j", "link", "show", "hy2"))[0]["address"]
        mac_client = json.loads(at(client.pid, "ip", "-j", "link", "show", "client"))[0]["address"]
        mac_wan = json.loads(run("ip", "-j", "link", "show", "wan"))[0]["address"]
        mac_host = json.loads(run("ip", "-j", "link", "show", "hyhost"))[0]["address"]
        # Avoid ARP/NDP timing influencing the first packet. These neighbors are all
        # disposable point-to-point links, not an assumption about production gateways.
        for address, mac, dev, pid in (
            (PUBLIC[4], mac_wan, "client", client.pid), (PUBLIC[6], mac_wan, "client", client.pid),
            ("169.254.200.1", mac_host, "hy2", hy2.pid), ("fd00:200::1", mac_host, "hy2", hy2.pid),
            (CLIENT[4], mac_client, "wan", None), (CLIENT[6], mac_client, "wan", None),
        ):
            command = ("ip", "neigh", "replace", address, "lladdr", mac, "nud", "permanent", "dev", dev)
            at(pid, *command) if pid else run(*command)
        # Activate host CT with a real rule. Positive control traffic must be tracked.
        run("nft", "-f", "-", input="""table inet host_control {
 chain pre { type filter hook prerouting priority filter; ct state new,established counter; }
 chain out { type filter hook output priority filter; ct state new,established counter; }
}
""")
        at(hy2.pid, "nft", "-f", "-", input="""table inet hy2_redirect {
 chain pre { type nat hook prerouting priority dstnat; udp dport 30000-30099 counter redirect to :30000; }
}
""")
        for dev in ("wan", "hyhost"):
            run("tc", "qdisc", "add", "dev", dev, "clsact")
        for eth, public in (("ip", PUBLIC[4]), ("ipv6", PUBLIC[6])):
            priority = "10" if eth == "ip" else "11"
            run("tc", "filter", "add", "dev", "wan", "ingress", "protocol", eth, "pref", priority, "flower",
                "skip_hw", "dst_ip", public, "ip_proto", "udp", "dst_port", "30000-30099",
                "action", "skbmod", "set", "dmac", mac_hy,
                "action", "mirred", "egress", "redirect", "dev", "hyhost")
            run("tc", "filter", "add", "dev", "hyhost", "ingress", "protocol", eth, "pref", priority, "flower",
                "skip_hw", "dst_ip", CLIENT[4 if eth == "ip" else 6], "ip_proto", "udp",
                "action", "skbmod", "set", "dmac", mac_client,
                "action", "mirred", "egress", "redirect", "dev", "wan")
        listeners.extend(receive_socket(hy2.pid, family) for family in (socket.AF_INET, socket.AF_INET6))
        # Controls live in the main namespace. UDP outside the range and same-port TCP
        # must stay here; normal TCP outbounds must not inherit the HY2 socket namespace.
        for family in (socket.AF_INET, socket.AF_INET6):
            bind_ip = PUBLIC[4 if family == socket.AF_INET else 6]
            for kind, port in ((socket.SOCK_DGRAM, 30100), (socket.SOCK_STREAM, 30037)):
                sock = socket.socket(family, kind)
                if family == socket.AF_INET6:
                    sock.setsockopt(socket.IPPROTO_IPV6, socket.IPV6_V6ONLY, 1)
                sock.bind((bind_ip, port))
                if kind == socket.SOCK_STREAM:
                    sock.listen(16)
                listeners.append(sock)

        def serve():
            try:
                while not stop.is_set():
                    for sock in select.select(listeners, [], [], .1)[0]:
                        if sock.type == socket.SOCK_STREAM:
                            accepted, peer = sock.accept()
                            with accepted:
                                accepted.settimeout(5)
                                data = accepted.recv(65535)
                                accepted.sendall(json.dumps({"peer": peer, "received_bytes": len(data)}).encode())
                        else:
                            data, peer = sock.recvfrom(65535)
                            records.append({"peer": peer, "local": sock.getsockname(), "size": len(data),
                                            "process_ns": os.readlink("/proc/self/ns/net")})
                            sock.sendto(json.dumps({"peer": peer, "received_bytes": len(data)}).encode(), peer)
            except Exception:
                errors.append(traceback.format_exc())

        server = threading.Thread(target=serve)
        server.start()

        def request(version, port, source_port, protocol="udp", expected=True, payload_size=128):
            value = at(client.pid, sys.executable, SELF, "--probe", version, port, source_port, protocol,
                       int(expected), payload_size)
            check(not errors, str(errors))
            return json.loads(value)

        def record(name, **details):
            value = {"case": name, "passed": True, **details}
            cases.append(value)
            print(json.dumps(value), flush=True)

        for version in (4, 6):
            for port in (30000, 30001, 30037, 30098, 30099):
                source = 40000 + (port - 30000) + version * 100
                request(version, port, source)
                host_rows = tracked(None, "udp", version, source, port)
                hy_rows = tracked(hy2.pid, "udp", version, source, port)
                check(not host_rows and len(hy_rows) == 1, "UDP CT escaped the socket namespace or was absent")
                record(f"ipv{version}-port-{port}", host_ct=0, hy2_ct=1, tuple=hy_rows[0])
            request(version, 29999, 41000 + version, expected=False)
            check(not tracked(hy2.pid, "udp", version, 41000 + version, 29999), "range lower-bound leaked")
            record(f"ipv{version}-below-range")
            for protocol, port in (("udp", 30100), ("tcp", 30037)):
                source = 42000 + version + (100 if protocol == "tcp" else 0)
                request(version, port, source, protocol)
                check(len(tracked(None, protocol, version, source, port)) == 1, "host positive CT control missing")
                check(not tracked(hy2.pid, protocol, version, source, port), "unrelated control entered HY2 namespace")
                record(f"ipv{version}-{protocol}-host-control", host_ct=1, hy2_ct=0)
            request(version, 30037, 43000 + version, payload_size=1200)
            record(f"ipv{version}-1200-byte-payload")

        # Simultaneous flows use one listening FD per address family and distinct reply
        # mappings. This also catches accidental source NAT and FD namespace migration.
        with concurrent.futures.ThreadPoolExecutor(max_workers=16) as pool:
            futures = [pool.submit(request, 4 if i % 2 == 0 else 6, 30000 + i % 100, 45000 + i)
                       for i in range(200)]
            for future in futures:
                future.result(timeout=20)
        for i in range(200):
            version, port, source = (4 if i % 2 == 0 else 6), 30000 + i % 100, 45000 + i
            check(not tracked(None, "udp", version, source, port), "concurrent flow tracked on host")
            check(len(tracked(hy2.pid, "udp", version, source, port)) == 1, "concurrent flow absent in HY2 namespace")
        record("concurrent-200", host_ct=0, hy2_ct=200)
        check(os.readlink("/proc/self/ns/net") == root_ns, "server process changed namespace")
        check(all(r["process_ns"] == root_ns for r in records), "UDP handling moved the process")
        record("scm-rights-process-stayed-in-main-netns", main_namespace=root_ns,
               socket_birth_namespace=os.readlink(f"/proc/{hy2.pid}/ns/net"))

        # A HY2 NAT table in another namespace must not itself activate automatic
        # tracking of ordinary host traffic. Remove only our positive-control table.
        run("nft", "delete", "table", "inet", "host_control")
        for version in (4, 6):
            request(version, 30037, 44000 + version, "tcp")
            check(not tracked(None, "tcp", version, 44000 + version, 30037),
                  "HY2 namespace NAT activated host TCP tracking")
            request(version, 30037, 44100 + version)
            check(not tracked(None, "udp", version, 44100 + version, 30037), "HY2 UDP returned to host CT")
            check(len(tracked(hy2.pid, "udp", version, 44100 + version, 30037)) == 1,
                  "HY2 NAT stopped tracking after host positive-control removal")
            record(f"ipv{version}-hy2-nat-does-not-enable-host-tcp-tracking")

        if os.environ.get("BROCADE_LAB_XRAY"):
            # The echo listeners release the exact FDs that will be re-created in
            # the socket namespace and inherited by the real Xray core below.
            stop.set()
            server.join(timeout=6)
            check(not server.is_alive() and not errors, "echo server failed to stop")
            for sock in listeners:
                sock.close()
            listeners.clear()
            import hy2 as hy2_tests
            for value in hy2_tests.exercise(client.pid, hy2.pid, output, root_ns):
                record(value.pop("case"), **value)
        result["passed"] = True
        result["received_udp_datagrams"] = len(records)
    except Exception:
        result["failure"] = traceback.format_exc()
        raise
    finally:
        stop.set()
        if "server" in locals():
            server.join(timeout=6)
        for sock in listeners:
            sock.close()
        if "hy2" in locals():
            for name, command in {
                "host-nft.txt": ("nft", "list", "ruleset"),
                "tc-wan.txt": ("tc", "-s", "filter", "show", "dev", "wan", "ingress"),
                "tc-return.txt": ("tc", "-s", "filter", "show", "dev", "hyhost", "ingress"),
                "host-conntrack.txt": ("conntrack", "-L", "-o", "extended"),
            }.items():
                try:
                    (output / name).write_text(run(*command) + "\n")
                except Exception as exc:
                    result.setdefault("collection_errors", []).append(str(exc))
            for name, command in {"hy2-nft.txt": ("nft", "list", "ruleset"),
                                  "hy2-conntrack.txt": ("conntrack", "-L", "-o", "extended"),
                                  "hy2-sockets.txt": ("ss", "-ulnp")}.items():
                try:
                    (output / name).write_text(at(hy2.pid, *command) + "\n")
                except Exception as exc:
                    result.setdefault("collection_errors", []).append(str(exc))
        for child in children:
            child.stdin.close()
            try:
                child.wait(timeout=3)
            except subprocess.TimeoutExpired:
                child.kill()
                child.wait(timeout=3)
            child.stdout.close()
        result.update({"ended": stamp(), "elapsed_seconds": round(time.monotonic() - start, 3)})
        (output / "udp-observations.json").write_text(json.dumps(records, indent=2) + "\n")
        (output / "result.json").write_text(json.dumps(result, indent=2) + "\n")
        print(json.dumps(result), flush=True)


def main():
    if sys.argv[1:] == ["--hold"]:
        hold_namespace()
    elif sys.argv[1:2] == ["--socket"]:
        pass_socket(int(sys.argv[2]), int(sys.argv[3]))
    elif sys.argv[1:2] == ["--probe"]:
        v, p, src, proto, expect, size = sys.argv[2:]
        print(json.dumps(probe(int(v), int(p), int(src), proto, bool(int(expect)), int(size))))
    elif len(sys.argv) == 3 and sys.argv[1] == "--inside":
        main_lab(pathlib.Path(sys.argv[2]))
    elif len(sys.argv) == 2:
        fd = os.open("/proc/self/ns/net", os.O_RDONLY)
        env = dict(os.environ, BROCADE_LAB_ANCESTOR_NET_FD=str(fd))
        try:
            return subprocess.run(["unshare", "--net", "--", sys.executable, SELF, "--inside", sys.argv[1]],
                                  pass_fds=(fd,), env=env, timeout=240).returncode
        finally:
            os.close(fd)
    else:
        raise RuntimeError("expected output directory")
    return 0


if __name__ == "__main__":
    sys.exit(main())
