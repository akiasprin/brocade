#!/usr/bin/env python3
"""Run the same branched, dual-stack Xray topology in containers or four real KVMs."""
import argparse
import hashlib
import json
import pathlib
import shutil
import subprocess
import time
import uuid

ROLES = ("entry", "exit-a", "exit-b", "client")
IMAGE_SHA256 = "6a81c37564db9b1ee84e141922625e1d7c5b389b99bb3c572e0243607d5bb4d2"


def run(*command, timeout=60):
    return subprocess.check_output(command, text=True, stderr=subprocess.STDOUT, timeout=timeout).strip()


def digest(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--mode", choices=("docker", "kvm"), required=True)
    parser.add_argument("--binary", type=pathlib.Path, required=True)
    parser.add_argument("--old-binary", type=pathlib.Path, required=True)
    parser.add_argument("--topology", type=pathlib.Path, required=True)
    parser.add_argument("--reverse-config-dir", type=pathlib.Path, required=True)
    parser.add_argument("--agent-tests", type=pathlib.Path, required=True)
    parser.add_argument("--ktls-tests-dir", type=pathlib.Path)
    parser.add_argument("--shared-budget-tests", action="store_true", help="KVM real kTLS and shared user/machine budget E2E")
    parser.add_argument("--output", type=pathlib.Path, required=True)
    parser.add_argument("--old-entry", action="store_true")
    parser.add_argument("--old-exit-a", action="store_true")
    parser.add_argument("--image", default="brocade-xray-topology:local")
    parser.add_argument("--qemu-image", default="cube-qemu-lab:24.04")
    parser.add_argument("--vm-image", type=pathlib.Path)
    args = parser.parse_args()
    if args.ktls_tests_dir and args.mode != "kvm":
        raise ValueError("real kernel TLS verification is restricted to the disposable KVM guests")
    if args.mode == "kvm" and args.vm_image is None:
        parser.error("--vm-image is required for KVM")
    if args.shared_budget_tests and not args.ktls_tests_dir:
        parser.error("--shared-budget-tests requires real KVM kTLS tests")
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=False)
    inputs, results = output / "inputs", output / "results"
    inputs.mkdir()
    results.mkdir()
    for source, name in ((args.binary, "xray"), (args.old_binary, "xray-old"), (args.topology, "production-topology.json"),
                         (args.agent_tests, "agent-tests"),
                         (pathlib.Path(__file__).with_name("workload.py"), "workload.py")):
        shutil.copy2(source.resolve(strict=True), inputs / name)
    for role in ("entry", "exit-a"):
        shutil.copy2(args.reverse_config_dir / (role + ".json"), inputs / ("rendered-reverse-" + role + ".json"))
    if args.ktls_tests_dir:
        for name in ("tls-kernel.test", "anytls-kernel.test"):
            shutil.copy2(args.ktls_tests_dir / name, inputs / name)
    if args.shared_budget_tests:
        (inputs / "shared-budget-tests").write_text("required\n")
    topology = json.loads(args.topology.read_text())
    if not topology["steps"] or not any(i["hy2"] and i["anytls"] for i in topology["ingresses"]):
        raise RuntimeError("the selected live topology does not support this representative fixture")
    generated = run(str(args.binary.resolve()), "x25519")
    fields = dict(line.split(":", 1) for line in generated.splitlines() if ":" in line)
    identity = {"private": fields["PrivateKey"].strip(), "public": fields["Password (PublicKey)"].strip()}
    (inputs / "identity.json").write_text(json.dumps(identity))
    (inputs / "identity.json").chmod(0o600)
    versions = {role: "new" for role in ROLES}
    if args.old_entry:
        versions["entry"] = "old"
    if args.old_exit_a:
        versions["exit-a"] = "old"
    (inputs / "versions.json").write_text(json.dumps(versions))
    run("openssl", "req", "-x509", "-newkey", "ec", "-pkeyopt", "ec_paramgen_curve:P-256", "-noenc",
        "-keyout", str(inputs / "cover.key"), "-out", str(inputs / "cover.pem"), "-days", "1",
        "-subj", "/CN=cover.test", "-addext", "subjectAltName=DNS:cover.test,DNS:a.test,DNS:b.test")
    image = args.qemu_image if args.mode == "kvm" else args.image
    manifest = {"mode": args.mode, "shared_budget_tests": args.shared_budget_tests, "versions": versions, "binary_sha256": digest(inputs / "xray"),
                "old_binary_sha256": digest(inputs / "xray-old"), "workload_sha256": digest(inputs / "workload.py"),
                "agent_tests_sha256": digest(inputs / "agent-tests"),
                "reverse_config_sha256": {role: digest(inputs / ("rendered-reverse-" + role + ".json")) for role in ("entry", "exit-a")},
                "ktls_tests_sha256": {p.name: digest(p) for p in inputs.glob("*-kernel.test")},
                "topology_sha256": digest(inputs / "production-topology.json"),
                "image_id": run("docker", "image", "inspect", "--format", "{{.Id}}", image),
                "live_structure": {"nodes": len(topology["nodes"]), "ingresses": len(topology["ingresses"]), "steps": len(topology["steps"])},
                "fixture": "dual-stack entry fanout to two VLESS Encryption exits, Mux pool, three ingress protocols"}
    if args.mode == "kvm":
        if not pathlib.Path("/dev/kvm").exists() or digest(args.vm_image) != IMAGE_SHA256:
            raise RuntimeError("verified base image and real /dev/kvm are required")
        manifest["vm_image_sha256"] = IMAGE_SHA256
    (output / "manifest.json").write_text(json.dumps(manifest, indent=2) + "\n")
    token = uuid.uuid4().hex[:10]
    network = "brocade-xray-topology-" + token
    containers = []
    created_network = False
    try:
        if args.mode == "docker":
            run("docker", "network", "create", "--internal", "--ipv6", "--subnet", "10.234.77.0/24", "--subnet", "fd42:b10c:ade::/64", network)
        else:
            run("docker", "network", "create", "--internal", "--subnet", "172.30.77.0/24", network)
        created_network = True
        for role in ROLES:
            name = network + "-" + role
            if args.mode == "docker":
                number = {"client": 10, "entry": 20, "exit-a": 30, "exit-b": 40}[role]
                run("docker", "run", "-d", "--name", name, "--network", network,
                    "--ip", f"10.234.77.{number}", "--ip6", f"fd42:b10c:ade::{number}",
                    "--cap-drop", "ALL", "--cap-add", "DAC_OVERRIDE", "--cap-add", "NET_ADMIN", "--memory", "768m", "--cpus", "2", "--pids-limit", "256",
                    "--read-only", "--tmpfs", "/tmp:rw,size=128m", "-v", f"{output}:/lab", image,
                    "python3", "/lab/inputs/workload.py", role)
            else:
                vm = output / ("vm-" + role)
                vm.mkdir()
                number = {"client": 10, "entry": 20, "exit-a": 30, "exit-b": 40}[role]
                startup = f"""#!/bin/sh
set -eu
mkdir -p /lab
mount -t 9p -o trans=virtio,version=9p2000.L lab /lab
python3 /lab/inputs/workload.py {role}
"""
                (vm / "user-data").write_text("#cloud-config\nwrite_files:\n  - path: /usr/local/bin/xray-topology\n    permissions: '0755'\n    content: |\n" + "".join("      " + line + "\n" for line in startup.splitlines()) + "runcmd:\n  - [sh, -c, '/usr/local/bin/xray-topology > /var/log/xray-topology.log 2>&1 &']\n")
                (vm / "meta-data").write_text("instance-id: xray-topology-" + token + "-" + role + "\n")
                (vm / "network-config").write_text(f"version: 2\nethernets:\n  eth0:\n    match:\n      macaddress: '52:54:00:77:00:{number}'\n    set-name: eth0\n    dhcp4: false\n    dhcp6: false\n    accept-ra: false\n    addresses: [10.234.77.{number}/24, 'fd42:b10c:ade::{number}/64']\n")
                base = str(args.vm_image.resolve())
                run("docker", "run", "--rm", "--network", "none", "-v", f"{vm}:/vm", "-v", f"{base}:/base.img:ro", image,
                    "qemu-img", "create", "-f", "qcow2", "-F", "qcow2", "-b", "/base.img", "/vm/root.qcow2", "4G")
                run("docker", "run", "--rm", "--network", "none", "-v", f"{vm}:/vm", image,
                    "cloud-localds", "--network-config=/vm/network-config", "/vm/seed.img", "/vm/user-data", "/vm/meta-data")
                # Each node has its own guest kernel. Socket multicast is an isolated
                # Ethernet segment within the randomly owned internal Docker network.
                group = "230.77." + str(int(token[:2], 16)) + "." + str(int(token[2:4], 16))
                run("docker", "run", "-d", "--name", name, "--network", network, "--ip", f"172.30.77.{number}", "--device", "/dev/kvm",
                    "--memory", "1400m", "--pids-limit", "256", "-v", f"{output}:/lab", "-v", f"{vm}:/vm", "-v", f"{base}:/base.img:ro", image,
                    "qemu-system-x86_64", "-accel", "kvm", "-cpu", "host", "-smp", "2", "-m", "1024", "-display", "none",
                    "-serial", f"file:/lab/results/{role}-serial.log", "-drive", "file=/vm/root.qcow2,if=virtio,format=qcow2",
                    "-drive", "file=/vm/seed.img,if=virtio,format=raw,readonly=on",
                    "-netdev", f"socket,id=net0,mcast={group}:27777,localaddr=172.30.77.{number}", "-device", f"virtio-net-pci,netdev=net0,mac=52:54:00:77:00:{number}",
                    "-fsdev", "local,id=lab,path=/lab,security_model=none", "-device", "virtio-9p-pci,fsdev=lab,mount_tag=lab")
            containers.append(name)
        seen = 0
        deadline = time.monotonic() + 900
        while time.monotonic() < deadline:
            failures = list(results.glob("*-failure.txt"))
            if failures:
                raise RuntimeError(failures[0].read_text())
            evidence = results / "evidence.json"
            if evidence.exists():
                records = json.loads(evidence.read_text())
                for record in records[seen:]:
                    print(json.dumps(record), flush=True)
                seen = len(records)
            if (results / "result.json").exists():
                result = json.loads((results / "result.json").read_text())
                expected = (25 if args.ktls_tests_dir else 24) + (3 if args.shared_budget_tests else 0)
                if not result["passed"] or result["cases"] != expected or seen != expected:
                    raise RuntimeError("incomplete or failed topology matrix")
                for role in ("entry", "exit-a", "exit-b", "client"):
                    if not (results / (role + "-netem.json")).exists():
                        raise RuntimeError("missing real network impairment evidence")
                print(json.dumps(result), flush=True)
                return
            for name in containers:
                if run("docker", "inspect", "-f", "{{.State.Running}}", name) != "true":
                    raise RuntimeError("topology process exited: " + name)
            time.sleep(1)
        raise RuntimeError("topology exceeded 15-minute bound; inspect node and serial logs")
    finally:
        for name in reversed(containers):
            (results / (name + "-container.log")).write_text(run("docker", "logs", name))
            run("docker", "rm", "-f", name)
        if created_network:
            run("docker", "network", "rm", network)


if __name__ == "__main__":
    main()
