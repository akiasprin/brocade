#!/usr/bin/env python3
"""Destructive drills guarded to an explicitly marked, disposable KVM guest."""
import base64
import hashlib
import http.cookiejar
import json
import os
from pathlib import Path
import re
import shutil
import stat
import subprocess
import time
import traceback
import urllib.request
import uuid

ROOT = Path('/lab')
RESULTS = ROOT / 'results'
DATA = Path('/var/lib/brocade')
BACKUPS = Path('/var/backups/brocade-upgrades')
STATE = Path('/var/lib/brocade-upgrade-drill.json')
PASSWORD = 'Drill-' + uuid.uuid4().hex
DB_PASSWORD = uuid.uuid4().hex + '@drill'
EXPECTED_DATA = None
HTTP = urllib.request.build_opener(urllib.request.HTTPCookieProcessor(http.cookiejar.CookieJar()))
PORT = 19081
CHECKSUM = '794044a4f1b7ead02b8102e21bc7f94d3c01f309216a5e15ada747c61de528c04b494a46e993f683a7d256aa37cd0947'


def clean(text):
    text = text.replace(PASSWORD, '<redacted>').replace(DB_PASSWORD, '<redacted>')
    text = re.sub(r'postgres(?:ql)?://\S+', '<database-url>', text)
    return re.sub(r'(#/initialize/)[^\s]+', r'\1<redacted>', text)


def cmd(*args, check=True, timeout=300, input=None, env=None):
    result = subprocess.run(args, input=input, env=env, text=True, capture_output=True, timeout=timeout)
    if check and result.returncode:
        raise RuntimeError(f'{Path(args[0]).name}: {clean(result.stdout[-2000:] + result.stderr[-4000:])}')
    return result


def record(case, **values):
    path = RESULTS / 'evidence.json'
    records = json.loads(path.read_text()) if path.exists() else []
    records.append({'case': case, 'time': time.time(), **values})
    temporary = path.with_suffix('.tmp')
    temporary.write_text(json.dumps(records, indent=2) + '\n')
    temporary.replace(path)


def sha(path):
    with Path(path).open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()


def file_snapshot(path):
    metadata = Path(path).stat()
    return {'sha256': sha(path), 'mode': stat.S_IMODE(metadata.st_mode),
            'uid': metadata.st_uid, 'gid': metadata.st_gid}


def http(path, payload=None, token=None):
    body = json.dumps(payload).encode() if payload is not None else None
    headers = {'Content-Type': 'application/json'}
    if token:
        headers['Authorization'] = 'Bearer ' + token
    request = urllib.request.Request(f'http://127.0.0.1:{PORT}' + path, data=body, headers=headers)
    with HTTP.open(request, timeout=15) as response:
        return json.load(response)


def wait_ready():
    for _ in range(90):
        try:
            return http('/auth/state')
        except (OSError, ValueError):
            time.sleep(1)
    raise RuntimeError('Console did not become ready')


def active():
    return cmd('systemctl', 'is-active', '--quiet', 'brocade.service', check=False).returncode == 0


def sql(mode, statement):
    if mode == 'managed':
        env = {**os.environ, 'PGPASSWORD': (DATA / 'postgresql/password').read_text().strip()}
        args = ['runuser', '-u', 'brocade', '--', 'psql', '-h', str(DATA / 'postgresql/run'),
                '-U', 'postgres', '-d', 'brocade']
    else:
        env = None
        args = ['runuser', '-u', 'postgres', '--', 'psql', '-d', 'brocade']
    return cmd(*args, '-XqAt', '-v', 'ON_ERROR_STOP=1', input=statement, env=env).stdout.strip()


def receipts(mode):
    return sql(mode, "SELECT string_agg(version::text,',' ORDER BY version) FROM _sqlx_migrations")


