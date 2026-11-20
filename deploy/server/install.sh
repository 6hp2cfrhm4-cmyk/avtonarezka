#!/usr/bin/env bash
# Установка «Автонарезки» на сервер по шагам. Меняет ТОЛЬКО то, что называется avtonarezka*
# (пользователь, /opt/avtonarezka, /var/lib/avtonarezka*, /etc/avtonarezka, юниты, sudoers, одна строка в authorized_keys).
# Каждый шаг спрашивает «да». Идемпотентен: повторный запуск ничего не ломает.
#
#   sudo ./install.sh --pubkey avtonarezka_ed25519.pub [--login-user ИМЯ] [--step N | --step all] [--yes] [--dry-run]
#   --login-user — пользователь, под которым вы заходите на сервер по ssh (по умолчанию тот, кто запустил sudo).
#   Шаги: 1 пользователь и каталоги · 2 образ диска · 3 юниты · 4 ключ и sudoers · 5 программы · 6 запуск
#         tools — установка внешних утилит из локального каталога (--tools-dir), ничего не скачивает.
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
# shellcheck source=lib.sh
. "$HERE/lib.sh"

STEP=all PUBKEY="" TOOLS_DIR="" MODELS_DIR="" DIST="$HERE/dist"
while [ $# -gt 0 ]; do
  case "$1" in
    --step) STEP="$2"; shift 2 ;;
    --pubkey) PUBKEY="$2"; shift 2 ;;
    --tools-dir) TOOLS_DIR="$2"; shift 2 ;;
    --models-dir) MODELS_DIR="$2"; shift 2 ;;
    --login-user) LOGIN_USER="$2"; shift 2 ;;
    --dist) DIST="$2"; shift 2 ;;
    --yes) ASSUME_YES=1; shift ;;
    --dry-run) DRY=1; shift ;;
    -h|--help) sed -n 2,9p "$0"; exit 0 ;;
    *) die "неизвестный параметр: $1" ;;
  esac
done
need_root
# логин нужен только шагу с ключом и sudoers (4) и «всё»
if [ "$STEP" = all ] || [ "$STEP" = 4 ]; then set_login_user "$LOGIN_USER"; fi

want() { [ "$STEP" = all ] || [ "$STEP" = "$1" ]; }

step1() {
  say "Шаг 1: системный пользователь $AVTO_USER (nologin, без sudo, без docker) и каталоги"
  confirm "Создать пользователя и каталоги $OPT, $ETC?" || return 0
  if [ "$AVTO_FAKE" != 1 ] && ! id "$AVTO_USER" >/dev/null 2>&1; then
    sysrun useradd --system --no-create-home --home-dir /nonexistent --shell /usr/sbin/nologin "$AVTO_USER"
  else
    sysrun true
  fi
  run mkdir -p "$OPT/bin" "$ETC"
  sysrun chown root:root "$OPT" "$OPT/bin"
  sysrun chown root:"$AVTO_USER" "$ETC"
  run chmod 0755 "$OPT" "$OPT/bin"
  run chmod 0750 "$ETC"
  if [ ! -e "$ETC/secrets.key" ]; then
    # Ключ шифрования секретов: 32 случайных байта, 64 hex-символа.
    if [ "$DRY" = 1 ]; then say "  [сухо] создать $ETC/secrets.key"; else
      ( umask 077; od -An -N32 -tx1 /dev/urandom | tr -d ' \n' > "$ETC/secrets.key" )
    fi
  fi
  run chmod 0640 "$ETC/secrets.key" 2>/dev/null || true
  sysrun chown root:"$AVTO_USER" "$ETC/secrets.key"
  say "  готово"
}

