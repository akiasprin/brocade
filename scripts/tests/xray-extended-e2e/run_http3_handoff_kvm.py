#!/usr/bin/env python3
"""Run the frozen HTTP/3 handoff scenarios on one isolated, verified KVM guest."""
import argparse
import json
import pathlib
import re
import runpy
import shutil
import subprocess
import time
import uuid


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", required=True, type=pathlib.Path)
    parser.add_argument("--tests", required=True, type=pathlib.Path)
    parser.add_argument("--vm-image", required=True, type=pathlib.Path)
    parser.add_argument("--output", required=True, type=pathlib.Path)
    parser.add_argument("--qemu-image", default="cube-qemu-lab:24.04")
    args = parser.parse_args()
    topology = runpy.run_path(str(pathlib.Path(__file__).resolve().parents[1] / "xray-upgrade-topology/run.py"))
    run, digest = topology["run"], topology["digest"]
    if not pathlib.Path("/dev/kvm").exists() or digest(args.vm_image) != topology["IMAGE_SHA256"]:
        raise RuntimeError("real /dev/kvm and the verified topology base image are required")
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=False)
    inputs, vm, results = (output / name for name in ("inputs", "vm", "results"))
    for directory in (inputs, vm, results):
        directory.mkdir()
    for source, name in ((args.binary, "xray"), (args.tests, "scenarios.test")):
        shutil.copy2(source.resolve(strict=True), inputs / name)
    startup = """#!/bin/sh
set -eu
mkdir -p /lab
mount -t 9p -o trans=virtio,version=9p2000.L lab /lab
uname -r > /lab/results/kernel.txt
set +e
BROCADE_XRAY_BIN=/lab/inputs/xray /lab/inputs/scenarios.test -test.v -test.run '^TestBrocadeFrozenHTTP3GenerationHandoff$' -test.count=3 -test.timeout=90s > /lab/results/tests.log 2>&1
code=$?
printf '{"exit_code":%s}\\n' "$code" > /lab/results/exit.tmp
mv /lab/results/exit.tmp /lab/results/exit.json
sync
"""
    token = uuid.uuid4().hex[:12]
    name = "brocade-http3-handoff-" + token
    (vm / "user-data").write_text("#cloud-config\nwrite_files:\n  - path: /usr/local/bin/http3-handoff\n    permissions: '0755'\n    content: |\n" + "".join("      " + line + "\n" for line in startup.splitlines()) + "runcmd:\n  - [sh, -c, '/usr/local/bin/http3-handoff > /var/log/http3-handoff.log 2>&1 &']\n")
    (vm / "meta-data").write_text("instance-id: " + name + "\n")
    (vm / "network-config").write_text("version: 2\nethernets: {}\n")
    base = str(args.vm_image.resolve())
    manifest = {"binary_sha256": digest(inputs / "xray"), "test_program_sha256": digest(inputs / "scenarios.test"),
                "base_sha256": digest(args.vm_image), "accelerator": "kvm", "network": "none",
                "tooling_image_id": run("docker", "image", "inspect", "--format", "{{.Id}}", args.qemu_image)}
    (output / "manifest.json").write_text(json.dumps(manifest, indent=2) + "\n")
    run("docker", "run", "--rm", "--network", "none", "-v", f"{vm}:/vm", "-v", f"{base}:/base.img:ro", args.qemu_image,
        "qemu-img", "create", "-f", "qcow2", "-F", "qcow2", "-b", "/base.img", "/vm/root.qcow2", "4G")
    run("docker", "run", "--rm", "--network", "none", "-v", f"{vm}:/vm", args.qemu_image,
        "cloud-localds", "--network-config=/vm/network-config", "/vm/seed.img", "/vm/user-data", "/vm/meta-data")
    try:
        run("docker", "run", "-d", "--name", name, "--network", "none", "--device", "/dev/kvm", "--memory", "1400m", "--pids-limit", "256",
            "-v", f"{output}:/lab", "-v", f"{vm}:/vm", "-v", f"{base}:/base.img:ro", args.qemu_image,
            "qemu-system-x86_64", "-accel", "kvm", "-cpu", "host", "-smp", "2", "-m", "1024", "-display", "none", "-nic", "none",
            "-serial", "file:/lab/results/serial.log", "-drive", "file=/vm/root.qcow2,if=virtio,format=qcow2",
            "-drive", "file=/vm/seed.img,if=virtio,format=raw,readonly=on", "-fsdev", "local,id=lab,path=/lab,security_model=none",
            "-device", "virtio-9p-pci,fsdev=lab,mount_tag=lab")
        deadline = time.monotonic() + 240
        while not (results / "exit.json").exists():
            if time.monotonic() >= deadline or run("docker", "inspect", "-f", "{{.State.Running}}", name) != "true":
                raise RuntimeError("HTTP/3 KVM did not finish; inspect retained serial output")
            time.sleep(1)
        outcome = json.loads((results / "exit.json").read_text())
        text = (results / "tests.log").read_text()
        cases = ["TestBrocadeFrozenHTTP3GenerationHandoff/" + mode for mode in ("xhttp", "masque", "masque-mixed")]
        if outcome["exit_code"] or "--- SKIP:" in text or "--- FAIL:" in text:
            raise RuntimeError("HTTP/3 handoff scenarios failed or skipped")
        for case in cases:
            if len(re.findall(r"^\s*--- PASS: " + re.escape(case) + r" ", text, re.MULTILINE)) != 3:
                raise RuntimeError("required HTTP/3 mode did not pass three times: " + case)
        outcome.update({"passed": True, "mode_executions": 9, "kernel": (results / "kernel.txt").read_text().strip(),
                        "log_sha256": digest(results / "tests.log")})
        (output / "result.json").write_text(json.dumps(outcome, indent=2) + "\n")
        print(json.dumps(outcome), flush=True)
    finally:
        try:
            (results / "container.log").write_text(run("docker", "logs", name, timeout=15))
        finally:
            subprocess.run(["docker", "rm", "-f", name], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=15)


if __name__ == "__main__":
    main()