def install(bundle, mode, upgrade=False, label='install', env=None, success=True):
    args = ['sh', str(bundle / 'install.sh')]
    if upgrade:
        args.append('--upgrade')
    else:
        args += ['--public-ip', '192.0.2.20', '--port', str(PORT)]
        if mode == 'managed':
            args += ['--managed-db']
        else:
            from urllib.parse import quote
            args += ['--database-url', f'postgresql://postgres:{quote(DB_PASSWORD, safe="")}@127.0.0.1/brocade']
    result = cmd(*args, check=False, timeout=900, env=env)
    (RESULTS / (label + '.log')).write_text(clean(result.stdout + result.stderr))
    assert (result.returncode == 0) == success, f'{label}: unexpected exit {result.returncode}; inspect log'
    return result


def reset():
    cmd('systemctl', 'stop', 'brocade.service', check=False)
    cmd('systemctl', 'disable', 'brocade.service', check=False)
    for path in ['/opt/brocade', '/etc/brocade', '/etc/systemd/system/brocade.service.d',
                 '/var/lib/brocade', '/var/backups/brocade-upgrades']:
        shutil.rmtree(path, ignore_errors=True)
    Path('/etc/systemd/system/brocade.service').unlink(missing_ok=True)
    cmd('systemctl', 'daemon-reload')
    cmd('systemctl', 'reset-failed')
    cmd('runuser', '-u', 'postgres', '--', 'dropdb', '--if-exists', 'brocade')
    # Do not remove the verified managed engine cache between independent fixtures.


def seed_upgrade_data(mode):
    sql(mode, """
        INSERT INTO tenants(id,name) VALUES('platform.upgrade','Upgrade fixture');
        INSERT INTO users(tenant_id,id,uuid) VALUES
            ('platform.upgrade','upgrade-user','00000000-0000-4000-8000-000000000031');
        INSERT INTO apps(id,label,position)
            SELECT 'upgrade-app','Upgrade app',COALESCE(max(position)+1,0) FROM apps;
        INSERT INTO nodes(id,tenant_id,name,public_ipv4,overlay_addr,wg_private_key,wg_public_key,
                          wg_listen_port,api_port,overlay,egress_allowed,dns_kind)
            VALUES('upgrade-node','platform.upgrade','Upgrade node','192.0.2.31','10.66.31.1',
                   'upgrade-fixture-private','upgrade-fixture-public',51820,10085,false,true,'system');
        INSERT INTO user_app_quotas(tenant_id,user_id,app_id,limit_bytes)
            VALUES('platform.upgrade','upgrade-user','upgrade-app',100000000);
        INSERT INTO forwarding_entitlements(tenant_id,user_id,node_id,updated_by)
            VALUES('platform.upgrade','upgrade-user','upgrade-node','upgrade-fixture');
        INSERT INTO forwarding_rules(tenant_id,user_id,node_id,name,protocol,listen_port,target_host,target_port)
            VALUES('platform.upgrade','upgrade-user','upgrade-node','Upgrade rule','tcp',30031,'example.net',443);
        INSERT INTO forwarding_node_quotas(tenant_id,user_id,node_id,limit_bytes,updated_by)
            VALUES('platform.upgrade','upgrade-user','upgrade-node',200000000,'upgrade-fixture');
        INSERT INTO usage_rollups(tenant_id,user_id,app_id,period_start,period_end,uplink_bytes,downlink_bytes)
            VALUES('platform.upgrade','upgrade-user','upgrade-app',date_trunc('month',now()),
                   date_trunc('month',now())+interval '1 month',1024,2048);
        INSERT INTO forwarding_usage_rollups(period_start,node_id,tenant_id,user_id,rule_id,uplink_bytes,downlink_bytes,has_gap)
            SELECT date_trunc('month',now()),'upgrade-node','platform.upgrade','upgrade-user',id,4096,8192,false
            FROM forwarding_rules WHERE node_id='upgrade-node';
        WITH generation AS (INSERT INTO usage_generations(node_id) VALUES('upgrade-node') RETURNING id)
        INSERT INTO usage_counter_heads(node_id,label,xray_epoch,read_at,xray_started_at,uplink_bytes,
                                        downlink_bytes,generation_id,agent_instance_id,sequence)
            SELECT 'upgrade-node','upgrade-fixture','upgrade-epoch',now(),now()-interval '1 hour',
                   1024,2048,id,'upgrade-instance',7 FROM generation;
        INSERT INTO forwarding_usage_counter_heads(node_id,rule_id,agent_instance_id,xray_epoch,
                                                   uplink_bytes,downlink_bytes,read_at,sequence)
            SELECT 'upgrade-node',id,'upgrade-instance','upgrade-epoch',4096,8192,now(),11
            FROM forwarding_rules WHERE node_id='upgrade-node';
    """)


