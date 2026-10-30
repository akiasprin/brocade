import contextlib, hashlib, ipaddress, json, socket, socketserver, struct, subprocess, threading, time
from pathlib import Path
import sys, os, ssl, signal
def deadline(signum, frame): raise TimeoutError("connection provenance matrix exceeded 180 seconds")
signal.signal(signal.SIGALRM, deadline)
signal.alarm(180)
ROOT=Path(sys.argv[2])
BIN=Path(sys.argv[1]).resolve()
COUNTS={}; LOCK=threading.Lock()
def port():
 with socket.socket() as s:s.bind(('127.0.0.1',0));return s.getsockname()[1]
def read(s,n):
 out=b''
 while len(out)<n:
  b=s.recv(n-len(out))
  if not b:raise EOFError()
  out+=b
 return out
class DNS(socketserver.BaseRequestHandler):
 def handle(self):
  data,s=self.request;i=12;labels=[]
  while data[i]:n=data[i];labels.append(data[i+1:i+1+n].decode());i+=n+1
  i+=1;typ,cls=struct.unpack('!HH',data[i:i+4]);domain='.'.join(labels)
  with LOCK:
   count=COUNTS.get((self.server.role,domain,typ),0)+1;COUNTS[(self.server.role,domain,typ)]=count
  if getattr(self.server,'drop_all',False) or count % getattr(self.server,'reply_every',1):return
  value=None
  if typ==1 and self.server.has_a:value=ipaddress.ip_address({'policy':'127.0.0.1','machine':'127.0.0.2','secondary':'127.0.0.3','entry':'127.0.0.4'}[self.server.role]).packed
  if typ==28 and self.server.has_aaaa:value=ipaddress.ip_address('::1').packed
  reply=data[:2]+struct.pack('!HHHHH',0x8180,1,int(value is not None),0,0)+data[12:i+4]
  if value:reply+=b'\xc0\x0c'+struct.pack('!HHIH',typ,1,60,len(value))+value
  s.sendto(reply,self.client_address)
class TCPDNS(socketserver.BaseRequestHandler):
 def handle(self):
  # Not used by the Xray config: UDP packet counts are the resolver evidence.
  pass
class HTTP(socketserver.BaseRequestHandler):
 def handle(self):
  self.request.settimeout(4)
  self.request=TLS_SERVER.wrap_socket(self.request,server_side=True)
  data=b''
  while b'\r\n\r\n' not in data:
   block=self.request.recv(1024)
   if not block:return
   data+=block
  body=self.server.marker.encode();self.request.sendall(b'HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Length: '+str(len(body)).encode()+b'\r\n\r\n'+body)
class HTTP6(socketserver.ThreadingTCPServer):address_family=socket.AF_INET6
class Server(socketserver.ThreadingUDPServer):daemon_threads=True
class H4(socketserver.ThreadingTCPServer):daemon_threads=True
@contextlib.contextmanager
def server(cls,address,handler,**kw):
 s=cls(address,handler)
 for k,v in kw.items():setattr(s,k,v)
 t=threading.Thread(target=s.serve_forever,daemon=True);t.start()
 try:yield s
 finally:s.shutdown();s.server_close();t.join()
@contextlib.contextmanager
def xray(name,cfg):
 p=ROOT/(name+'.json');p.write_text(json.dumps(cfg));log=open(ROOT/(name+'.log'),'w')
 env=os.environ.copy()
 for key in ['XRAY_BROCADE_PROBE_NODE','XRAY_BROCADE_PROBE_IDENTITY_FILE','XRAY_BROCADE_PROBE_FILE','XRAY_BROCADE_PROBE_PROFILE']:env.pop(key,None)
 if name.endswith('-client'):env['XRAY_BROCADE_PROBE_FILE']=str(ROOT/(name+'.connections'))
 else:
  identity=ROOT/(name+'.node-id');identity.write_text('other' if name.endswith('-other') else 'mo' if name.endswith('-mo') else 'entry');identity.chmod(0o600)
  env['XRAY_BROCADE_PROBE_IDENTITY_FILE']=str(identity)
  direct=[o for o in cfg['outbounds'] if o['protocol']=='freedom']
  projected={'dns':cfg.get('dns'),'outbounds':direct}
  fingerprint=hashlib.sha256(json.dumps(projected,sort_keys=True,separators=(',',':')).encode()).hexdigest()
  strategies={o['tag']:o.get('settings',{}).get('domainStrategy','AsIs') for o in direct if not o['tag'].startswith('probe:') and not o['tag'].endswith((':probe-v4',':probe-v6'))}
  env['XRAY_BROCADE_PROBE_PROFILE']=json.dumps({'fingerprint':fingerprint,'strategies':strategies})
 proc=subprocess.Popen([str(BIN),'run','-config',str(p)],env=env,stdout=log,stderr=log)
 try:
  deadline=time.monotonic()+3
  while True:
   if proc.poll() is not None:raise RuntimeError(f'{name} Xray exited')
   try:
    with socket.create_connection(('127.0.0.1',cfg['inbounds'][0]['port']),timeout=.1):break
   except OSError:
    if time.monotonic()>deadline:raise
    time.sleep(.02)
  yield
 finally:proc.terminate();proc.wait(timeout=4);log.close()
