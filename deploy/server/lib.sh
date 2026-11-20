# Общие функции скриптов установки. Подключается через `. lib.sh`; сам ничего не выполняет.
# Для проверки без сервера:
#   AVTO_ROOT=/каталог   — все пути берутся внутри него (подставной корень);
#   AVTO_FAKE=1          — не вызываются useradd, systemctl, mount, mkfs, chown (только печатается);
#   DRY=1                — вообще ничего не меняется, только печатается.
AVTO_ROOT="${AVTO_ROOT:-}"
AVTO_FAKE="${AVTO_FAKE:-0}"
DRY="${DRY:-0}"
ASSUME_YES="${ASSUME_YES:-0}"

R() { printf '%s%s' "$AVTO_ROOT" "$1"; }          # путь внутри (подставного) корня
say() { printf '%s\n' "$*"; }
die() { printf 'ОШИБКА: %s\n' "$*" >&2; exit 1; }

# Выполняет команду, либо печатает её (сухой прогон).
run() {
  if [ "$DRY" = 1 ]; then say "  [сухо] $*"; return 0; fi
  "$@"
}
# То же, но пропускается в подставном корне (системные вызовы).
sysrun() {
  if [ "$DRY" = 1 ]; then say "  [сухо] $*"; return 0; fi
  if [ "$AVTO_FAKE" = 1 ]; then say "  [подставной корень] $*"; return 0; fi
  "$@"
}

# Спрашивает явное «да» перед шагом. Без --yes и без терминала не продолжает.
confirm() {
  [ "$ASSUME_YES" = 1 ] && return 0
  [ "$DRY" = 1 ] && return 0
  [ -t 0 ] || die "нужен терминал для подтверждения или флаг --yes"
  printf '%s [да/нет] ' "$1"
  read -r ans
  [ "$ans" = "да" ] || [ "$ans" = "yes" ] || { say "пропущено"; return 1; }
}

need_root() {
  [ "$AVTO_FAKE" = 1 ] && return 0
  [ "$(id -u)" = 0 ] || die "нужен root (запустите через sudo)"
}

AVTO_USER=avtonarezka
OPT="$(R /opt/avtonarezka)"
VAR="$(R /var/lib/avtonarezka)"
IMG="$(R /var/lib/avtonarezka.img)"
ETC="$(R /etc/avtonarezka)"
UNITDIR="$(R /etc/systemd/system)"
SUDOERS="$(R /etc/sudoers.d/avtonarezka)"
# Пользователь, под которым вы заходите на сервер по ssh (ключ проекта добавляется в его authorized_keys).
# По умолчанию — тот, кто запустил sudo; можно задать --login-user или AVTO_LOGIN_USER.
LOGIN_USER="${AVTO_LOGIN_USER:-${SUDO_USER:-}}"
ADMIN_HOME="" AUTH_KEYS=""
set_login_user() {
  LOGIN_USER="$1"
  [ -n "$LOGIN_USER" ] && [ "$LOGIN_USER" != root ] || die "укажите --login-user: пользователя, под которым вы заходите на сервер по ssh (не root)"
  case "$LOGIN_USER" in *[!a-zA-Z0-9._-]*) die "недопустимое имя пользователя: $LOGIN_USER" ;; esac
  if [ "$AVTO_FAKE" = 1 ]; then ADMIN_HOME="$(R /home/$LOGIN_USER)"; else
    local h; h=$(getent passwd "$LOGIN_USER" | cut -d: -f6); [ -n "$h" ] && [ -d "$h" ] || die "нет пользователя $LOGIN_USER или его домашнего каталога"
    ADMIN_HOME="$h"
  fi
  AUTH_KEYS="$ADMIN_HOME/.ssh/authorized_keys"
}
KEY_MARK="# avtonarezka-managed"
IMG_SIZE_GIB="${IMG_SIZE_GIB:-4}"