def data_snapshot(mode):
    # Compare the old public fields exactly; 0002 adds operational policy defaults, not new usage.
    return json.loads(sql(mode, """SELECT jsonb_build_object(
        'cluster',(SELECT system_identifier FROM pg_control_system()),
        'postgres_version',current_setting('server_version'),
        'user',(SELECT to_jsonb(u)-ARRAY['traffic_reset_day','service_starts_at','service_expires_at','service_suspended']
                FROM users u WHERE tenant_id='platform.upgrade' AND id='upgrade-user'),
        'access_quota',(SELECT to_jsonb(q)-ARRAY['exhausted_action','rate_bps','exhausted','revision']
                        FROM user_app_quotas q WHERE tenant_id='platform.upgrade'),
        'machine_quota',(SELECT to_jsonb(q) FROM forwarding_node_quotas q WHERE node_id='upgrade-node'),
        'rule',(SELECT to_jsonb(q) FROM forwarding_rules q WHERE node_id='upgrade-node'),
        'access_usage',(SELECT to_jsonb(q) FROM usage_rollups q WHERE tenant_id='platform.upgrade'),
        'forwarding_usage',(SELECT to_jsonb(q) FROM forwarding_usage_rollups q WHERE node_id='upgrade-node'),
        'access_counter',(SELECT to_jsonb(q) FROM usage_counter_heads q WHERE node_id='upgrade-node'),
        'forwarding_counter',(SELECT to_jsonb(q) FROM forwarding_usage_counter_heads q WHERE node_id='upgrade-node'))"""))


def make_fixture(version, mode):
    global EXPECTED_DATA
    reset()
    if mode == 'external':
        cmd('runuser', '-u', 'postgres', '--', 'psql', '-Xq', '-v', 'ON_ERROR_STOP=1',
            input=f"ALTER USER postgres PASSWORD '{DB_PASSWORD}'; CREATE DATABASE brocade;")
    old = Path('/opt/drill') / version
    old.mkdir(parents=True, exist_ok=True)
    cmd('tar', '-xzf', str(ROOT / f'inputs/{version}.tar.gz'), '--strip-components=1', '-C', str(old))
    assert cmd(str(old / 'brocade-launcher'), '--version').stdout.strip() == 'brocade 0.3.1'
    install(old, mode, label=f'{version}-{mode}-old-install')
    wait_ready()
    assert receipts(mode) == '1'
    token = (DATA / 'bootstrap-token').read_text().strip()
    http('/auth/init', {'operator_id': 'drill-admin', 'display_name': 'Upgrade Drill', 'password': PASSWORD}, token)
    name = version + '-' + mode
    sql(mode, f"UPDATE control_state SET site_name='{name}';")
    seed_upgrade_data(mode)
    EXPECTED_DATA = data_snapshot(mode)
    # Sentinel data has a private random key and an explicit restrictive mode in both DB modes.
    sentinel = DATA / 'upgrade-drill-key'
    sentinel.write_bytes(os.urandom(32))
    sentinel.chmod(0o600)
    cmd('chown', 'brocade:brocade', str(sentinel))
    dropin = Path('/etc/systemd/system/brocade.service.d')
    dropin.mkdir()
    dropin_text = '[Service]\nTimeoutStopSec=75s\n'
    external_secret = Path('/etc/brocade/upgrade-drill.env')
    if mode == 'external':
        external_secret.write_text('BROCADE_SECRET_KEY=' + base64.b64encode(os.urandom(32)).decode() + '\n')
        external_secret.chmod(0o600)
        dropin_text += f'EnvironmentFile={external_secret}\n'
    (dropin / 'drill.conf').write_text(dropin_text)
    cmd('systemctl', 'daemon-reload')
    cmd('systemctl', 'restart', 'brocade.service')
    wait_ready()
    preserved = ['/etc/brocade/agent-public-url', '/etc/brocade/listen-address',
                 '/etc/brocade/database-type', '/etc/systemd/system/brocade.service',
                 str(dropin / 'drill.conf'), str(sentinel)]
    if mode == 'external':
        preserved += ['/etc/brocade/database-url', str(external_secret)]
    elif (DATA / 'secret-key').exists():
        preserved += [str(DATA / 'secret-key')]
    return old, name, {p: file_snapshot(p) for p in preserved}


