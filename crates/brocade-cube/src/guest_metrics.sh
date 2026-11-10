set -eu
printf 'boot '; cat /proc/sys/kernel/random/boot_id
printf 'uptime '; cut -d ' ' -f 1 /proc/uptime
awk '/^cpu / {print "cpu",$2,$3,$4,$5,$6,$7,$8,$9;exit}' /proc/stat
awk '/^MemTotal:/ {print "mem_total",$2} /^MemAvailable:/ {print "mem_available",$2}' /proc/meminfo
LC_ALL=C df -Pk / | awk 'NR==2 {print "root",$(NF-4),$(NF-3)}'
awk '/^cpu[0-9]+ / && n++<32 {id=$1;sub(/^cpu/,"",id);print "core",id,$2,$3,$4,$5,$6,$7,$8,$9}' /proc/stat
awk '{print "load",$1,$2,$3;split($4,p,"/");print "processes",p[1],p[2]}' /proc/loadavg
awk '/^(MemFree|Cached|Buffers|AnonPages|SReclaimable|Slab|Dirty|Writeback|SwapTotal|SwapFree|Shmem|Unevictable):/ {key=$1;sub(/:$/,"",key);print "memory_"key,$2}' /proc/meminfo
awk '/^(pswpin|pswpout|pgmajfault|oom_kill|pgscan_direct|pgsteal_direct) / {print "vm_"$1,$2}' /proc/vmstat
for kind in cpu memory io; do
  [ ! -r /proc/pressure/$kind ] || awk -v k="$kind" '{sub(/^avg10=/,"",$2);print "pressure_"k"_"$1,$2}' /proc/pressure/$kind
done
LC_ALL=C df -Pi / 2>/dev/null | awk 'NR==2 {print "inode",$(NF-4),$(NF-2)}' || true
rootdev=$(findmnt -nr -o MAJ:MIN -T / 2>/dev/null || true)
[ -z "$rootdev" ] || awk -v dev="$rootdev" '$1":"$2==dev {print "diskio",$3,$4,$8,$6,$10,$7,$11,$13,$14,$12}' /proc/diskstats
awk '/^Tcp:/ {if(!nt++){for(i=2;i<=NF;i++)kt[i]=$i}else{for(i=2;i<=NF;i++)if(kt[i]~/^(CurrEstab|RetransSegs|EstabResets|AttemptFails|InSegs|OutSegs|ActiveOpens|PassiveOpens|InErrs|OutRsts)$/)print "tcp_"kt[i],$i}} /^Udp:/ {if(!nu++){for(i=2;i<=NF;i++)ku[i]=$i}else{for(i=2;i<=NF;i++)if(ku[i]~/^(InErrors|RcvbufErrors|SndbufErrors|NoPorts)$/)print "tcp_Udp"ku[i],$i}}' /proc/net/snmp
for p in /sys/class/net/*; do
  [ -r "$p/ifindex" ] || continue
  printf 'net %s ' "${p##*/}"
  cat "$p/ifindex" "$p/statistics/rx_bytes" "$p/statistics/tx_bytes" | tr '\n' ' '
  printf '\n'
  printf 'net_detail_%s ' "${p##*/}"
  for key in rx_packets tx_packets rx_dropped tx_dropped rx_errors tx_errors; do cat "$p/statistics/$key" 2>/dev/null || printf 'unknown\n'; done | tr '\n' ' '
  cat "$p/carrier" 2>/dev/null || printf 'unknown\n'
  printf '\n'
done