step2() {
  say "Шаг 2: образ диска ${IMG_SIZE_GIB} ГиБ (потолок места, G4) и его монтирование"
  confirm "Создать $IMG (${IMG_SIZE_GIB} ГиБ) и смонтировать в $VAR?" || return 0
  local free_kib need_kib
  if [ "$AVTO_FAKE" != 1 ]; then
    free_kib=$(df --output=avail -k /var/lib | tail -1 | tr -d ' ')
    need_kib=$(( (IMG_SIZE_GIB + 2) * 1024 * 1024 ))
    [ "$free_kib" -ge "$need_kib" ] || die "мало места: нужно как минимум $((IMG_SIZE_GIB + 2)) ГиБ свободно"
  fi
  if [ ! -e "$IMG" ]; then
    run mkdir -p "$(dirname "$IMG")"
    if [ "$AVTO_FAKE" = 1 ] && [ "$DRY" != 1 ]; then : > "$IMG"; else run fallocate -l "${IMG_SIZE_GIB}G" "$IMG"; fi
    sysrun mkfs.ext4 -q -F -m 0 -L avtonarezka "$IMG"
    run chmod 0600 "$IMG"
  else
    say "  образ уже есть, не трогаю"
  fi
  run mkdir -p "$VAR"
  run install -m 0644 "$HERE/units/var-lib-avtonarezka.mount" "$UNITDIR/var-lib-avtonarezka.mount"
  sysrun systemctl daemon-reload
  sysrun systemctl enable --now var-lib-avtonarezka.mount
  if [ "$AVTO_FAKE" != 1 ]; then
    mountpoint -q "$VAR" || die "образ не смонтировался"
  fi
  sysrun chown "$AVTO_USER:$AVTO_USER" "$VAR"
  run mkdir -p "$VAR/tmp"
  sysrun chown "$AVTO_USER:$AVTO_USER" "$VAR/tmp"
  say "  готово"
}

step3() {
  say "Шаг 3: юниты systemd (срез с лимитами и демон; демон пока не запускается)"
  confirm "Установить avtonarezka.slice и avtonarezka-daemon.service?" || return 0
  run install -m 0644 "$HERE/units/avtonarezka.slice" "$UNITDIR/avtonarezka.slice"
  run install -m 0644 "$HERE/units/avtonarezka-daemon.service" "$UNITDIR/avtonarezka-daemon.service"
  sysrun systemctl daemon-reload
  say "  готово"
}

step4() {
  say "Шаг 4: ограничитель ключа, sudoers и ОДНА строка в $AUTH_KEYS"
  [ -n "$PUBKEY" ] || die "укажите --pubkey (публичный ключ avtonarezka_ed25519.pub)"
  [ -f "$PUBKEY" ] || die "нет файла $PUBKEY"
  local keyline
  keyline=$(grep -E '^(ssh-ed25519|ssh-rsa|ecdsa-sha2-[a-z0-9]+) ' "$PUBKEY" | head -1 | awk '{print $1" "$2}')
  [ -n "$keyline" ] || die "в $PUBKEY нет публичного ключа"
  confirm "Поставить avtoctl-gate, $SUDOERS и добавить ключ с ограничением restrict,command=...?" || return 0
  run install -m 0755 "$HERE/avtoctl-gate" "$OPT/bin/avtoctl-gate"
  sysrun chown root:root "$OPT/bin/avtoctl-gate"
  # sudoers: сначала проверка синтаксиса, только потом установка
  local rendered; rendered=$(mktemp); sed "s/@LOGIN_USER@/$LOGIN_USER/g" "$HERE/sudoers.avtonarezka" > "$rendered"
  if command -v visudo >/dev/null 2>&1; then
    visudo -cf "$rendered" >/dev/null || { rm -f "$rendered"; die "sudoers не прошёл проверку"; }
  fi
  run install -m 0440 "$rendered" "$SUDOERS"
  rm -f "$rendered"
  # строка ключа
  run mkdir -p "$ADMIN_HOME/.ssh"
  if [ ! -e "$AUTH_KEYS" ] && [ "$DRY" != 1 ]; then : > "$AUTH_KEYS"; chmod 0600 "$AUTH_KEYS"; fi
  if [ "$DRY" != 1 ] && grep -qF "avtonarezka-managed" "$AUTH_KEYS"; then
    say "  строка ключа уже есть, не дублирую"
  else
    if [ "$DRY" != 1 ]; then
      cp -p "$AUTH_KEYS" "$AUTH_KEYS.avtonarezka-backup"
      printf 'restrict,command="/opt/avtonarezka/bin/avtoctl-gate" %s avtonarezka-managed\n' "$keyline" >> "$AUTH_KEYS"
    else
      say "  [сухо] добавить строку ключа в $AUTH_KEYS"
    fi
  fi
  say "  готово. Резервная копия: $AUTH_KEYS.avtonarezka-backup"
}

