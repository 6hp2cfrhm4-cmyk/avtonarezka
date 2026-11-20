#!/usr/bin/env bash
# Снимок состояния сервера ДО установки (только чтение). Результат — каталог со срезами, который
# потом сравнивает postflight.sh. Ничего не меняет, root не обязателен (часть данных без него неполна).
#   ./preflight.sh [каталог-снимка]     по умолчанию ./snapshot-before
set -uo pipefail
OUT="${1:-snapshot-before}"
mkdir -p "$OUT"
snap() { # имя команда...
  local name="$1"; shift
  "$@" > "$OUT/$name.txt" 2>&1 || true
}
snap listen        ss -tlnpH
snap listen-udp    ss -ulnpH
snap units         systemctl list-units --type=service --all --no-legend --no-pager
snap unit-files    systemctl list-unit-files --no-legend --no-pager
snap docker-ps     docker ps -a --format '{{.Names}}\t{{.Image}}\t{{.Status}}'
snap iptables      bash -c 'iptables-save 2>/dev/null | grep -v "^#"'
snap users         bash -c 'cut -d: -f1,3,6,7 /etc/passwd'
snap groups        bash -c 'cut -d: -f1,3 /etc/group'
snap authkeys      bash -c 'for f in /home/*/.ssh/authorized_keys /root/.ssh/authorized_keys; do [ -f "$f" ] && echo "$f $(sha256sum < "$f")"; done'
snap sudoers       bash -c 'ls -l /etc/sudoers.d; sha256sum /etc/sudoers 2>/dev/null'
snap etc-hashes    bash -c 'for d in nginx ssh fail2ban docker ufw; do [ -e /etc/$d ] && find /etc/$d -type f -exec sha256sum {} + 2>/dev/null | sort -k2; done'
snap opt           bash -c 'ls -1 /opt'
snap mounts        bash -c 'findmnt -rn -o TARGET,SOURCE,FSTYPE'
snap nrestarts     bash -c 'for u in $(systemctl list-units --type=service --state=running --no-legend --no-pager | awk "{print \$1}"); do echo "$u $(systemctl show -p NRestarts --value "$u")"; done'
# переменные величины (сравнивать построчно нельзя, нужны для сводки)
snap free          free -m
snap df            df -m /
snap loadavg       cat /proc/loadavg
snap psi           bash -c 'for r in cpu memory io; do echo "$r $(cat /proc/pressure/$r 2>/dev/null | head -1)"; done'
snap dmesg-oom     bash -c 'dmesg 2>/dev/null | grep -ci "out of memory" || true'
date -u +%FT%TZ > "$OUT/taken-at.txt"
echo "Снимок записан в $OUT"
