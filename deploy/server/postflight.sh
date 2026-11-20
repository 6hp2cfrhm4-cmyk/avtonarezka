#!/usr/bin/env bash
# Снимок после установки и сравнение с preflight (правило G7): любое расхождение вне avtonarezka* — нарушение.
# Изменения самого владельца сервера (его контейнеры, конфиги) этот скрипт не отличает от изменений проекта: сверяйте вручную.
#   ./postflight.sh [каталог-до] [каталог-после]
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
BEFORE="${1:-snapshot-before}"
AFTER="${2:-snapshot-after}"
[ -d "$BEFORE" ] || { echo "нет $BEFORE: сначала preflight.sh"; exit 2; }
"$HERE/preflight.sh" "$AFTER" >/dev/null

# Только то, что должно совпадать построчно; всё про avtonarezka выкидываем (это наше).
STABLE="listen listen-udp units unit-files docker-ps iptables users groups authkeys sudoers etc-hashes opt mounts nrestarts"
bad=0
norm() { # убираем то, что меняется само: счётчики iptables, «… назад» у контейнеров, размеры каталогов
  case "$1" in
    iptables) sed -E 's/\[[0-9]+:[0-9]+\]//' "$2" ;;
    listen|listen-udp) sed -E 's/pid=[0-9]+/pid=N/g; s/fd=[0-9]+/fd=N/g' "$2" ;;
    docker-ps) cut -f1,2 "$2" ;;
    sudoers) grep -v '^total' "$2" ;;
    authkeys) cat "$2" ;;
    *) cat "$2" ;;
  esac | grep -vE 'avtonarezka|session-[0-9]+\.scope|run-[a-z0-9-]+\.(scope|service)|user@[0-9]+'
}
for n in $STABLE; do
  [ "$n" = authkeys ] && continue   # хэш файла меняется из-за нашей строки; проверяется отдельно ниже
  d=$(diff <(norm "$n" "$BEFORE/$n.txt" 2>/dev/null) <(norm "$n" "$AFTER/$n.txt" 2>/dev/null) || true)
  [ -z "$d" ] && continue
  # uptime контейнеров меняется сам: сравниваем только имя и образ
  if [ "$n" = docker-ps ]; then
    d=$(diff <(cut -f1,2 "$BEFORE/docker-ps.txt") <(cut -f1,2 "$AFTER/docker-ps.txt") || true)
    [ -z "$d" ] && continue
  fi
  # NRestarts: лишь рост у чужих сервисов считается нарушением
  echo "РАСХОЖДЕНИЕ в $n:"; echo "$d"; bad=1
done
# authorized_keys: чужие строки не должны измениться
for f in /home/*/.ssh/authorized_keys; do
  [ -f "$f.avtonarezka-backup" ] && ! diff <(grep -v avtonarezka-managed "$f") "$f.avtonarezka-backup" >/dev/null && { echo "РАСХОЖДЕНИЕ: чужие строки в $f"; bad=1; }
done
echo "--- переменные величины ---"
for n in loadavg psi dmesg-oom; do echo "[$n] до: $(tr '\n' ' ' < "$BEFORE/$n.txt")"; echo "[$n] после: $(tr '\n' ' ' < "$AFTER/$n.txt")"; done
if [ $bad = 0 ]; then echo "ИТОГ: вне avtonarezka* ничего не изменилось"; else echo "ИТОГ: ЕСТЬ РАСХОЖДЕНИЯ — остановитесь и разберитесь"; fi
exit $bad