def connect(socks,host,dest):
 s=socket.create_connection(('127.0.0.1',socks),timeout=3);s.settimeout(10);s.sendall(b'\x05\x01\x00');assert read(s,2)==b'\x05\x00'
 h=host.encode();s.sendall(b'\x05\x01\x00\x03'+bytes([len(h)])+h+struct.pack('!H',dest));h=read(s,4)
 if h[1]!=0:s.close();raise OSError('SOCKS rejected')
 n={1:4,4:16}.get(h[3]);n=n if n else read(s,1)[0];read(s,n+2);return s
def http(socks,host,dest):
 try:
  with TLS_CLIENT.wrap_socket(connect(socks,host,dest),server_hostname=host) as s:
   s.sendall(f'GET / HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n'.encode());data=b''
   while True:
    b=s.recv(4096)
    if not b:break
    data+=b
   return data.split(b'\r\n\r\n')[-1].decode() if b'\r\n\r\n' in data else 'NO_CONNECTION'
 except (OSError,EOFError):return 'NO_CONNECTION'
def counts(host):
 with LOCK:return {r:{'A':COUNTS.get((r,host,1),0),'AAAA':COUNTS.get((r,host,28),0)} for r in ['policy','secondary','machine']}

UUID=['00000000-0000-4000-8000-000000000001','00000000-0000-4000-8000-000000000002','00000000-0000-4000-8000-000000000003']
LABELS=['ordinary','probe#v4:fixture','probe#v6:fixture']
HOSTS=['www.youtube.com','redirector.googlevideo.com']
def inbound(ingress):
 return {'listen':'127.0.0.1','port':ingress,'protocol':'vless','settings':{'decryption':'none','clients':[{'id':uid,'email':label} for uid,label in zip(UUID,LABELS)]},'sniffing':{'enabled':True,'destOverride':['http','tls'],'routeOnly':True}}
def relay(tag,ingress,index):
 return {'tag':tag,'protocol':'vless','settings':{'vnext':[{'address':'127.0.0.1','port':ingress,'users':[{'id':UUID[index],'encryption':'none'}]}]}}