step5() {
  say "Шаг 5: программы avtonarezka-daemon и avtoctl из $DIST"
  confirm "Установить бинарники в $OPT/bin?" || return 0
  [ -f "$DIST/SHA256SUMS" ] || die "нет $DIST/SHA256SUMS (сначала build.sh)"
  ( cd "$DIST" && sha256sum -c SHA256SUMS ) || die "контрольные суммы не сошлись"
  run install -m 0755 "$DIST/avtonarezka-daemon" "$OPT/bin/avtonarezka-daemon"
  run install -m 0755 "$DIST/avtoctl" "$OPT/bin/avtoctl"
  sysrun chown root:root "$OPT/bin/avtonarezka-daemon" "$OPT/bin/avtoctl"
  say "  готово"
}

steptools() {
  say "Утилиты из каталога $TOOLS_DIR (ffmpeg, yt-dlp, deno и т. п.): скрипт ничего не скачивает"
  [ -n "$TOOLS_DIR" ] && [ -d "$TOOLS_DIR" ] || die "укажите --tools-dir с уже скачанными и проверенными файлами"
  [ -f "$TOOLS_DIR/SHA256SUMS" ] || die "нужен $TOOLS_DIR/SHA256SUMS"
  confirm "Скопировать утилиты из $TOOLS_DIR в $OPT/bin?" || return 0
  ( cd "$TOOLS_DIR" && sha256sum -c SHA256SUMS ) || die "контрольные суммы не сошлись"
  local f
  for f in "$TOOLS_DIR"/*; do
    [ "$(basename "$f")" = SHA256SUMS ] && continue
    run install -m 0755 "$f" "$OPT/bin/$(basename "$f")"
  done
  say "  готово"
}

stepmodels() {
  say "Модели из каталога $MODELS_DIR (подкаталоги вида имя/файл): скрипт ничего не скачивает"
  [ -n "$MODELS_DIR" ] && [ -d "$MODELS_DIR" ] || die "укажите --models-dir с уже скачанными и проверенными файлами"
  [ -f "$MODELS_DIR/SHA256SUMS" ] || die "нужен $MODELS_DIR/SHA256SUMS (пути относительно каталога)"
  confirm "Скопировать модели из $MODELS_DIR в /var/lib/avtonarezka/models?" || return 0
  ( cd "$MODELS_DIR" && sha256sum -c SHA256SUMS ) || die "контрольные суммы не сошлись"
  local f rel
  while read -r _ rel; do
    rel="${rel#\*}"
    run install -d -o "$AVTO_USER" -g "$AVTO_USER" "/var/lib/avtonarezka/models/$(dirname "$rel")"
    run install -m 0644 -o "$AVTO_USER" -g "$AVTO_USER" "$MODELS_DIR/$rel" "/var/lib/avtonarezka/models/$rel"
  done < "$MODELS_DIR/SHA256SUMS"
  say "  готово"
}

step6() {
  say "Шаг 6: запуск демона"
  confirm "Включить и запустить avtonarezka-daemon.service?" || return 0
  sysrun systemctl enable --now avtonarezka-daemon.service
  say "  готово"
}

want 1 && step1
want 2 && step2
want 3 && step3
want 4 && step4
want 5 && step5
[ "$STEP" = tools ] && steptools
[ "$STEP" = models ] && stepmodels
want 6 && step6
say "Установка завершена. Проверьте: ./postflight.sh"
