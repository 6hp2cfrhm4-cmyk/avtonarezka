#!/usr/bin/env bash
# Полный откат установки: удаляет только то, что создал install.sh. Данные (образ диска) остаются,
# пока не указан --purge. Идемпотентен.
#   sudo ./uninstall.sh [--purge] [--yes] [--dry-run]
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
# shellcheck source=lib.sh
. "$HERE/lib.sh"
PURGE=0
while [ $# -gt 0 ]; do
  case "$1" in
    --purge) PURGE=1; shift ;;
    --yes) ASSUME_YES=1; shift ;;
    --login-user) LOGIN_USER="$2"; shift 2 ;;
    --dry-run) DRY=1; shift ;;
    *) die "неизвестный параметр: $1" ;;
  esac
done
need_root
confirm "Удалить установку Автонарезки (purge=$PURGE)?" || exit 0

sysrun systemctl disable --now avtonarezka-daemon.service 2>/dev/null || true
sysrun systemctl stop avtonarezka.slice 2>/dev/null || true
# строка ключа: удаляем только помеченную, у любого пользователя, где она есть
if [ -n "$LOGIN_USER" ]; then files=("$(R /home/$LOGIN_USER)/.ssh/authorized_keys"); else files=("$(R /home)"/*/.ssh/authorized_keys); fi
for AUTH_KEYS in "${files[@]}"; do
  if [ -f "$AUTH_KEYS" ] && grep -qF "avtonarezka-managed" "$AUTH_KEYS"; then
    if [ "$DRY" = 1 ]; then say "  [сухо] убрать строку ключа из $AUTH_KEYS"; else
      cp -p "$AUTH_KEYS" "$AUTH_KEYS.avtonarezka-backup"
      grep -vF "avtonarezka-managed" "$AUTH_KEYS" > "$AUTH_KEYS.tmp" || true
      cat "$AUTH_KEYS.tmp" > "$AUTH_KEYS"; rm -f "$AUTH_KEYS.tmp"
    fi
  fi
done
run rm -f "$SUDOERS"
run rm -f "$UNITDIR/avtonarezka-daemon.service" "$UNITDIR/avtonarezka.slice"
if [ "$PURGE" = 1 ]; then
  sysrun systemctl disable --now var-lib-avtonarezka.mount 2>/dev/null || true
  run rm -f "$UNITDIR/var-lib-avtonarezka.mount"
  run rm -f "$IMG"
  run rm -rf "$OPT" "$ETC"
  # после размонтирования каталог точки монтирования пуст; если ещё смонтирован — не трогаем
  if [ "$AVTO_FAKE" != 1 ] && mountpoint -q "$VAR"; then die "$VAR всё ещё смонтирован; размонтируйте и повторите"; fi
  run rm -rf "$VAR"
  if [ "$AVTO_FAKE" != 1 ] && id "$AVTO_USER" >/dev/null 2>&1; then sysrun userdel "$AVTO_USER"; fi
else
  say "Данные оставлены: $IMG и $VAR (удалить: --purge)"
  run rm -rf "$OPT/bin"
fi
sysrun systemctl daemon-reload
say "Откат выполнен. Сравните состояние: ./postflight.sh"
