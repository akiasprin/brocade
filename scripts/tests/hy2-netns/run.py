#!/usr/bin/env python3
"""Run the identical frozen namespace workload in Docker or verified hardware KVM."""
import argparse
import datetime
import hashlib
import json
import os
import pathlib
import shutil
import subprocess
import time
import uuid


BASE_SHA256 = "6a81c37564db9b1ee84e141922625e1d7c5b389b99bb3c572e0243607d5bb4d2"


def run(*args, timeout=60):
    return subprocess.check_output([str(a) for a in args], text=True, stderr=subprocess.STDOUT,
                                   timeout=timeout).strip()


def digest(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def now():
    return datetime.datetime.now(datetime.timezone.utc).isoformat()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--mode", choices=("docker", "kvm"), required=True)
    parser.add_argument("--output", type=pathlib.Path, required=True)
    parser.add_argument("--image", default="brocade-hy2-netns:20261031")
    parser.add_argument("--qemu-image", default="cube-qemu-lab:24.04")
    parser.add_argument("--vm-image", type=pathlib.Path)
    parser.add_argument("--xray", type=pathlib.Path, help="prototype xray_fd.go binary built from frozen local Xray sources")
    parser.add_argument("--agent-tests", type=pathlib.Path, help="run the production Agent restart test with the actual Xray CLI")
    parser.add_argument("--agent", type=pathlib.Path, help="also run migration/restart through this release Agent CLI")
    parser.add_argument("--legacy-xray", type=pathlib.Path, help="also exercise this real older Xray and return to the new bytes")
    parser.add_argument("--legacy-agent", type=pathlib.Path, help="also exercise this older Agent after network downgrade")
    parser.add_argument("--docker-host-image", type=pathlib.Path, help="KVM-only real Docker host scenario; offline container rootfs tar")
    args = parser.parse_args()
    if args.agent_tests and not args.xray:
        parser.error("--agent-tests requires --xray")
    if args.agent and not args.agent_tests:
        parser.error("--agent requires --agent-tests")
    if args.legacy_xray and not args.agent:
        parser.error("--legacy-xray requires --agent")
    if args.legacy_agent and not args.legacy_xray:
        parser.error("--legacy-agent requires --legacy-xray")
    if args.docker_host_image and (args.mode != "kvm" or not args.agent or not args.legacy_xray):
        parser.error("--docker-host-image requires KVM, --agent and --legacy-xray")
    start, started = time.monotonic(), now()
    output = args.output.resolve()
    output.mkdir(mode=0o755, parents=True, exist_ok=False)
    inputs, results = output / "inputs", output / "results"
    inputs.mkdir(mode=0o755)
    results.mkdir(mode=0o755)
    workload = pathlib.Path(__file__).with_name("workload.py")
    shutil.copy2(workload, inputs / "workload.py")
    shutil.copy2(pathlib.Path(__file__).with_name("hy2.py"), inputs / "hy2.py")
    entrypoint = "production.py" if args.agent_tests else "workload.py"
    if args.docker_host_image:
        entrypoint = "docker_host.py"
        shutil.copy2(pathlib.Path(__file__).with_name("production.py"), inputs / "production.py")
        shutil.copy2(pathlib.Path(__file__).resolve().parents[1] / "xray-faults/run.py", inputs / "faults_fixture.py")
        shutil.copy2(args.docker_host_image.resolve(strict=True), inputs / "docker-rootfs.tar")
    if args.agent_tests:
        shutil.copy2(pathlib.Path(__file__).with_name(entrypoint), inputs / entrypoint)
        shutil.copy2(args.agent_tests.resolve(strict=True), inputs / "agent-tests")
        if args.agent:
            shutil.copy2(args.agent.resolve(strict=True), inputs / "agent")
        for source, name in [(args.legacy_xray, "legacy-xray"), (args.legacy_agent, "legacy-agent")]:
            if source:
                shutil.copy2(source.resolve(strict=True), inputs / name)
    if args.xray:
        shutil.copy2(args.xray.resolve(strict=True), inputs / ("xray" if args.agent_tests else "xray-fd"))
        run("openssl", "req", "-x509", "-newkey", "ec", "-pkeyopt", "ec_paramgen_curve:P-256", "-noenc",
            "-keyout", inputs / "cover.key", "-out", inputs / "cover.pem", "-days", "1",
            "-subj", "/CN=cover.test", "-addext", "subjectAltName=DNS:cover.test")
        (inputs / "cover.key").chmod(0o600)
    token = uuid.uuid4().hex[:12]
    name = "brocade-hy2-netns-" + token
    manifest = {"mode": args.mode, "started": started, "network": "none",
                "workload_sha256": digest(inputs / "workload.py"),
                "hy2_workload_sha256": digest(inputs / "hy2.py"),
                "runner_sha256": digest(pathlib.Path(__file__)),
                "runtime_image": run("docker", "image", "inspect", "--format", "{{.Id}}", args.image)}
    manifest.update(entrypoint=entrypoint, entrypoint_sha256=digest(inputs / entrypoint))
    if args.docker_host_image:
        manifest.update(docker_rootfs_sha256=digest(inputs / "docker-rootfs.tar"),
                        production_workload_sha256=digest(inputs / "production.py"),
                        release_fixture_sha256=digest(inputs / "faults_fixture.py"))
    if args.agent_tests:
        manifest["agent_tests_sha256"] = digest(inputs / "agent-tests")
        if args.agent:
            manifest["release_agent_sha256"] = digest(inputs / "agent")
        for legacy_name in ["legacy-xray", "legacy-agent"]:
            if (inputs / legacy_name).exists():
                manifest[legacy_name + "_sha256"] = digest(inputs / legacy_name)
    if args.xray:
        manifest["xray_prototype_sha256"] = digest(inputs / ("xray" if args.agent_tests else "xray-fd"))
    launched = False
    try:
        if args.mode == "docker":
            run("docker", "run", "-d", "--init", "--name", name, "--network", "none",
                "--memory", "512m", "--pids-limit", "128", "--cap-drop", "ALL", "--cap-add", "SYS_ADMIN",
                "--cap-add", "NET_ADMIN", "--cap-add", "NET_RAW", "--cap-add", "SETUID", "--cap-add", "SETGID", "--cap-add", "DAC_OVERRIDE", "--security-opt", "apparmor=unconfined",
                "-e", "BROCADE_LAB_XRAY=" + ("/inputs/xray-fd" if args.xray else ""),
                "-v", f"{inputs}:/inputs:ro", "-v", f"{results}:/results", args.image,
                "python3", "-u", "/inputs/" + entrypoint, "/results")
            launched = True
        else:
            if args.vm_image is None or not pathlib.Path("/dev/kvm").exists() or digest(args.vm_image) != BASE_SHA256:
                raise RuntimeError("requires real /dev/kvm and the verified Ubuntu base image")
            manifest.update({"base_image_sha256": BASE_SHA256,
                             "qemu_image": run("docker", "image", "inspect", "--format", "{{.Id}}", args.qemu_image)})
            base = args.vm_image.resolve()
            vm = output / "vm"
            vm.mkdir()
            # Keep guest package installation offline and reproducible from the same
            # runtime image used by Docker. Never mutate the shared base image.
            package_container = name + "-packages"
            run("docker", "create", "--name", package_container, "--network", "none", args.image, "true")
            try:
                run("docker", "cp", package_container + ":/packages", str(inputs / "packages"))
            finally:
                run("docker", "rm", package_container)
            manifest["packages"] = {p.name: digest(p) for p in sorted((inputs / "packages").glob("*.deb"))}
            if len(manifest["packages"]) < 10:
                raise RuntimeError("offline package closure is missing")
            startup = """#!/bin/sh
set -eu
mkdir -p /lab
mount -t 9p -o trans=virtio,version=9p2000.L lab /lab
set +e
DEBIAN_FRONTEND=noninteractive dpkg -i /lab/inputs/packages/*.deb > /lab/results/packages.log 2>&1
code=$?
if [ "$code" = 0 ]; then
  BROCADE_LAB_XRAY=XRAY_PATH python3 -u /lab/inputs/ENTRYPOINT /lab/results > /lab/results/workload.log 2>&1
  code=$?
fi
printf '{"exit_code":%s}\\n' "$code" > /lab/results/exit.tmp
mv /lab/results/exit.tmp /lab/results/exit.json
sync
"""
            startup = startup.replace("XRAY_PATH", "/lab/inputs/xray-fd" if args.xray else "")
            startup = startup.replace("ENTRYPOINT", entrypoint)
            if args.docker_host_image:
                startup = startup.replace("set +e\n", "printf '#!/bin/sh\\nexit 101\\n' > /usr/sbin/policy-rc.d\nchmod 755 /usr/sbin/policy-rc.d\nset +e\n", 1)
            (vm / "user-data").write_text("#cloud-config\nwrite_files:\n  - path: /usr/local/bin/hy2-netns-lab\n    permissions: '0755'\n    content: |\n" + "".join("      " + line + "\n" for line in startup.splitlines()) + "runcmd:\n  - [sh, -c, '/usr/local/bin/hy2-netns-lab > /var/log/hy2-netns-lab.log 2>&1 &']\n")
            (vm / "meta-data").write_text("instance-id: " + name + "\n")
            (vm / "network-config").write_text("version: 2\nethernets: {}\n")
            run("docker", "run", "--rm", "--network", "none", "-v", f"{vm}:/vm", "-v", f"{base}:/base.img:ro",
                args.qemu_image, "qemu-img", "create", "-f", "qcow2", "-F", "qcow2", "-b", "/base.img", "/vm/root.qcow2", "8G")
            run("docker", "run", "--rm", "--network", "none", "-v", f"{vm}:/vm", args.qemu_image,
                "cloud-localds", "--network-config=/vm/network-config", "/vm/seed.img", "/vm/user-data", "/vm/meta-data")
            run("docker", "run", "-d", "--name", name, "--network", "none", "--device", "/dev/kvm",
                "--memory", "1400m", "--pids-limit", "256", "-v", f"{output}:/lab", "-v", f"{vm}:/vm",
                "-v", f"{base}:/base.img:ro", args.qemu_image,
                "qemu-system-x86_64", "-accel", "kvm", "-cpu", "host", "-smp", "2", "-m", "1024",
                "-display", "none", "-nic", "none", "-serial", "file:/lab/results/serial.log",
                "-qmp", "unix:/vm/qmp.sock,server=on,wait=off",
                "-drive", "file=/vm/root.qcow2,if=virtio,format=qcow2",
                "-drive", "file=/vm/seed.img,if=virtio,format=raw,readonly=on",
                "-fsdev", "local,id=lab,path=/lab,security_model=none", "-device", "virtio-9p-pci,fsdev=lab,mount_tag=lab")
            launched = True
            qmp = """import json,socket
s=socket.socket(socket.AF_UNIX); s.settimeout(5); s.connect('/vm/qmp.sock'); f=s.makefile('rwb',buffering=0)
json.loads(f.readline())
for command in ('qmp_capabilities','query-kvm'):
 f.write((json.dumps({'execute':command})+'\\n').encode())
 while True:
  response=json.loads(f.readline())
  if 'error' in response: raise RuntimeError(response)
  if 'return' in response: break
print(json.dumps(response['return']))
"""
            acceleration = json.loads(run("docker", "exec", name, "python3", "-c", qmp))
            if acceleration != {"enabled": True, "present": True}:
                raise RuntimeError("hardware KVM acceleration is not active: " + str(acceleration))
            manifest["query_kvm"] = acceleration
            print("KVM hardware acceleration verified", flush=True)
        (output / "manifest.json").write_text(json.dumps(manifest, indent=2) + "\n")
        deadline, progress = time.monotonic() + 420, time.monotonic()
        while True:
            if args.mode == "kvm" and (results / "exit.json").exists():
                if json.loads((results / "exit.json").read_text())["exit_code"]:
                    raise RuntimeError("guest workload failed; see retained logs")
                break
            if args.mode == "docker" and run("docker", "inspect", "--format", "{{.State.Running}}", name) == "false":
                if run("docker", "inspect", "--format", "{{.State.ExitCode}}", name) != "0":
                    raise RuntimeError("Docker workload failed; see retained logs")
                break
            if time.monotonic() >= deadline:
                raise TimeoutError("lab did not finish within 420 seconds")
            if time.monotonic() - progress >= 30:
                print(f"PROGRESS mode={args.mode} elapsed_seconds={time.monotonic() - start:.1f}", flush=True)
                progress = time.monotonic()
            time.sleep(1)
        result = json.loads((results / "result.json").read_text())
        expected_cases = 21 if args.docker_host_image else ((37 + (2 if args.legacy_xray else 0) + (1 if args.legacy_agent else 0)) if args.agent_tests else (30 if args.xray else 22))
        if not result["passed"] or len(result["cases"]) != expected_cases or result.get("collection_errors"):
            raise RuntimeError("incomplete or failed workload evidence")
        print(json.dumps({"mode": args.mode, "passed": True, "cases": len(result["cases"]),
                          "kernel": result["kernel"], "workload_elapsed_seconds": result["elapsed_seconds"]}), flush=True)
    finally:
        if launched:
            try:
                (results / "container.log").write_text(run("docker", "logs", name, timeout=15) + "\n")
            finally:
                run("docker", "rm", "-f", name, timeout=15)
        manifest.update({"ended": now(), "elapsed_seconds": round(time.monotonic() - start, 3)})
        (output / "manifest.json").write_text(json.dumps(manifest, indent=2) + "\n")


if __name__ == "__main__":
    main()
