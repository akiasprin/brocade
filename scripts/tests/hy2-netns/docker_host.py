#!/usr/bin/env python3
"""Real Docker bridge/NAT beside the actual Agent in an isolated KVM network."""
import datetime
import hashlib
import http.server
import json
import os
from pathlib import Path
import shutil
import signal
import socket
import subprocess
import sys
import time
import traceback
import uuid

from workload import PUBLIC, CLIENT, at, check, namespace, run
from hy2 import ct, stop
from production import exercise, tuples
from faults_fixture import Lab, read_json

INPUTS = Path(__file__).resolve().parent
SELF = str(Path(__file__).resolve())


def docker(*args):
    return run("docker", "--host", "unix:///run/hy2-docker.sock", *args, timeout=45)


def processes(config):
    rows = []
    for directory in Path("/proc").glob("[0-9]*"):
        try:
            args = (directory / "cmdline").read_bytes().split(b"\0")
            if str(config).encode() in args and "xray" in os.readlink(directory / "exe"):
                with (directory / "exe").open("rb") as stream:
                    digest = hashlib.file_digest(stream, "sha256").hexdigest()
                rows.append({"pid": int(directory.name), "sha256": digest})
        except (OSError, ValueError):
            pass
    return rows


UNIT = "brocade-agent"


def start_agent(lab, output):
    run("systemd-run", "--quiet", "--collect", "--unit=" + UNIT,
        "--property=KillMode=process", "--property=LogNamespace=brocade-agent",
        "--property=Restart=always", "--property=RestartSec=5",
        f"--property=NetworkNamespacePath=/proc/{os.getpid()}/ns/net",
        "--property=StandardOutput=append:" + str(output / "agent-service.log"),
        "--property=StandardError=inherit", "--setenv=BROCADE_XRAY_BIN=" + str(lab.xray),
        str(INPUTS / "agent"), "run", "--state-dir", str(lab.state),
        "--server", lab.server, "--token", "broc_node_fault_lab", "--apply", "linux")


def await_reconcile(state, prior_mtime=0):
    path = state / "local-reconcile.json"
    deadline = time.monotonic() + 35
    while time.monotonic() < deadline:
        if path.exists() and path.stat().st_mtime_ns > prior_mtime:
            result = read_json(path)
            if result is not None:
                check(not result.get("error"), "resident Agent convergence failed: " + str(result.get("error")))
                return
        time.sleep(.1)
    raise TimeoutError("resident Agent did not record local convergence")


def namespace_commands(config, namespace):
    serving = processes(config)
    check(len(serving) == 1, "managed Xray identity was not unique")
    return ["nsenter", f"--mount=/proc/{serving[0]['pid']}/ns/mnt", "--", "ip", "netns", "exec", namespace]


def docker_rules():
    data = json.loads(run("nft", "-j", "list", "ruleset"))["nftables"]
    data = [item for item in data if any(value.get("family") in ("ip", "ip6") and value.get("table") in ("nat", "filter", "raw")
                                       for value in item.values() if isinstance(value, dict))]
    def clean(value):
        if isinstance(value, dict):
            return {k: clean(v) for k, v in value.items() if k not in ("handle", "packets", "bytes")}
        if isinstance(value, list):
            return [clean(v) for v in value]
        return value
    return clean(data)


def hy2_rows(version, namespace=None, source=None):
    prefix = namespace or []
    result = subprocess.run(prefix + ["conntrack", "-L", "-o", "extended", "-f", "ipv4" if version == 4 else "ipv6", "-p", "udp",
                                     "--orig-src", source or CLIENT[version], "--orig-dst", PUBLIC[version]],
                            text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=5)
    check(result.returncode == 0, "conntrack query failed: " + result.stderr)
    return {row for row in tuples(result.stdout.splitlines()) if 30000 <= int(row[3]) <= 30199}


