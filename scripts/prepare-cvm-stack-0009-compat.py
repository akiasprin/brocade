#!/usr/bin/env python3
"""Prepare one offline conversion for the exact prerelease Cube Stack 0009."""

import argparse
import hashlib
from pathlib import Path
import re
import subprocess


ROOT = Path(__file__).resolve().parents[1]
MIGRATIONS = Path('crates/brocade-store/migrations')
SQL = Path(__file__).with_suffix('.sql')
BASELINE = 'a4b3ef9f61102b30d3f71b0fbeb302b1dce6cd5d'
OLD = 'c5bc5772a129de7a2af045c469bb0542584b744c6981e67295af6db6d5adc693e7987d15854b5427691cd6b51032d949'
TARGET = 'a23524bec53b5ed29f73fd6a8ab33284656442396057b3b4c9b81f0eac5267cb12c46303f1461f20115a471422ed308b'
BOUNDARY = b'CREATE TABLE cvm_stack_observations ('
BACKUP_TABLES = {
    '_sqlx_migrations',
    'nodes',
    'cvm_stack_observations',
    'cvm_stack_releases',
    'cvm_stack_release_events',
}


def git(*args: str) -> bytes:
    return subprocess.check_output(['git', '-C', str(ROOT), *args], timeout=30)


def migration_inputs() -> list[tuple[Path, bytes, bytes]]:
    names = git('ls-tree', '--name-only', f'{BASELINE}:{MIGRATIONS}').decode().splitlines()
    paths = [MIGRATIONS / name for name in names]
    if any(not re.fullmatch(r'[0-9]+_[a-z0-9_]+\.sql', path.name) for path in paths) or [
        int(path.name.split('_', 1)[0]) for path in paths
    ] != list(range(1, 10)):
        raise ValueError('unexpected baseline migration chain')
    return [(path, git('show', f'{BASELINE}:{path}'), (ROOT / path).read_bytes()) for path in paths]


def reference_schema(suffix: bytes) -> str:
    # Use the original DDL and PostgreSQL itself to derive the catalog definitions.
    sql = suffix.decode('utf-8')
    if sql.count('CREATE TABLE cvm_stack_') != 3 or sql.count('CREATE INDEX ') != 1:
        raise ValueError('unexpected Cube Stack schema in recognized migration')
    return sql.replace('CREATE TABLE cvm_stack_', 'CREATE TEMP TABLE cvm_stack_')


def generate(migrations: list[tuple[Path, bytes, bytes]], backup_sha256: str) -> str:
    if not re.fullmatch(r'[0-9a-f]{64}', backup_sha256):
        raise ValueError('invalid verified backup digest')
    old, target = migrations[-1][1:]
    if (
        hashlib.sha384(old).hexdigest() != OLD
        or hashlib.sha384(target).hexdigest() != TARGET
        or any(before != after for _, before, after in migrations[:-1])
        or old.count(BOUNDARY) != 1
        or target.count(BOUNDARY) != 1
        or old.split(BOUNDARY)[0] != target.split(BOUNDARY)[0]
    ):
        raise ValueError('unrecognized baseline or target migration bytes')
    history = ',\n    '.join(
        f"({version}, '{path.stem.split('_', 1)[1].replace('_', ' ')}', decode('{hashlib.sha384(before).hexdigest()}', 'hex'))"
        for version, (path, before, _) in enumerate(migrations, start=1)
    )
    return (
        SQL.read_text(encoding='utf-8')
        .replace('@BACKUP_SHA256@', backup_sha256)
        .replace('@HISTORY@', history)
        .replace('@OLD_SCHEMA@', reference_schema(BOUNDARY + old.split(BOUNDARY)[1]))
        .replace('@TARGET_SCHEMA@', reference_schema(BOUNDARY + target.split(BOUNDARY)[1]))
        .replace('@TARGET_SHA384@', TARGET)
    )


def verified_backup_digest(backup: Path, pg_restore: str) -> str:
    if not backup.is_file() or backup.stat().st_size == 0:
        raise ValueError('a nonempty verified rollback archive is required')
    # Keep archive contents out of logs. An operator must still have restored it
    # successfully and verified that it came from the quiesced target.
    with backup.open('rb') as stream:
        listing = subprocess.check_output([pg_restore, '--list'], stdin=stream, timeout=30)
        tables = set(re.findall(rb'^\d+; \d+ \d+ TABLE DATA public (\w+) ', listing, re.MULTILINE))
        if not {name.encode() for name in BACKUP_TABLES} <= tables:
            raise ValueError('rollback archive is missing required table data entries')
        stream.seek(0)
        return hashlib.file_digest(stream, 'sha256').hexdigest()


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--backup', required=True, type=Path, help='full pre-conversion pg_dump archive')
    parser.add_argument('--backup-verified', action='store_true', help='attest the archive was restored and verified')
    parser.add_argument('--pg-restore', default='pg_restore', help='path to the matching pg_restore executable')
    parser.add_argument('--output', required=True, type=Path, help='new SQL file (never overwrites)')
    args = parser.parse_args()
    if not args.backup_verified:
        parser.error('restore and verify the rollback archive before using --backup-verified')
    try:
        digest = verified_backup_digest(args.backup, args.pg_restore)
        sql = generate(migration_inputs(), digest)
        with args.output.open('x', encoding='utf-8') as stream:
            stream.write(sql)
    except (OSError, ValueError, subprocess.SubprocessError) as exc:
        parser.exit(status=1, message=f'conversion SQL not prepared: {exc}\n')


if __name__ == '__main__':
    main()