def run_case(name,strategy,family,stop,has_a,has_aaaa,pp,mp,sp,ep,hp,secondary=False,mixed=False,parallel=False,racing=False,retry=False):
 with LOCK:COUNTS.clear()
 mo,other,entry=port(),port(),port();normal=port();groups={'webpage':port(),'premium':port(),'cdn':port()}
 path=ROOT/(name+'-client.connections');path.write_bytes(b'');path.chmod(0o600)
 servers=[{'address':'127.0.0.1','port':pp,'domains':['domain:youtube.com','domain:googlevideo.com'],'tag':'dns:egress:shared','queryStrategy':strategy,'skipFallback':True,'finalQuery':stop and not racing,'disableCache':retry}]
 if racing:
  servers.append({**servers[0],'port':sp,'finalQuery':stop})
 if secondary:servers.append({'address':'127.0.0.1','port':sp,'domains':['domain:youtube.com','domain:googlevideo.com'],'tag':'dns:egress:second','queryStrategy':'UseIP','skipFallback':True})
 servers.append({'address':'127.0.0.1','port':mp})
 def exit_config(ingress):
  return {'log':{'loglevel':'debug'},'dns':{'servers':servers,'enableParallelQuery':racing or retry},'inbounds':[inbound(ingress)],'outbounds':[{'tag':'out:forward','protocol':'freedom','settings':{'domainStrategy':'UseIP'}},{'tag':'out:forward:probe-v4','protocol':'freedom','settings':{'domainStrategy':'ForceIPv4'}},{'tag':'out:forward:probe-v6','protocol':'freedom','settings':{'domainStrategy':'ForceIPv6'}}],'routing':{'rules':[{'type':'field','user':[LABELS[1]],'outboundTag':'out:forward:probe-v4'},{'type':'field','user':[LABELS[2]],'outboundTag':'out:forward:probe-v6'}]}}
 entry_cfg={'log':{'loglevel':'debug'},'dns':{'servers':[{'address':'127.0.0.1','port':ep}]},'inbounds':[inbound(entry)],'outbounds':[{'tag':'out:forward','protocol':'freedom','settings':{'domainStrategy':'UseIP'}}]+[relay(label+'-'+node,ingress,i) for node,ingress in [('mo',mo),('other',other)] for i,label in enumerate(['normal','v4','v6'])],'routing':{'rules':[{'type':'field','domain':['full:'+host],'user':[LABELS[i]],'outboundTag':label+'-'+('other' if mixed and host==HOSTS[1] else 'mo')} for host in HOSTS for i,label in enumerate(['normal','v4','v6'])]}}
 client={'log':{'loglevel':'error'},'inbounds':[{'tag':'normal','listen':'127.0.0.1','port':normal,'protocol':'socks','settings':{'auth':'noauth'}}]+[{'tag':'probe:request:youtube_premium.'+part,'listen':'127.0.0.1','port':value,'protocol':'socks','settings':{'auth':'noauth'}} for part,value in groups.items()],'outbounds':[relay('normal',entry,0),relay('selected',entry,1 if family=='v4' else 2)],'routing':{'rules':[{'type':'field','inboundTag':['probe:request:youtube_premium.'+part for part in groups],'outboundTag':'selected'}]}}
 with xray(name+'-mo',exit_config(mo)),xray(name+'-other',exit_config(other)),xray(name+'-entry',entry_cfg),xray(name+'-client',client):
  assert http(normal,'example.org',hp)=='ENTRY_DEFAULT'
  assert path.read_bytes()==b'', 'ordinary traffic produced diagnostic evidence'
  cold=[]
  if strategy=='UseIPv4' and family=='v6':
   body=http(groups['cdn'],HOSTS[1],hp)
   assert body=='IPV6_ENDPOINT', f'cold diagnostics must preserve a successful IPv6 fallback: {body}'
   assert COUNTS.get(('policy',HOSTS[1],1),0)==1, 'cold diagnostics must confirm the configured resolver with a real A lookup'
   cold=[('cdn',HOSTS[1],body)]
  diagnostic_before=path.read_bytes()
  baseline=http(normal,HOSTS[0],hp)
  assert path.read_bytes()==diagnostic_before, 'ordinary traffic produced diagnostic evidence'
  before={role:COUNTS.get((role,HOSTS[0],1 if family=='v4' else 28),0) for role in ['machine','secondary']}
  outcomes=cold.copy()
  for part,host in [('webpage',HOSTS[0]),('premium',HOSTS[0]),('cdn',HOSTS[1])]:outcomes.append((part,host,http(groups[part],host,hp)))
  if parallel:
   workers=[]
   for i in range(24):
    thread=threading.Thread(target=lambda:outcomes.append(('webpage',HOSTS[0],http(groups['webpage'],HOSTS[0],hp))))
    workers.append(thread);thread.start()
   for thread in workers:thread.join()
  rows=[json.loads(line) for line in path.read_text().splitlines()]
  starts={r['id']:r for r in rows if r.get('started')};finished=[r for r in rows if not r.get('started')]
  assert len(starts)==len(outcomes)==len(finished), (name,len(starts),len(outcomes),len(finished))
  assert len({r['id'] for r in finished})==len(finished)
  fallback=(strategy=='UseIPv6' and family=='v4') or (strategy=='UseIPv4' and family=='v6') or (strategy=='UseIP' and ((family=='v4' and not has_a) or (family=='v6' and not has_aaaa)))
  for record in finished:
   assert record['version']==2 and record['id'] in starts and record['domain']==starts[record['id']]['domain']
   assert record['exit']==('other' if mixed and record['domain']==HOSTS[1] else 'mo'), record
   assert record['complete'] and record['profile'] and record['normal_strategy']=='UseIP', record
   assert len(record['lookups'])==1 and record['lookups'][0]['domain']==record['domain']
   steps=record['lookups'][0]['steps']
   if fallback:
    assert steps[0]['outcome']=='empty' and steps[0]['other'], 'cold and warm diagnostics must agree'
    if stop:
     assert not record['connected'] and record['error']=='dns_empty' and len(steps)==1, record
    else:
     assert record['connected'] and record['family']==family and len(steps)==2
     assert steps[-1]['outcome']=='answer' and steps[-1]['scoped']==secondary
   else:assert record['connected'] and record['family']==family and len(steps)==1 and steps[0]['outcome']=='answer'
   if racing:
    assert steps[0]['winner']==2 and any(m['index']==2 and m['outcome']=='answer' for m in steps[0]['members']),record
   if retry:
    assert steps[0]['winner']==1 and steps[0]['members'][0]['attempts']==3,record
  if stop and not racing:
   assert all(body=='NO_CONNECTION' for part,host,body in outcomes)
   assert COUNTS.get(('machine',HOSTS[0],1 if family=='v4' else 28),0)==before['machine']
  else:
   expected='IPV6_ENDPOINT' if family=='v6' else 'SECONDARY_IPV4' if racing or fallback and secondary else 'MACHINE_IPV4' if fallback else 'POLICY_IPV4'
   assert all(body==expected for part,host,body in outcomes),outcomes
   if fallback:assert COUNTS.get(('secondary' if secondary else 'machine',HOSTS[0],1 if family=='v4' else 28),0)>before['secondary' if secondary else 'machine']
  result={'case':name,'normal':baseline,'family':family,'fallback':fallback,'stop':stop,'connections':len(finished),'actual_exits':sorted({r['exit'] for r in finished}),'records':finished}
  print(json.dumps({k:v for k,v in result.items() if k!='records'}),flush=True)
  return result