def check_docker(client, output, label, cases, namespace=None):
    observed = json.loads(docker("exec", "hy2-echo", "/echo", "egress", f"http://{CLIENT[4]}:18084/"))
    check(observed["source"] == PUBLIC[4], "container outbound did not use real source NAT")
    rows = ct(None, "-f", "ipv4", "-p", "tcp", "--orig-dst", CLIENT[4], "--dport", "18084")
    check(bool(rows), "Docker egress conntrack positive control missing")
    cases.append({"case": label + "-container-source-nat", "passed": True, "observed_source": observed["source"], "main_conntrack_verified": True})
    script = "import urllib.request; assert urllib.request.urlopen('http://198.18.0.1:28080/',timeout=3).read()==b'container-ok'"
    at(client, "python3", "-c", script)
    cases.append({"case": label + "-published-tcp", "passed": True})
    script = "import socket;s=socket.socket(socket.AF_INET,socket.SOCK_DGRAM);s.settimeout(3);s.sendto(b'docker-udp',('198.18.0.1',28081));data,peer=s.recvfrom(128);assert data==b'docker-udp' and peer==('198.18.0.1',28081)"
    at(client, "python3", "-c", script)
    cases.append({"case": label + "-published-udp", "passed": True})
    for version, port in ((4, 30037), (6, 30137)):
        directory = output / (label + f"-hy2-{version}"); directory.mkdir()
        before = hy2_rows(version)
        before_namespace = hy2_rows(version, namespace) if namespace else set()
        record = exercise(client, directory, version, port)
        if namespace:
            check(not (hy2_rows(version) - before), "Docker coexistence leaked a new HY2 flow into main conntrack")
            check(bool(hy2_rows(version, namespace) - before_namespace), "new HY2 flow missing from isolated conntrack")
            record["main_new_hy2_entries"] = 0
            record["isolated_conntrack_verified"] = True
        cases.append({"case": label + f"-hy2-ipv{version}-tcp-and-udp", "passed": True, **record})
    print(json.dumps({"phase": label, "cases_checked": len(cases)}), flush=True)


