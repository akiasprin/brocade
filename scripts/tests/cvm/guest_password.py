"""Use managed Guest passwords without putting secrets in command arguments."""

import os
import re


def prepare(call, output, instance, upload):
    identity = instance['desired']['id']
    if not re.fullmatch(r'[A-Za-z0-9_.-]{1,96}', identity):
        raise ValueError('invalid Guest identity')
    credential = call('GET', '/cvm/' + identity + '/credential')
    if credential['instance_id'] != identity or credential['revision'] != instance['desired']['spec']['login_revision']:
        raise RuntimeError('Guest password is not the approved version')
    password = credential['password']
    if not re.fullmatch(r'[A-Za-z0-9_-]{32}', password):
        raise ValueError('invalid generated Guest password')
    directory = output / 'guest-credentials'
    directory.mkdir(mode=0o700, exist_ok=True)
    files = [
        ('guest.password-' + identity, password + '\n', False),
        ('guest.askpass', '#!/bin/sh\nexec cat -- "$BROCADE_GUEST_PASSWORD_FILE"\n', True),
        ('guest.revision-' + identity, str(credential['revision']) + '\n', False),
    ]
    for name, content, executable in files:
        path = directory / name
        descriptor = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_TRUNC | os.O_NOFOLLOW, 0o600)
        os.fchmod(descriptor, 0o600)
        with os.fdopen(descriptor, 'w') as stream:
            stream.write(content)
        upload(path, name, executable)


def validate_cached(output, instance, revision):
    """Use only an explicitly pinned, previously approved test password after fault injection."""
    identity = instance['desired']['id']
    if not re.fullmatch(r'[A-Za-z0-9_.-]{1,96}', identity):
        raise ValueError('invalid Guest identity')
    directory = output / 'guest-credentials'
    if revision != instance['desired']['spec']['login_revision']:
        raise RuntimeError('cached Guest password revision does not match the instance')
    if (directory / ('guest.revision-' + identity)).read_text().strip() != str(revision):
        raise RuntimeError('cached Guest password is not the pinned approved version')
    if not re.fullmatch(r'[A-Za-z0-9_-]{32}\n', (directory / ('guest.password-' + identity)).read_text()):
        raise ValueError('invalid cached Guest password')


def ssh(prefix, identity):
    return [
        'env',
        'SSH_ASKPASS=' + prefix + '/guest.askpass',
        'SSH_ASKPASS_REQUIRE=force',
        'DISPLAY=brocade',
        'BROCADE_GUEST_PASSWORD_FILE=' + prefix + '/guest.password-' + identity,
        'setsid',
        '-w',
        'ssh',
        '-o',
        'PreferredAuthentications=password',
        '-o',
        'PubkeyAuthentication=no',
    ]