def verify(mode, name, preserved, expected='1,2'):
    assert active()
    assert wait_ready()['initialized']
    assert http('/auth/login', {'operator_id': 'drill-admin', 'password': PASSWORD})['admin']
    assert receipts(mode) == expected
    assert sql(mode, 'SELECT site_name FROM control_state') == name
    assert sql(mode, "SELECT encode(checksum,'hex') FROM _sqlx_migrations WHERE version=1") == CHECKSUM
    assert sql(mode, 'SELECT count(*) FROM binary_releases') == '0'
    assert all(file_snapshot(p) == original for p, original in preserved.items()), 'private file bytes, permissions or ownership changed'
    assert data_snapshot(mode) == EXPECTED_DATA, 'user identity, quotas or usage changed across upgrade/restore'
    assert http('/quotas')['quotas'], 'preserved quota must remain readable through the real API'
    version = '0.3.1' if expected == '1' else (ROOT / 'inputs/candidate-version').read_text().strip()
    assert cmd('/opt/brocade/brocade-launcher', '--version').stdout.strip() == 'brocade ' + version


def latest_backup():
    return max(BACKUPS.iterdir(), key=lambda p: p.stat().st_mtime_ns)


def restore(mode, backup, label):
    cmd('systemctl', 'stop', 'brocade.service')
    shutil.rmtree(DATA)
    cmd('tar', '-C', '/', '-xzpf', str(backup / 'installation.tar.gz'))
    cmd('tar', '-C', '/var/lib', '-xzpf', str(backup / 'data.tar.gz'))
    if mode == 'external':
        cmd('runuser', '-u', 'postgres', '--', 'dropdb', 'brocade')
        cmd('runuser', '-u', 'postgres', '--', 'createdb', 'brocade')
        # Root-only backup paths remain private; send dump contents through stdin to the DB owner.
        with (backup / 'database/database.dump').open('rb') as stream:
            restored = subprocess.run(['runuser', '-u', 'postgres', '--', 'pg_restore',
                                       '--exit-on-error', '--dbname', 'brocade'],
                                      stdin=stream, capture_output=True, timeout=180)
        assert restored.returncode == 0, clean(restored.stderr.decode()[-2000:])
    cmd('systemctl', 'daemon-reload')
    cmd('systemctl', 'reset-failed', 'brocade.service')
    cmd('systemctl', 'start', 'brocade.service')
    wait_ready()
    record(label, database=mode, old_version=cmd('/opt/brocade/brocade-launcher', '--version').stdout.strip())


def faults_before(mode, candidate, name, preserved):
    sql(mode, "UPDATE _sqlx_migrations SET checksum=decode(repeat('00',48),'hex') WHERE version=1")
    install(candidate, mode, True, 'unknown-checksum', success=False)
    assert active() and receipts(mode) == '1'
    sql(mode, f"UPDATE _sqlx_migrations SET checksum=decode('{CHECKSUM}','hex') WHERE version=1")
    sql(mode, "INSERT INTO _sqlx_migrations(version,description,success,checksum,execution_time) VALUES(9999,'future',true,decode('00','hex'),0)")
    install(candidate, mode, True, 'newer-database', success=False)
    assert active()
    sql(mode, 'DELETE FROM _sqlx_migrations WHERE version=9999')
    dump = Path('/usr/bin/pg_dump')
    dump.rename('/usr/bin/pg_dump.drill-saved')
    try:
        install(candidate, mode, True, 'missing-client', success=False)
        assert active()
    finally:
        Path('/usr/bin/pg_dump.drill-saved').rename(dump)
    faultbin = Path('/opt/drill/faultbin')
    faultbin.mkdir(exist_ok=True)
    (faultbin / 'tar').write_text('#!/bin/sh\ncase "$*" in *-czpf*) exit 73;; esac\nexec /usr/bin/tar "$@"\n')
    (faultbin / 'tar').chmod(0o755)
    install(candidate, mode, True, 'compressed-backup-failure',
            env={**os.environ, 'PATH': str(faultbin) + ':' + os.environ['PATH']}, success=False)
    verify(mode, name, preserved, '1')
    record('pre-migration-failures', cases=['unknown-checksum', 'newer-database', 'missing-client', 'compressed-backup-failure'], old_service_usable=True)