def inside(output):
    started = time.monotonic(); stamp = datetime.datetime.now(datetime.timezone.utc).isoformat()
    ancestor = int(os.environ["BROCADE_HY2_LAB_ANCESTOR_FD"])
    check(os.readlink(f"/proc/self/fd/{ancestor}") != os.readlink("/proc/self/ns/net"), "not a private network")
    check([name for _, name in socket.if_nameindex()] == ["lo"], "requires a fresh disposable network")
    output.mkdir(parents=True, exist_ok=True)
    if any(parts[4] == "/proc/sys" and "ro" in parts[5].split(",") for parts in (line.split() for line in Path("/proc/self/mountinfo").read_text().splitlines())):
        run("mount", "-o", "remount,rw", "/proc/sys")
    cases = []; children = []; daemon = None; lab = None; agent_started = False; state = Path("/var/tmp") / ("hy2-docker-" + uuid.uuid4().hex[:8])
    result = {"passed": False, "compatible": False, "cases": cases, "started_at": stamp,
              "kernel": os.uname().release, "iptables": run("iptables", "--version"), "systemd_log_namespace": True}
    config_path = state / "xray.json"
    try:
        client = namespace(); children.append(client)
        run("ip", "link", "set", "lo", "up")
        run("ip", "link", "add", "wan", "type", "veth", "peer", "name", "client")
        run("ip", "link", "set", "client", "netns", client.pid)
        for pid, name, v4, v6 in ((None, "wan", PUBLIC[4], PUBLIC[6]), (client.pid, "client", CLIENT[4], CLIENT[6])):
            execute = (lambda *args: at(pid, *args)) if pid else run
            execute("ip", "link", "set", "lo", "up")
            execute("ip", "addr", "add", v4 + "/24", "dev", name)
            execute("ip", "-6", "addr", "add", v6 + "/64", "dev", name, "nodad")
            execute("ip", "link", "set", name, "up")
        run("tc", "qdisc", "add", "dev", "wan", "root", "handle", "1:", "tbf", "rate", "1gbit", "burst", "1m", "latency", "50ms")
        run("tc", "qdisc", "add", "dev", "wan", "parent", "1:1", "handle", "2:", "fq")
        target = subprocess.Popen(["nsenter", f"--net=/proc/{client.pid}/ns/net", "--", "python3", str(INPUTS / "hy2.py"), "--target"],
                                  stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True)
        children.append(target); check(target.stdout.readline().strip() == "ready", "proxy echo target failed")
        http_log = (output / "egress-target.log").open("wb")
        http_target = subprocess.Popen(["nsenter", f"--net=/proc/{client.pid}/ns/net", "--", "python3", SELF, "--egress-target"], stdout=http_log, stderr=http_log)
        children.append(http_target)
        daemon_log = (output / "dockerd.log").open("wb")
        daemon = subprocess.Popen(["dockerd", "--host", "unix:///run/hy2-docker.sock", "--data-root", "/var/lib/hy2-docker", "--exec-root", "/run/hy2-docker", "--pidfile", "/run/hy2-docker.pid", "--storage-driver", "vfs"], stdout=daemon_log, stderr=daemon_log)
        for _ in range(100):
            check(daemon.poll() is None, "real dockerd exited; inspect dockerd.log")
            try:
                docker("info", "--format", "{{json .ServerVersion}}"); break
            except RuntimeError:
                time.sleep(.2)
        else:
            raise TimeoutError("Docker daemon did not become ready")
        result["docker_version"] = docker("version", "--format", "{{.Server.Version}}")
        print(json.dumps({"phase": "real-dockerd-ready", "docker_version": result["docker_version"]}), flush=True)
        docker("import", str(INPUTS / "docker-rootfs.tar"), "hy2-fixture:local")
        docker("run", "-d", "--name", "hy2-echo", "--security-opt", "apparmor=unconfined", "-p", "28080:8080/tcp", "-p", "28081:8081/udp", "hy2-fixture:local", "/echo")
        result["docker_bridge"] = json.loads(docker("network", "inspect", "bridge"))
        forward = run("iptables", "-S", "FORWARD")
        check("-P FORWARD DROP" in forward, "default Docker FORWARD DROP was not exercised")
        cases.append({"case": "real-docker-default-forward-drop", "passed": True})
        # Reuse the existing frozen-work HTTP fixture; the resident production
        # Agent downloads, verifies, installs and reports the actual release.
        lab = Lab("binary-success", output, INPUTS)
        state = lab.state; config_path = state / "xray.json"
        (state / "source-access-policy.json").write_text(json.dumps({"revision": 0, "node_ips": [], "users": []}))
        with socket.socket() as reservation:
            reservation.bind(("127.0.0.1", 0)); api_port = reservation.getsockname()[1]
        config = {"log": {"loglevel": "info"}, "stats": {}, "api": {"tag": "api", "listen": f"127.0.0.1:{api_port}", "services": ["HandlerService", "StatsService"]},
                  "policy": {"levels": {"0": {"statsUserUplink": True, "statsUserDownlink": True}}}, "inbounds": [], "outbounds": [{"tag": "out:forward", "protocol": "freedom"}]}
        grants = []
        for tag, listen, port in (("hy4", "0.0.0.0", 30000), ("hy6", "::", 30100)):
            config["inbounds"].append({"tag": tag, "listen": listen, "port": port, "protocol": "hysteria", "settings": {"version": 2, "users": []},
                                       "streamSettings": {"network": "hysteria", "security": "tls", "hysteriaSettings": {"version": 2}, "tlsSettings": {"certificates": [{"certificateFile": str(INPUTS / "cover.pem"), "keyFile": str(INPUTS / "cover.key")}]}}})
            grants.append({"tag": tag, "clients": [{"email": tag + "@lab.test", "uuid": "hy2-netns-disposable-test", "flow": None, "level": 0}]})
        config_path.write_text(json.dumps(config)); (state / "grants.desired.json").write_text(json.dumps(grants)); (state / "usage-generation").write_text("1")
        (state / "hy2_port_hop.json").write_text(json.dumps({"redirects": [{"start": 30000, "end": 30099, "to": 30000}, {"start": 30100, "end": 30199, "to": 30100}]}))
        old = lab.xray; shutil.copy2(INPUTS / "legacy-xray", old); old.chmod(0o700)
        start_agent(lab, output); agent_started = True; await_reconcile(state)
        before = processes(config_path); check(len(before) == 1, "old managed Xray identity was not unique")
        check_docker(client.pid, output, "before", cases)
        baseline = docker_rules(); (output / "docker-rules-before.json").write_text(json.dumps(baseline, indent=2))
        lab.candidate_bytes = (INPUTS / "xray").read_bytes()
        lab.offer = {"release_id": 91, "attempt": 1, "version": run(str(INPUTS / "xray"), "version").split()[1],
                     "url": lab.server + "/candidate", "sha256": hashlib.sha256(lab.candidate_bytes).hexdigest(),
                     "previous_sha256": hashlib.sha256((INPUTS / "legacy-xray").read_bytes()).hexdigest(), "strategy": "restart"}
        (output / "release-offer.json").write_text(json.dumps(lab.offer, indent=2))
        run("systemctl", "restart", UNIT)
        deadline = time.monotonic() + 70
        while not lab.receipts and time.monotonic() < deadline:
            time.sleep(.1)
        check(bool(lab.receipts), "resident Agent did not report the Xray binary release")
        receipt = lab.receipts[-1]; (output / "release-receipt.json").write_text(json.dumps(receipt, indent=2))
        result["release_receipt"] = receipt
        expected_sha = hashlib.sha256(lab.candidate_bytes).hexdigest()
        result["compatible"] = receipt["outcome"] == "succeeded" and receipt["performed_update"]
        result["migration_succeeded"] = result["compatible"]
        after = processes(config_path)
        if result["compatible"]:
            check(hashlib.sha256(old.read_bytes()).hexdigest() == expected_sha, "release installed incorrect Xray bytes")
        else:
            check(hashlib.sha256(old.read_bytes()).hexdigest() == hashlib.sha256((INPUTS / "legacy-xray").read_bytes()).hexdigest(),
                  "failed release did not restore the previous installed bytes")
            result["old_binary_recovered"] = True
        cases.append({"case": "hy2-migrated-with-real-docker", "passed": result["compatible"]})
        namespace_name = namespace_commands(config_path, json.loads((state / "hy2-network.json").read_text())["namespace"]) if result["compatible"] else None
        check_docker(client.pid, output, "after", cases, namespace_name)
        if result["compatible"]:
            check(len(after) == 1 and after[0]["sha256"] == expected_sha, "candidate process identity is wrong")
            for version, port in ((4, 30037), (6, 30137)):
                directory = output / f"local-public-hy2-{version}"; directory.mkdir()
                record = exercise(os.getpid(), directory, version, port)
                check(not hy2_rows(version, source=PUBLIC[version]), "local HY2 probe leaked into main conntrack with Docker enabled")
                check(bool(hy2_rows(version, namespace_name, PUBLIC[version])), "local HY2 probe absent in isolated conntrack")
                cases.append({"case": f"local-public-ipv{version}-probe", "passed": True, **record})
            prior = processes(config_path)
            prior_mtime = (state / "local-reconcile.json").stat().st_mtime_ns
            old_agent = int(run("systemctl", "show", UNIT, "-p", "MainPID", "--value"))
            old_mount = os.readlink(f"/proc/{old_agent}/ns/mnt")
            run("systemctl", "restart", UNIT)
            await_reconcile(state, prior_mtime)
            current_state = json.loads((state / "hy2-network.json").read_text())
            new_agent = int(run("systemctl", "show", UNIT, "-p", "MainPID", "--value"))
            check(new_agent != old_agent and os.readlink(f"/proc/{new_agent}/ns/mnt") != old_mount,
                  "Agent service restart did not exercise a new mount namespace")
            identity = run("nsenter", f"--mount=/proc/{new_agent}/ns/mnt", "--", "stat", "-Lc", "%d:%i",
                           "/run/netns/" + current_state["namespace"])
            check(identity == f"{os.stat('/proc/self/ns/net').st_dev}:{current_state['namespace_inode']}",
                  "new Agent did not restore the original namespace handle in its own mount view")
            check(current_state["phase"] == "active" and processes(config_path) == prior,
                  "Agent restart did not adopt the existing Xray and network")
            cases.append({"case": "agent-restart-adopts-live-hy2-namespace", "passed": True,
                          "mount_namespace_changed": True, "original_namespace_identity_verified": True})
            check_docker(client.pid, output, "after-agent-restart", cases, namespace_commands(config_path, current_state["namespace"]))
        after_rules = docker_rules(); (output / "docker-rules-after.json").write_text(json.dumps(after_rules, indent=2))
        check(after_rules == baseline, "Agent changed Docker's firewall rules")
        cases.append({"case": "docker-firewall-preserved", "passed": True})
        result["safe_after_attempt"] = True
        result["passed"] = result["compatible"] and len(cases) == 21 and all(c["passed"] for c in cases)
    except Exception:
        result["compatible"] = False
        result["failure"] = traceback.format_exc()
        raise
    finally:
        if agent_started:
            run("systemctl", "stop", UNIT)
        if lab is not None:
            lab.http.shutdown(); lab.upstream.shutdown(); lab.udp_upstream.close()
        (output / "all-firewall.json").write_text(run("nft", "-j", "list", "ruleset"))
        for process in processes(config_path):
            try:
                os.kill(process["pid"], signal.SIGTERM)
            except ProcessLookupError:
                pass
        if daemon is not None:
            try:
                docker("rm", "-f", "hy2-echo")
            except Exception:
                pass
            stop(daemon)
        for process in reversed(children):
            stop(process)
        result["elapsed_seconds"] = time.monotonic() - started
        result["ended_at"] = datetime.datetime.now(datetime.timezone.utc).isoformat()
        (output / "result.json").write_text(json.dumps(result, indent=2) + "\n")
        print(json.dumps({k: v for k, v in result.items() if k not in ("docker_bridge", "cases")}), flush=True)
    return 0 if result["passed"] else 2


if __name__ == "__main__":
    if sys.argv[1:2] == ["--egress-target"]:
        class Handler(http.server.BaseHTTPRequestHandler):
            def do_GET(self):
                body = json.dumps({"source": self.client_address[0]}).encode()
                self.send_response(200); self.send_header("Content-Length", str(len(body))); self.end_headers(); self.wfile.write(body)
        http.server.HTTPServer((CLIENT[4], 18084), Handler).serve_forever()
    elif sys.argv[1:2] == ["--inside"]:
        sys.exit(inside(Path(sys.argv[2])))
    else:
        fd = os.open("/proc/self/ns/net", os.O_RDONLY)
        try:
            result = subprocess.run(["unshare", "--mount", "--net", "--", "python3", SELF, "--inside", sys.argv[1]], env=dict(os.environ, BROCADE_HY2_LAB_ANCESTOR_FD=str(fd)), pass_fds=(fd,), timeout=340)
            sys.exit(result.returncode)
        finally:
            os.close(fd)
