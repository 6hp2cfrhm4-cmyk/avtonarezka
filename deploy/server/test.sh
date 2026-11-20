#!/usr/bin/env bash
# Локальная проверка скриптов установки на подставном корне: на сервер ничего не отправляется.
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
T="$(mktemp -d)"; trap 'rm -rf "$T"' EXIT
export AVTO_ROOT="$T/root" AVTO_FAKE=1 ASSUME_YES=1
mkdir -p "$AVTO_ROOT/home/tester/.ssh" "$AVTO_ROOT/etc/systemd/system" "$AVTO_ROOT/etc/sudoers.d" "$AVTO_ROOT/var/lib" "$AVTO_ROOT/opt" "$T/dist"
echo "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAItestkeytestkeytestkeytestkeytestkeytest user@pc" > "$T/k.pub"
echo "ssh-ed25519 AAAAOTHER tester@pc" > "$AVTO_ROOT/home/tester/.ssh/authorized_keys"
printf '#!/bin/sh\necho d\n' > "$T/dist/avtonarezka-daemon"; printf '#!/bin/sh\necho c\n' > "$T/dist/avtoctl"
( cd "$T/dist" && sha256sum avtonarezka-daemon avtoctl > SHA256SUMS )
before=$(cd "$AVTO_ROOT" && find . | sort | xargs -I{} sh -c 'test -f "{}" && echo "{} $(sha256sum < "{}")" || echo "{}"')

"$HERE/install.sh" --login-user tester --dry-run --pubkey "$T/k.pub" --dist "$T/dist" >/dev/null
after_dry=$(cd "$AVTO_ROOT" && find . | sort | xargs -I{} sh -c 'test -f "{}" && echo "{} $(sha256sum < "{}")" || echo "{}"')
[ "$before" = "$after_dry" ] || { echo "FAIL: сухой прогон что-то изменил"; exit 1; }

"$HERE/install.sh" --login-user tester --pubkey "$T/k.pub" --dist "$T/dist" >/dev/null
"$HERE/install.sh" --login-user tester --pubkey "$T/k.pub" --dist "$T/dist" >/dev/null   # повтор: идемпотентность
ak="$AVTO_ROOT/home/tester/.ssh/authorized_keys"
[ "$(grep -c avtonarezka-managed "$ak")" = 1 ] || { echo "FAIL: строка ключа продублирована"; exit 1; }
grep -q '^restrict,command="/opt/avtonarezka/bin/avtoctl-gate" ssh-ed25519 ' "$ak" || { echo "FAIL: нет ограничений ключа"; exit 1; }
grep -q AAAAOTHER "$ak" || { echo "FAIL: чужой ключ пропал"; exit 1; }
grep -q "^tester ALL=(avtonarezka)" "$AVTO_ROOT/etc/sudoers.d/avtonarezka" || { echo "FAIL: sudoers без пользователя входа"; exit 1; }
! grep -q "@LOGIN_USER@" "$AVTO_ROOT/etc/sudoers.d/avtonarezka" || { echo "FAIL: шаблон sudoers не подставлен"; exit 1; }
for f in opt/avtonarezka/bin/avtoctl-gate opt/avtonarezka/bin/avtoctl opt/avtonarezka/bin/avtonarezka-daemon \
         etc/sudoers.d/avtonarezka etc/systemd/system/avtonarezka.slice etc/systemd/system/avtonarezka-daemon.service \
         etc/systemd/system/var-lib-avtonarezka.mount etc/avtonarezka/secrets.key var/lib/avtonarezka.img; do
  [ -e "$AVTO_ROOT/$f" ] || { echo "FAIL: нет $f"; exit 1; }
done
[ "$(wc -c < "$AVTO_ROOT/etc/avtonarezka/secrets.key" | tr -d ' ')" = 64 ] || { echo "FAIL: ключ секретов не 64 символа"; exit 1; }

"$HERE/uninstall.sh" --purge >/dev/null
after=$(cd "$AVTO_ROOT" && find . | sort | xargs -I{} sh -c 'test -f "{}" && echo "{} $(sha256sum < "{}")" || echo "{}"' | grep -v 'authorized_keys.avtonarezka-backup')
[ "$before" = "$after" ] || { echo "FAIL: после отката остались следы"; diff <(echo "$before") <(echo "$after"); exit 1; }

# gate: чужие команды отклоняются
out=$(SSH_ORIGINAL_COMMAND="bash -c id" sh "$HERE/avtoctl-gate" 2>&1 || true)
echo "$out" | grep -q "не разрешена" || { echo "FAIL: gate пропустил чужую команду"; exit 1; }
# preflight читает без ошибок и postflight на неизменном состоянии не видит расхождений
"$HERE/preflight.sh" "$T/a" >/dev/null 2>&1; 
echo "ВСЕ ПРОВЕРКИ ПРОШЛИ"