def failed_migration(mode, candidate, name, preserved):
    sql(mode, 'CREATE TABLE node_traffic_limit (injected BOOLEAN)')
    install(candidate, mode, True, 'ddl-conflict', success=False)
    assert not active()
    backup = latest_backup()
    assert (backup / 'database/migration-started').exists()
    assert not (backup / 'database/database-complete').exists()
    assert receipts(mode) == '1'
    assert sql(mode, "SELECT count(*) FROM information_schema.columns WHERE table_name='forwarding_node_state' AND column_name='rule_results'") == '0'
    sql(mode, 'DROP TABLE node_traffic_limit')
    cmd('systemctl', 'start', 'brocade.service')
    verify(mode, name, preserved, '1')
    record('real-ddl-failure', transaction_rolled_back=True, stopped=True, backup_retained=True)


def health_failure(mode, candidate, name, preserved, migrated):
    guard = Path('/etc/systemd/system/brocade.service.d/fail-start.conf')
    guard.write_text('[Service]\nExecStartPre=/usr/bin/false\nRestart=no\n')
    cmd('systemctl', 'daemon-reload')
    before = sha('/opt/brocade/brocade-console')
    previous_launcher = sha('/opt/brocade/brocade-launcher')
    # For no-schema rollback, use different candidate bytes (a wrapper around the real launcher)
    # to make restoring the old installation observable, without simulating DB commands.
    bundle = candidate
    if not migrated:
        bundle = Path('/opt/drill/no-schema-candidate')
        shutil.copytree(candidate, bundle)
        shutil.copy2(bundle / 'brocade-launcher', '/opt/drill/real-launcher')
        (bundle / 'brocade-launcher').write_text('#!/bin/sh\nif [ "${1-}" = up ]; then exit 74; fi\nexec /opt/drill/real-launcher "$@"\n')
        (bundle / 'brocade-launcher').chmod(0o755)
        # A unit-level guard would also prevent the restored healthy program from starting.
        guard.unlink()
        cmd('systemctl', 'daemon-reload')
        (bundle / 'SHA256SUMS').write_text(''.join(f'{sha(bundle / p)}  {p}\n' for p in
            ['brocade-console', 'brocade-launcher', 'BROCADE_PLATFORM', 'install.sh']))
    install(bundle, mode, True, 'health-failure-' + str(migrated), success=False)
    if migrated:
        assert not active() and (latest_backup() / 'database/migration-started').exists()
    else:
        assert active() and sha('/opt/brocade/brocade-console') == before
        assert sha('/opt/brocade/brocade-launcher') == previous_launcher
    guard.unlink(missing_ok=True)
    cmd('systemctl', 'daemon-reload')
    cmd('systemctl', 'reset-failed', 'brocade.service')
    if migrated:
        cmd('systemctl', 'start', 'brocade.service')
    verify(mode, name, preserved)
    record('post-switch-start-failure', schema_changed=migrated, stopped_until_repair=migrated,
           old_installation_restored=not migrated)


