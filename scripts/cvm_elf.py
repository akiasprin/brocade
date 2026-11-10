"""Check the host ELF requirements of Cube build and package inputs."""
import os
from pathlib import Path
import re
import struct
import subprocess


DEFAULT_GLIBC = '2.35'


def validate(path, architecture, baseline):
    """Return the highest required GLIBC version, rejecting incompatible ELF."""
    if not re.fullmatch(r'[0-9]+\.[0-9]+', baseline):
        raise ValueError('invalid glibc baseline')
    path = Path(path)
    with path.open('rb') as stream:
        header = stream.read(20)
    machine = {'x86_64': 62, 'aarch64': 183}[architecture]
    if (len(header) < 20 or header[:4] != b'\x7fELF' or header[4:6] != b'\x02\x01'
            or struct.unpack_from('<H', header, 18)[0] != machine):
        raise ValueError(f'ELF architecture does not match {architecture}: {path}')
    output = subprocess.check_output(['readelf', '--version-info', '--wide', str(path)],
                                     env={**os.environ, 'LC_ALL': 'C'}, text=True, timeout=30)
    # Exported version definitions do not impose host requirements. Only the
    # version-needs section describes symbols imported from other libraries.
    needs = output.partition('Version needs section')[2]
    versions = set(re.findall(r'Name: (GLIBC_\S+)', needs))
    unsupported = sorted(version for version in versions
                         if not re.fullmatch(r'GLIBC_[0-9]+(?:\.[0-9]+)+', version))
    if unsupported:
        raise ValueError(f'unsupported glibc ABI requirements in {path}: {", ".join(unsupported)}')
    required = max((version.removeprefix('GLIBC_') for version in versions),
                   key=lambda value: tuple(map(int, value.split('.'))), default=None)
    if required and tuple(map(int, required.split('.'))) > tuple(map(int, baseline.split('.'))):
        raise ValueError(f'{path} requires GLIBC_{required}, exceeding declared glibc {baseline}; '
                         'rebuild this input with a compatible sysroot')
    return required
