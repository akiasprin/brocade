#!/usr/bin/env python3
"""Verify Brocade custom contracts against explicit, frozen Xray artifacts."""
import argparse
import hashlib
import json
import os
import pathlib
import re
import shutil
import subprocess
import time
import uuid

SCENARIOS = ["TestBrocadeFrozen" + suffix for suffix in (
    "DNSRetryCacheAndProtocol", "DNSColdWarmDiagnosticsAndBudget", "DNSCanceledLeaderAndStaleRefresh",
    "SourceBansAndAttribution", "SiteStatisticsDoNotDuplicateRelay", "OtherDomainAndFailedTargetAttribution",
    "SS2022OversizedKeys", "SS2022RollingMultiUserCompatibility", "LegacyUDPHopAndGSO",
    "AnyTLSProtocolBoundaries", "AnyTLSIdlePoolLifecycle", "AnyTLSFINDeliversQueuedTail",
    "SourceWindowRetainsClosedTraffic", "DetailCheckpointAdmission", "ForwardingRemovalAndGuard",
    "SniffFailureSelectsRoute", "CertificateRotationKeepsConnections", "HTTPFrontendsIgnoreForgedSource",
    "TLSVerificationPolicy", "GeoIPFamilyRoutes", "UserRevocationIsolation", "HTTP3GenerationHandoff")]
SCENARIOS += ["TestMasque" + suffix for suffix in ("", "HTTP2", "Server", "ServerHTTP2", "ServerRejectsWrongPassword", "ServerClientToClient")]
SCENARIOS += [
    "TestQuotaRevocationRealProcessesKeepsOtherUserAndCounters",
    "TestVLESSReverseRealProcessConcurrencyAndRestart", "TestVLESSReverseRealProcessUDPDataPlane",
    "TestVLESSReverseRealProcessOneWayBlackholeRecovery", "TestVLESSReverseRealProcessPortalOutageBackoff",
    "TestVlessXtlsVisionRealityLocalCoverHTTPS"]
MACHINE = "traffic_limit_budget::real_xray_test::traffic_limit_real_xray_generations_share_one_budget_for_existing_tcp_and_udp"
USER = "user_traffic::tests::user_global_native_tcp_udp_forwarding_and_existing_sessions_share_budget"
MTPROXY_CASES = ["TestBrocadeFrozenMTProxy" + suffix for suffix in (
    "DirectoryRPCAccountingAndBan", "RevocationIsolation", "MustHonorMachineBudget", "MustHonorUserBudget")]