def main():
    global PASSWORD, EXPECTED_DATA
    assert os.geteuid() == 0
    assert (ROOT / 'inputs/DISPOSABLE_UPGRADE_DRILL').read_text().strip() == 'KVM-only'
    assert cmd('systemd-detect-virt').stdout.strip() == 'kvm'
    modes = ['external', 'managed']
    next_mode = 0
    if STATE.exists():
        saved = json.loads(STATE.read_text())
        PASSWORD = saved['password']
        EXPECTED_DATA = saved['expected_data']
        assert saved['boot_id'] != Path('/proc/sys/kernel/random/boot_id').read_text().strip()
        assert active() and wait_ready()['initialized']
        assert receipts(saved['mode']) == '1,2'
        assert sql(saved['mode'], 'SELECT site_name FROM control_state') == saved['name']
        verify(saved['mode'], saved['name'], saved['preserved'])
        record('real-kvm-reboot', database=saved['mode'], boot_changed=True, service_enabled=True,
               original_data_readable=True, login=True)
        next_mode = saved['next_mode']
        if next_mode == len(modes):
            (RESULTS / 'result.json').write_text(json.dumps({'passed': True, 'runtime': 'KVM/systemd',
                'versions': ['v0.3.1'], 'databases': ['managed17.11', 'external16'],
                'production_changes': False, 'agent_xray_releases_created': False}, indent=2) + '\n')
            return
    else:
        record('environment', os=Path('/etc/os-release').read_text(), kernel=cmd('uname', '-r').stdout.strip(),
               systemd=cmd('systemctl', '--version').stdout.splitlines()[0])
    candidate = ROOT / 'inputs/candidate'
    version = 'v0.3.1'
    for index, mode in enumerate(modes[next_mode:], start=next_mode):
        old, name, preserved = make_fixture(version, mode)
        record('old-version-running', version=version, database=mode)
        if version == 'v0.3.1' and mode == 'external':
            faults_before(mode, candidate, name, preserved)
            failed_migration(mode, candidate, name, preserved)
        install(candidate, mode, True, f'{version}-{mode}-upgrade')
        verify(mode, name, preserved)
        assert sha('/opt/brocade/brocade-console') == sha(candidate / 'brocade-console')
        assert sha('/opt/brocade/brocade-launcher') == sha(candidate / 'brocade-launcher')
        backup = latest_backup()
        cmd('gzip', '-t', str(backup / 'installation.tar.gz'), str(backup / 'data.tar.gz'))
        if mode == 'external':
            toc = cmd('pg_restore', '--list', str(backup / 'database/database.dump')).stdout
            assert any('Compression:' in line and ('gzip' in line or '6' in line) for line in toc.splitlines())
        record('forward-upgrade', version=version, database=mode, login=True, config_preserved=True,
               checksums_preserved=True, compressed_backups=True, fleet_releases=0,
               user_quotas_usage_and_counter_heads_preserved=True, same_postgres_cluster=True)
        restore(mode, backup, f'{version}-{mode}-restore')
        verify(mode, name, preserved, '1')
        assert cmd('/opt/brocade/brocade-launcher', '--version').stdout.strip() == 'brocade ' + version[1:]
        record('backup-restored-old-console', version=version, database=mode, login=True)
        if mode == 'managed':
            # Exercise the actual systemd failure after schema changed; then repair and retry.
            health_failure(mode, candidate, name, preserved, migrated=True)
        else:
            install(candidate, mode, True, f'{version}-{mode}-upgrade-again')
        install(candidate, mode, True, f'{version}-{mode}-repeat')
        verify(mode, name, preserved)
        assert not (latest_backup() / 'database/migration-started').exists()
        record('idempotent-repeat', version=version, database=mode, receipts='1,2')
        if version == 'v0.3.1' and mode == 'external':
            health_failure(mode, candidate, name, preserved, migrated=False)
        STATE.write_text(json.dumps({'mode': mode, 'next_mode': index + 1, 'name': name, 'password': PASSWORD,
            'expected_data': EXPECTED_DATA, 'preserved': preserved,
            'boot_id': Path('/proc/sys/kernel/random/boot_id').read_text().strip()}))
        STATE.chmod(0o600)
        record('reboot-requested', database=mode)
        cmd('systemctl', 'reboot')
        return


if __name__ == '__main__':
    try:
        main()
    except Exception:
        if (ROOT / 'inputs/DISPOSABLE_UPGRADE_DRILL').exists():
            (RESULTS / 'failure.txt').write_text(clean(traceback.format_exc()))
            (RESULTS / 'service.log').write_text(clean(cmd('journalctl', '-u', 'brocade.service',
                '-n', '150', '--no-pager', check=False).stdout))
        raise
