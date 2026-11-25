"""Actual Guest traffic under automatic host-wide connection protection."""

import shlex


def verify(args, prefix, created, call, guest, wait_instance, record, primary):
    workspace = call('GET', '/cvm')
    host = next(h for h in workspace['hosts'] if h['node_id'] == created['node_id'])
    capacity = host['capabilities'].get('network_quota')
    if not capacity or capacity['total_states'] <= capacity['system_reserved_states']:
        raise RuntimeError('host has no verified allocatable connection capacity')
    assert 'quotas' not in workspace and 'network_quotas' not in workspace
    assert guest('~/cvm-probe tcp4 ' + shlex.quote(primary) + ' host-primary') == 'host-primary'
    assert guest('~/cvm-probe tcp4 203.0.113.10:9090 host-business') == 'host-business'
    record(
        'automatic host capacity protects initialized primary and business NICs without user quota setup; actual TCP works on both paths'
    )
