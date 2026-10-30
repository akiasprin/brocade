#!/usr/bin/env python3
"""Export only anonymized live topology through an authorized read-only SSH session."""
import argparse
import json
import pathlib
import re
import subprocess

SQL = """BEGIN TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY;
WITH ns AS (SELECT id,'node-'||row_number() OVER(ORDER BY id) AS alias FROM nodes),
cs AS (SELECT id,'chain-'||row_number() OVER(ORDER BY id) AS alias FROM chains)
SELECT json_build_object(
'captured_at',now(),
'nodes',(SELECT json_agg(json_build_object('id',ns.alias,'overlay',n.overlay,
  'egress',n.egress_allowed,'mtu',n.mtu,'dns',n.dns_kind,
  'ipv6',n.public_ipv6 IS NOT NULL,'retired',n.retired_at IS NOT NULL))
  FROM nodes n JOIN ns USING(id)),
'ingresses',(SELECT json_agg(json_build_object('chain',cs.alias,'node',ns.alias,
  'transport',i.transport_kind,'hy2',i.hy2_enabled,'anytls',i.anytls_enabled,
  'anytls_security',i.anytls_security,'mtproto',i.mtproto_port IS NOT NULL))
  FROM ingresses i JOIN ns ON ns.id=i.node_id JOIN cs ON cs.id=i.chain_id),
'steps',(SELECT json_agg(json_build_object('chain',cs.alias,'node',ns.alias,
  'wire',s.hop_in_wire->>'t','rules',(SELECT json_agg(json_build_object(
    'match',r->'m'->>'t','action',r->'a'->>'t',
    'to',(SELECT alias FROM ns n2 WHERE n2.id=r->'a'->>'to'),
    'listener_node',(SELECT alias FROM ns n2 WHERE n2.id=r->'a'->'listener'->>'node'),
    'listener_chain',(SELECT alias FROM cs c2 WHERE c2.id=r->'a'->'listener'->>'chain'),
    'dial',r->'a'->'dial'->>'t','pool',r->'a'->'pool'->>'t',
    'address_family',CASE WHEN r->'a'->'dial'->>'t'='addr' THEN
      CASE WHEN r->'a'->'dial'->>'v' LIKE '[%' THEN 'v6' ELSE 'v4' END END))
    FROM jsonb_array_elements(s.rules) r)))
  FROM steps s JOIN ns ON ns.id=s.node_id JOIN cs ON cs.id=s.chain_id));
COMMIT;
"""


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--ssh", required=True, help="authorized user@console host")
    parser.add_argument("--database-file", default="/etc/brocade/database-url")
    parser.add_argument("--output", required=True, type=pathlib.Path)
    args = parser.parse_args()
    if args.ssh.startswith("-") or not re.fullmatch(r"[A-Za-z0-9_.@:\[\]-]+", args.ssh):
        parser.error("invalid SSH destination")
    if not pathlib.PurePosixPath(args.database_file).is_absolute():
        parser.error("--database-file must be an absolute remote path")
    remote = "DATABASE_FILE=" + repr(args.database_file) + "\nQUERY=" + repr(SQL) + "\n" + """from pathlib import Path
from urllib.parse import urlsplit,unquote
import subprocess,os
u=urlsplit(Path(DATABASE_FILE).read_text().strip())
env={**os.environ,'PGHOST':u.hostname or 'localhost','PGPORT':str(u.port or 5432),
     'PGUSER':unquote(u.username or ''),'PGPASSWORD':unquote(u.password or ''),
     'PGDATABASE':unquote(u.path.lstrip('/')),
     'PGOPTIONS':'-c default_transaction_read_only=on -c statement_timeout=10000'}
result=subprocess.run(['psql','-X','-q','-A','-t','-v','ON_ERROR_STOP=1'],
    input=QUERY,text=True,capture_output=True,env=env,timeout=20)
if result.returncode:raise RuntimeError('read-only structural query failed')
print(result.stdout)
"""
    result = subprocess.run(
        ["ssh", "-o", "BatchMode=yes", "-o", "StrictHostKeyChecking=yes", "-o", "ConnectTimeout=10",
         args.ssh, "python3 -"], input=remote, text=True, capture_output=True, timeout=30,
    )
    if result.returncode:
        raise RuntimeError("SSH read-only topology export failed")
    value = json.loads(result.stdout)
    args.output.parent.mkdir(parents=True, exist_ok=True)
    with args.output.open("x") as output:
        output.write(json.dumps(value, indent=2) + "\n")
    args.output.chmod(0o600)
    print(json.dumps({name: len(value[name] or []) for name in ("nodes", "ingresses", "steps")}))


if __name__ == "__main__":
    main()