def digest(path):
    with path.open("rb") as source:
        return hashlib.file_digest(source, "sha256").hexdigest()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", required=True, type=pathlib.Path)
    parser.add_argument("--old-binary", required=True, type=pathlib.Path)
    parser.add_argument("--agent-tests", required=True, type=pathlib.Path)
    parser.add_argument("--output", required=True, type=pathlib.Path)
    parser.add_argument("--go", default="go")
    parser.add_argument("--docker-image", default="brocade-xray-topology:local")
    parser.add_argument("--require-ktls", action="store_true")
    args = parser.parse_args()
    root = pathlib.Path(__file__).resolve().parents[3]
    module = root / "components/xray-core"
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=False, mode=0o700)
    inputs = output / "inputs"
    logs = output / "logs"
    inputs.mkdir()
    logs.mkdir()
    for src, name in ((args.binary, "xray"), (args.old_binary, "xray-old"), (args.agent_tests, "agent-tests")):
        shutil.copy2(src.resolve(strict=True), inputs / name)
        (inputs / name).chmod(0o755)
    env = {**os.environ, "GOTOOLCHAIN": "go" + (module / ".go-version").read_text().strip(),
           "GOFLAGS": "-buildvcs=false", "GOMAXPROCS": "4", "BROCADE_XRAY_BIN": str(inputs / "xray"),
           "BROCADE_XRAY_OLD_BIN": str(inputs / "xray-old"), "BROCADE_E2E_OUTPUT": str(logs)}
    env["BROCADE_E2E_ASSETS"] = str(module / "resources")
    report = {"passed": False, "started_unix": int(time.time()), "artifacts": {p.name: digest(p) for p in inputs.iterdir()},
              "require_ktls": args.require_ktls, "cases": []}
    if args.require_ktls:
        env["BROCADE_REQUIRE_KTLS"] = "1"
    else:
        env.pop("BROCADE_REQUIRE_KTLS", None)

    def save():
        temporary = output / "report.tmp"
        temporary.write_text(json.dumps(report, indent=2) + "\n")
        temporary.replace(output / "report.json")

    def execute(label, command, expected=(), extra_env=None, timeout=360):
        path = logs / (label + ".log")
        started = time.monotonic()
        with path.open("w") as log:
            result = subprocess.run(command, cwd=module, env={**env, **(extra_env or {})}, stdout=log, stderr=log, timeout=timeout)
        text = path.read_text()
        if result.returncode or "--- SKIP:" in text or "0 passed;" in text:
            raise RuntimeError(f"{label} failed or skipped; inspect {path}")
        passed = set(re.findall(r"^--- PASS: (\S+)", text, re.MULTILINE))
        if not set(expected).issubset(passed):
            raise RuntimeError(f"{label} did not execute required cases: {set(expected) - passed}")
        report["cases"].append({"case": label, "passed": True, "seconds": round(time.monotonic() - started, 3),
                                "required_tests": list(expected), "log_sha256": digest(path)})
        save()
        print(json.dumps(report["cases"][-1]), flush=True)

    save()
    try:
        for package, name in (("./testing/scenarios", "scenarios.test"), ("./proxy/mtproto", "mtproxy.test")):
            execute("build-" + name, [args.go, "test", "-c", "-o", str(inputs / name), package])
            report["artifacts"][name] = digest(inputs / name)
        pattern = "^(" + "|".join(SCENARIOS) + ")$"
        execute("frozen-network-contracts", [str(inputs / "scenarios.test"), "-test.v", "-test.run", pattern, "-test.timeout=5m"], SCENARIOS)
        execute("machine-shared-budget", [str(inputs / "agent-tests"), "--exact", MACHINE, "--ignored", "--nocapture"], timeout=90)
        for splice in ("on", "off"):
            label = "user-machine-budget-splice-" + splice
            execute(label, [str(inputs / "agent-tests"), "--exact", USER, "--ignored", "--nocapture"],
                    extra_env={"BROCADE_E2E_ANYTLS_SPLICE": splice}, timeout=150)
            if "1 passed; 0 failed; 0 ignored" not in (logs / (label + ".log")).read_text():
                raise RuntimeError("shared user budget selection matched no test")
        # Only this disposable namespace has a synthetic public socket identity.
        # HTTPS proxying is confined to the test's local directory peer.
        container = "brocade-mtproxy-e2e-" + uuid.uuid4().hex[:12]
        try:
            execute("mtproxy-directory-rpc", ["docker", "run", "--rm", "--name", container, "--network", "none",
                    "--cap-drop", "ALL", "--cap-add", "NET_ADMIN", "--cap-add", "DAC_OVERRIDE", "--memory", "512m",
                    "--cpus", "2", "--pids-limit", "128", "--read-only", "--tmpfs", "/tmp:rw,size=64m",
                    "-v", str(inputs) + ":/lab:ro", args.docker_image, "sh", "-ec",
                    "ip link set lo up\nip addr add 198.51.100.2/32 dev lo\n"
                    "BROCADE_XRAY_BIN=/lab/xray /lab/mtproxy.test -test.v -test.run '^(" + "|".join(MTPROXY_CASES) + ")$' -test.timeout=45s"], MTPROXY_CASES, timeout=60)
        finally:
            subprocess.run(["docker", "rm", "-f", container], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=15)
        report["passed"] = True
    finally:
        report["finished_unix"] = int(time.time())
        save()


if __name__ == "__main__":
    main()
