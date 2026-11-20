#!/usr/bin/env bash
# G9 (давление измеряется у чужих служб: system.slice/user.slice): нагрузочный прогон «2 эфира + 1 рендер» под наблюдением давления (PSI), памяти и отклика nginx.
# Всё работает внутри avtonarezka.slice с теми же лимитами, что у демона. Аварийный выход: PSI cpu > 25 % или
# доступной памяти < 600 МБ — всё останавливается немедленно. Каталог прогона удаляется в конце.
#   sudo ./loadtest.sh [секунд, по умолчанию 150]
set -uo pipefail
[ "$(id -u)" = 0 ] || { echo "нужен root (sudo)"; exit 1; }
DUR="${1:-150}"
FF=/opt/avtonarezka/bin/ffmpeg
WORK=/var/lib/avtonarezka/tmp/loadtest
UNITS="avto-lt-rec1 avto-lt-rec2 avto-lt-render"
cleanup() { for u in $UNITS; do systemctl stop "$u.service" 2>/dev/null; done; rm -rf "$WORK"; }
trap cleanup EXIT
rm -rf "$WORK"; install -d -o avtonarezka -g avtonarezka "$WORK" "$WORK/r1" "$WORK/r2"
common=(--quiet --collect --slice=avtonarezka.slice -p User=avtonarezka -p Nice=19 -p IOSchedulingClass=idle -p OOMScoreAdjust=1000 -p MemorySwapMax=0 -p ReadWritePaths="$WORK" -p PrivateTmp=true -p NoNewPrivileges=true)
probe() { # cpu_psi mem_avail_mb nginx_ms load1
  local cpu mem ms
  # давление у чужих служб: system.slice и user.slice (общий счётчик включает наши же упоры в лимиты)
  cpu=$(for f in /sys/fs/cgroup/system.slice/cpu.pressure /sys/fs/cgroup/user.slice/cpu.pressure; do awk '/^some/{for(i=1;i<=NF;i++) if($i ~ /^avg10=/){sub("avg10=","",$i); print $i}}' "$f"; done | sort -rn | head -1)
  ours=$(awk '/^some/{for(i=1;i<=NF;i++) if($i ~ /^avg10=/){sub("avg10=","",$i); print $i}}' /sys/fs/cgroup/avtonarezka.slice/cpu.pressure 2>/dev/null)
  mem=$(awk '/MemAvailable/{print int($2/1024)}' /proc/meminfo)
  ms=$(curl -s -o /dev/null -m 5 -w '%{time_total}' http://127.0.0.1/ 2>/dev/null || echo 9)
  echo "$cpu $mem $ms $(cut -d' ' -f1 /proc/loadavg) ${ours:-0}"
}
echo "== до нагрузки =="; probe
echo "готовлю тестовый эфир (720p, 5 Мбит/с, $((DUR+20)) с)"
systemd-run "${common[@]}" --wait --pipe -p MemoryMax=600M "$FF" -hide_banner -loglevel error -f lavfi -i testsrc2=s=1280x720:r=30 -f lavfi -i sine=f=300 \
  -t $((DUR+20)) -c:v libx264 -preset ultrafast -threads 1 -b:v 5M -g 60 -c:a aac "$WORK/src.mp4" || { echo "не удалось подготовить исходник"; exit 1; }
echo "пауза 20 с: давление от подготовки исходника не должно попасть в замеры"; sleep 20
echo "== перед запуском =="; probe
for n in 1 2; do
  systemd-run "${common[@]}" --unit="avto-lt-rec$n" -p MemoryMax=400M "$FF" -hide_banner -loglevel error -re -i "$WORK/src.mp4" -c copy -f hls -hls_time 4 -hls_list_size 300 -hls_flags delete_segments+program_date_time "$WORK/r$n/ring.m3u8"
done
systemd-run "${common[@]}" --unit=avto-lt-render -p MemoryMax=700M bash -c "while true; do $FF -hide_banner -loglevel error -y -i $WORK/src.mp4 -t 40 -vf 'scale=1080:1920:force_original_aspect_ratio=increase,crop=1080:1920' -c:v libx264 -preset veryfast -crf 23 -threads 1 -c:a aac $WORK/out.mp4; done"
echo "== под нагрузкой ($DUR с) =="
maxcpu=0; minmem=999999; maxms=0; maxload=0; abort=0
for ((t=0; t<DUR; t+=5)); do
  sleep 5
  read -r cpu mem ms load ours < <(probe)
  printf '%4ss чужие_psi=%s наш_psi=%s mem=%sМБ nginx=%sс load=%s\n' "$((t+5))" "$cpu" "$ours" "$mem" "$ms" "$load"
  maxcpu=$(awk -v a="$maxcpu" -v b="$cpu" 'BEGIN{print (b>a)?b:a}'); maxms=$(awk -v a="$maxms" -v b="$ms" 'BEGIN{print (b>a)?b:a}')
  maxload=$(awk -v a="$maxload" -v b="$load" 'BEGIN{print (b>a)?b:a}')
  [ "$mem" -lt "$minmem" ] && minmem=$mem
  if awk -v c="$cpu" 'BEGIN{exit !(c>25)}' || [ "$mem" -lt 600 ]; then echo "АВАРИЙНЫЙ ВЫХОД: давление или память за порогом"; abort=1; break; fi
done
for u in $UNITS; do systemctl is-active "$u.service" >/dev/null 2>&1 && echo "работает: $u" || echo "НЕ работает: $u"; done
echo "== итог =="
echo "макс. давление на чужие службы(avg10)=$maxcpu мин. память=${minmem}МБ макс. отклик nginx=${maxms}с макс. load1=$maxload"
echo "OOM в dmesg: $(dmesg 2>/dev/null | grep -ci 'out of memory\|oom-kill')"
verdict=0
awk -v c="$maxcpu" 'BEGIN{exit !(c<10)}' || { echo "ПРОВАЛ: давление на чужие службы >= 10 %"; verdict=1; }
awk -v c="$maxms" 'BEGIN{exit !(c<0.2)}' || { echo "ПРОВАЛ: отклик nginx >= 0,2 с"; verdict=1; }
[ "$abort" = 0 ] || verdict=1
[ $verdict = 0 ] && echo "ИТОГ: критерий G9 выполнен" || echo "ИТОГ: критерий G9 НЕ выполнен"
exit $verdict
