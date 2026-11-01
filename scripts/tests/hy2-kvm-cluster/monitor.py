#!/usr/bin/env python3
"""Bounded Docker egress observations, including reload gaps and recovery."""
import datetime
import json
from pathlib import Path
import subprocess
import sys
import time

ROOT=Path('/lab/results/node')
COUNT=ROOT/'docker-monitor-count.txt'
initial=int(COUNT.read_text()) if COUNT.exists() else 0
data={'started_at':datetime.datetime.now(datetime.timezone.utc).isoformat(),'attempts':0,'successes':0,'failed_requests':0,
      'failures':[],'recoveries':[],'last_success_at':None,'last_failure_at':None,'last_request_passed':None}
start=time.monotonic()
interval=float(sys.argv[1]) if len(sys.argv)>1 else .5
assert .05<=interval<=1
last_failure_tick=None
for i in range(4800):
    stamp=datetime.datetime.now(datetime.timezone.utc).isoformat();tick=time.monotonic()
    try:
        result=subprocess.run(['docker','--host','unix:///run/lab-docker.sock','exec','cluster-echo','/echo','egress','http://198.18.3.2:8080/'],capture_output=True,text=True,timeout=10)
        ok=result.returncode==0 and result.stdout=='cluster-target-ok';code=result.returncode;error=result.stderr[-1000:]
    except subprocess.TimeoutExpired:
        ok=False;code=None;error='Docker egress request exceeded 10 seconds'
    previous=data['last_request_passed'];data['attempts']+=1;data['last_request_passed']=ok
    if ok:
        data['successes']+=1;data['last_success_at']=stamp
        if previous is False and len(data['recoveries'])<128:data['recoveries'].append({'first_success_at':stamp,'seconds_since_last_failed_request':tick-last_failure_tick})
    else:
        data['failed_requests']+=1;data['last_failure_at']=stamp
        last_failure_tick=tick
        if len(data['failures'])<128:data['failures'].append({'started_at':stamp,'relative_seconds':time.monotonic()-start,'duration_seconds':time.monotonic()-tick,'exit_code':code,'error':error})
    data['elapsed_seconds']=time.monotonic()-start
    tmp=ROOT/'docker-monitor-observations.tmp';tmp.write_text(json.dumps(data,indent=2));tmp.replace(ROOT/'docker-monitor-observations.json')
    COUNT.write_text(str(initial+i+1))
    time.sleep(interval)
