"""Exercise installer file publication/recovery in disposable containers; systemctl is simulated."""
import base64
import os
from pathlib import Path
import subprocess
import unittest
import uuid


@unittest.skipUnless(os.environ.get('BROCADE_RUN_UPGRADE_INSTALLER_TESTS') == '1', 'requires Docker opt-in')
class InstallerUpgradeTests(unittest.TestCase):
    def test_installation_preservation_and_failure_boundaries(self):
        installer = Path(__file__).resolve().parents[3] / 'install.sh'
        for scenario in ['success', 'backup-failure', 'migration-failure', 'health-failure', 'health-failure-no-schema']:
            with self.subTest(scenario=scenario):
                container = 'brocade-upgrade-installer-' + uuid.uuid4().hex[:10]
                try:
                    subprocess.run(['docker', 'run', '-d', '--name', container, '--network', 'none',
                                    'brocade-forwarding-continuity:local', 'sleep', 'infinity'],
                                   capture_output=True, check=True, timeout=30)
                    source = SETUP.replace('@INSTALLER@', base64.b64encode(installer.read_bytes()).decode())
                    result = subprocess.run(['docker', 'exec', '-i', container, 'sh', '-s', '--', scenario],
                                            input=source, capture_output=True, text=True, timeout=90)
                    self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
                finally:
                    subprocess.run(['docker', 'rm', '-f', container], capture_output=True, timeout=30, check=False)


SETUP = r'''
set -eu
scenario=$1
mkdir -p /bundle /opt/brocade /etc/brocade /var/lib/brocade /run/systemd/system /etc/systemd/system
printf '%s\n' "$scenario" > /scenario
cat >/usr/local/bin/systemctl <<'EOF'
#!/bin/sh
printf '%s\n' "$*" >>/control.log
case $1 in
  is-active) exit 0 ;;
  is-failed) case $(cat /scenario) in health-failure*) exit 0;; *) exit 1;; esac ;;
esac
EOF
cat >/opt/brocade/brocade-launcher <<'EOF'
#!/bin/sh
printf 'brocade 0.3.1\n'
EOF
printf old-console >/opt/brocade/brocade-console
printf '#!/bin/sh\nexit 0\n' >/usr/local/bin/brocade
chmod 755 /usr/local/bin/systemctl /usr/local/bin/brocade /opt/brocade/brocade-launcher
printf 'x86_64-gnu\n' >/opt/brocade/BROCADE_PLATFORM
printf 'old-manifest\n' >/opt/brocade/SHA256SUMS
printf '# Managed by Brocade installer\n# preserve-custom-service-setting\n' >/etc/systemd/system/brocade.service
printf 'http://192.0.2.20:9001\n' >/etc/brocade/agent-public-url
printf '0.0.0.0:9001\n' >/etc/brocade/listen-address
printf 'external\n' >/etc/brocade/database-type
printf 'postgresql://fixture:fixture@127.0.0.1/brocade\n' >/etc/brocade/database-url
printf 'preserve-secret\n' >/var/lib/brocade/secret-key
printf '#!/bin/sh\nexit 0\n' >/usr/local/bin/pg_dump
cp /usr/local/bin/pg_dump /usr/local/bin/pg_restore
chmod 755 /usr/local/bin/pg_dump /usr/local/bin/pg_restore
cat >/bundle/brocade-launcher <<'EOF'
#!/bin/sh
set -eu
case $1 in
 --version) printf 'brocade 0.3.2\n';;
 upgrade-database)
    backup=
    check=false
    while [ "$#" -gt 0 ]; do
      case $1 in
        --backup-dir) backup=$2; shift;;
        --check-only) check=true;;
      esac
      shift
    done
    [ "$check" = false ] || exit 0
    scenario=$(cat /scenario)
    [ "$scenario" != backup-failure ] || exit 9
    printf 'fixture-dump' >"$backup/database.dump"
    [ "$scenario" = health-failure-no-schema ] || touch "$backup/migration-started"
    [ "$scenario" != migration-failure ] || exit 9
    touch "$backup/database-complete"
    ;;
 url)
    case $(cat /scenario) in health-failure*) exit 1;; esac
    [ "$BROCADE_AGENT_PUBLIC_URL" = http://192.0.2.20:9001 ]
    [ "$BROCADE_ADMIN_BIND" = 0.0.0.0:9001 ]
    printf '%s\n' "$BROCADE_AGENT_PUBLIC_URL"
    ;;
 *) exit 2;;
esac
EOF
chmod 755 /bundle/brocade-launcher
printf new-console >/bundle/brocade-console
printf 'x86_64-gnu\n' >/bundle/BROCADE_PLATFORM
printf '%s' '@INSTALLER@' | base64 -d >/bundle/install.sh
(cd /bundle; sha256sum brocade-launcher brocade-console BROCADE_PLATFORM install.sh >SHA256SUMS)
status=0
sh /bundle/install.sh --upgrade >/result.log 2>&1 || status=$?
if [ "$scenario" = success ]; then
    [ "$status" = 0 ] || { cat /result.log; exit 1; }
    cmp /opt/brocade/brocade-console /bundle/brocade-console
else
    [ "$status" != 0 ] || exit 1
fi
case $scenario in
 backup-failure|health-failure-no-schema)
    [ "$(cat /opt/brocade/brocade-console)" = old-console ]
    grep -qx 'start brocade.service' /control.log
    ;;
 migration-failure|health-failure)
    ! grep -qx 'start brocade.service' /control.log
    [ "$(tail -n 1 /control.log)" = 'stop brocade.service' ]
    ;;
esac
[ "$(cat /var/lib/brocade/secret-key)" = preserve-secret ]
[ "$(cat /etc/brocade/agent-public-url)" = http://192.0.2.20:9001 ]
[ "$(cat /etc/brocade/listen-address)" = 0.0.0.0:9001 ]
[ "$(cat /etc/brocade/database-type)" = external ]
grep -q preserve-custom-service-setting /etc/systemd/system/brocade.service
backup=$(find /var/backups/brocade-upgrades -mindepth 1 -maxdepth 1 -type d | head -n 1)
[ -s "$backup/installation.tar.gz" ]
[ -s "$backup/data.tar.gz" ]
gzip -t "$backup/installation.tar.gz" "$backup/data.tar.gz"
tar -tzf "$backup/installation.tar.gz" | grep -qx etc/brocade/database-url
printf '%s passed\n' "$scenario"
'''


if __name__ == '__main__':
    unittest.main()
