#!/usr/bin/env python3
"""Actual firewall services and kernel WireGuard; no substitute rulesets."""
import json
from pathlib import Path


def configure_firewall(kind, run):
    ports=('20000-20199/udp','30000-30199/udp','28080/tcp','28081/udp','51820/udp')
    if kind=='firewalld':
        run('systemctl','start','dbus')
        run('systemctl','start','firewalld')
        for port in ports:run('firewall-cmd','--permanent','--zone=public','--add-port='+port)
        run('firewall-cmd','--reload')
        return {'version':run('firewall-cmd','--version'),'state':run('firewall-cmd','--state'),
                'zones':run('firewall-cmd','--list-all-zones')}
    assert kind=='ufw'
    run('ufw','--force','reset')
    run('ufw','default','deny','incoming')
    run('ufw','default','allow','outgoing')
    for port in ports:run('ufw','allow',port.replace('-',':'))
    run('ufw','--force','enable')
    return {'version':run('ufw','version'),'state':run('ufw','status','verbose')}


def wg_key(run):
    private=run('wg','genkey')
    path=Path('/var/tmp/cluster-wg-private')
    path.touch(mode=0o600,exist_ok=False);path.write_text(private+'\n')
    return run('wg','pubkey',input=private+'\n')


def wireguard(args,run):
    host=args['host'];peer=2 if host==1 else 1
    run('ip','link','add','wg-lab','type','wireguard')
    run('wg','set','wg-lab','listen-port','51820','private-key','/var/tmp/cluster-wg-private',
        'peer',args['peer_key'],'allowed-ips',f'10.200.0.{peer}/32,fd42:bc:200::{peer}/128',
        'endpoint',f'198.18.3.{peer}:51820','persistent-keepalive','5')
    run('ip','addr','add',f'10.200.0.{host}/24','dev','wg-lab')
    run('ip','-6','addr','add',f'fd42:bc:200::{host}/64','dev','wg-lab','nodad')
    run('ip','link','set','wg-lab','mtu','1420','up')
    return {'addresses':json.loads(run('ip','-j','addr','show','dev','wg-lab')),
            'endpoints':run('wg','show','wg-lab','endpoints')}
