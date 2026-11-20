# Свой сервер для Автонарезки

Сервер работает вторым работником: ищет тренды, пишет эфиры и режет их, принимает задачи, пока ПК выключен. Он ваш: приложение хранит адрес и ключ локально, ничего не привязано к чужому серверу.

## Что нужно

- VPS с **Ubuntu 24.04**, 2 ядра, 4 ГБ памяти, свободно 6+ ГБ диска. Видеокарта не нужна.
- Вход по ssh и пользователь с `sudo` (не root).
- На сервере может работать что угодно ещё: установка создаёт только `avtonarezka*` (отдельный пользователь, `/opt/avtonarezka`, образ диска 4 ГиБ, юниты systemd с лимитами) и не меняет чужое. Демон ничего не слушает.

## Быстрый путь из приложения

«Сервер» → «Свой сервер: пошаговая настройка» → «Создать ключ». Приложение покажет команды для сервера с вашим публичным ключом. Дальше: адрес, порт, пользователь → «Сохранить» → «Проверить отпечаток сервера» (сверьте с панелью хостинга) → «Проверить связь».

## Вручную

На сервере (версию подставьте из «О приложении» или релиза на GitHub):

```bash
V=0.4.2
curl -fL -O https://github.com/6hp2cfrhm4-cmyk/avtonarezka/releases/download/v$V/avtonarezka-server-$V.tar.gz
curl -fL -O https://github.com/6hp2cfrhm4-cmyk/avtonarezka/releases/download/v$V/avtonarezka-server-$V.tar.gz.sha256
sha256sum -c avtonarezka-server-$V.tar.gz.sha256
tar xzf avtonarezka-server-$V.tar.gz && cd avtonarezka-server
# публичный ключ из приложения (или `ssh-keygen -t ed25519 -f avtonarezka_key` на ПК)
echo 'ssh-ed25519 AAAA… avtonarezka' > key.pub
sudo ./install.sh --pubkey key.pub --login-user ВАШ_ПОЛЬЗОВАТЕЛЬ     # шаги 1–6, каждый спрашивает «да»
./fetch-tools.sh --yes                                                  # ffmpeg, yt-dlp, Deno, модели: с проверкой сумм
sudo ./install.sh --step tools --tools-dir tools --yes
sudo ./install.sh --step models --models-dir models --yes
sudo systemctl restart avtonarezka-daemon
```

Что скачивает `fetch-tools.sh`: ffmpeg (≈42 МБ, johnvansickle.com), yt-dlp (≈3 МБ, GitHub), Deno (≈42 МБ, GitHub), Whisper small (≈190 МБ) и YAMNet (≈16 МБ) с huggingface.co. Всё сверяется с контрольными суммами. Без `--yes` скрипт ничего не качает.

## Безопасность

- Ключ проекта на сервере ограничен (`restrict,command=…`): только канал задач, без shell и проброса портов. Другие команды отклоняются.
- Приложение пробует войти не более двух раз подряд (иначе fail2ban может заблокировать ваш адрес).
- Секреты (токены Twitch, YouTube, Kick, Telegram), отправленные на сервер из «Интеграций», лежат в зашифрованном файле, ключ шифрования вне каталога данных.

## Проверка и откат

- `sudo ./preflight.sh before` до установки и `sudo ./postflight.sh before after` после: покажет изменения вне `avtonarezka*`. Изменения, сделанные вами самими на сервере между снимками, он тоже покажет.
- `sudo ./loadtest.sh` проверяет, что нагрузка проекта не мешает соседним службам.
- `sudo ./uninstall.sh --purge` убирает всё установленное проектом.
- Установку на подставном корне проверяет `./test.sh` (на сервер не ходит).