TLS_SERVER=None
TLS_CLIENT=ssl.SSLContext(ssl.PROTOCOL_TLS_CLIENT)
TLS_CLIENT.check_hostname=False
TLS_CLIENT.verify_mode=ssl.CERT_NONE

def main():
 global TLS_SERVER
 # The self-signed certificate is used only by isolated loopback test endpoints.
 cert,key=ROOT/'endpoint.crt',ROOT/'endpoint.key'
 subprocess.run(['openssl','req','-x509','-newkey','rsa:2048','-nodes','-keyout',str(key),'-out',str(cert),'-days','1','-subj','/CN=www.youtube.com'],check=True,stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)
 key.chmod(0o600)
 TLS_SERVER=ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER);TLS_SERVER.load_cert_chain(cert,key)
 pp,mp,sp,ep=port(),port(),port(),port();results=[]
 with server(Server,('127.0.0.1',pp),DNS,role='policy',has_a=True,has_aaaa=True) as policy,server(Server,('127.0.0.1',mp),DNS,role='machine',has_a=True,has_aaaa=True),server(Server,('127.0.0.1',sp),DNS,role='secondary',has_a=True,has_aaaa=True),server(Server,('127.0.0.1',ep),DNS,role='entry',has_a=True,has_aaaa=False),server(H4,('127.0.0.1',0),HTTP,marker='POLICY_IPV4') as h4:
  hp=h4.server_address[1]
  with server(H4,('127.0.0.2',hp),HTTP,marker='MACHINE_IPV4'),server(H4,('127.0.0.3',hp),HTTP,marker='SECONDARY_IPV4'),server(H4,('127.0.0.4',hp),HTTP,marker='ENTRY_DEFAULT'),server(HTTP6,('::1',hp),HTTP,marker='IPV6_ENDPOINT'):
   cases=[('v6-policy-v4-fallback','UseIPv6','v4',False,True,True,False,False,False),('v4-policy-v6-fallback','UseIPv4','v6',False,True,False,False,False,False),('useip-aaaa-only','UseIP','v4',False,False,True,False,False,False),('dual-stack','UseIP','v4',False,True,True,False,False,False),('v6-policy-stop-v4','UseIPv6','v4',True,True,True,False,False,False),('later-custom-policy','UseIPv6','v4',False,True,True,True,False,False),('identical-policy-two-exits','UseIPv6','v4',False,True,True,False,True,False),('parallel-correlated','UseIPv6','v4',False,True,True,False,True,True)]
   for name,strategy,family,stop,has_a,has_aaaa,secondary,mixed,parallel in cases:
    policy.has_a=has_a;policy.has_aaaa=has_aaaa
    results.append(run_case(name,strategy,family,stop,has_a,has_aaaa,pp,mp,sp,ep,hp,secondary,mixed,parallel))
   policy.has_a=True;policy.has_aaaa=True;policy.drop_all=True
   for stop in [False,True]:
    results.append(run_case('racing-stop-'+str(stop),'UseIPv4','v4',stop,True,True,pp,mp,sp,ep,hp,racing=True))
   policy.drop_all=False;policy.reply_every=3
   results.append(run_case('udp-retry-third-send','UseIPv4','v4',False,True,True,pp,mp,sp,ep,hp,retry=True))
 (ROOT/'simulation-results.json').write_text(json.dumps({'passed':True,'xray_sha256':hashlib.sha256(BIN.read_bytes()).hexdigest(),'results':results},indent=2));print('PASS: actual connection provenance matrix')
main()
