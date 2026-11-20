import { useState } from "react";
import { api, errText, type ServerConfig, type ServerKey } from "../lib/api";

const REPO = "https://github.com/6hp2cfrhm4-cmyk/avtonarezka";

function Code({ text }: { text: string }) {
  const [done, setDone] = useState(false);
  return <div style={{ position: "relative" }}>
    <pre className="server-code">{text}</pre>
    <button type="button" className="factory-link" style={{ position: "absolute", top: 6, right: 8 }} onClick={() => { void navigator.clipboard?.writeText(text).then(() => { setDone(true); setTimeout(() => setDone(false), 1500); }); }}>{done ? "Скопировано" : "Копировать"}</button>
  </div>;
}

/** Пошаговая настройка своего сервера: ключ → установка → подключение. Ничего не отправляется на сервер без вашего ручного шага. */
export default function ServerWizard({ cfg, onKey }: { cfg: ServerConfig; onKey: (keyPath: string) => void }) {
  const [key, setKey] = useState<ServerKey | null>(null);
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);
  const make = async () => { setBusy(true); setError(""); try { const k = await api.nodeKeygen(); setKey(k); onKey(k.keyPath); } catch (e) { setError(errText(e)); } finally { setBusy(false); } };
  const v = key?.version ?? "";
  const url = `${REPO}/releases/download/v${v}/avtonarezka-server-${v}.tar.gz`;
  const login = cfg.user.trim() || "ВАШ_ПОЛЬЗОВАТЕЛЬ";
  const install = `curl -fL -o avto-server.tar.gz ${url}
curl -fL -o avto-server.sha256 ${url}.sha256 && sed -i 's/avtonarezka-server-${v}.tar.gz/avto-server.tar.gz/' avto-server.sha256 && sha256sum -c avto-server.sha256
tar xzf avto-server.tar.gz && cd avtonarezka-server
echo '${key?.publicKey ?? "(сначала создайте ключ)"}' > key.pub
sudo ./install.sh --pubkey key.pub --login-user ${login}
./fetch-tools.sh --yes
sudo ./install.sh --step tools --tools-dir tools --yes
sudo ./install.sh --step models --models-dir models --yes
sudo systemctl restart avtonarezka-daemon`;
  return <details className="factory-panel" style={{ marginBottom: 16 }}>
    <summary><strong>Свой сервер: пошаговая настройка</strong></summary>
    <p>Подойдёт любой VPS с Ubuntu 24.04 (2 ядра, 4 ГБ памяти, свободно хотя бы 6 ГБ), доступом по ssh и пользователем с sudo. Установка создаёт отдельного пользователя `avtonarezka`, свой каталог и лимиты systemd; чужие службы на сервере не затрагиваются, каждый шаг спрашивает подтверждение.</p>
    <h3>1. Ключ для приложения</h3>
    <p>Создаётся отдельный ключ только для этого сервера. На сервере он ограничивается: только канал задач, без shell.</p>
    <div className="factory-actions"><button type="button" className="factory-secondary" disabled={busy} onClick={() => void make()}>{key ? "Показать ключ снова" : "Создать ключ"}</button></div>
    {error && <p className="factory-error" role="alert">{error}</p>}
    {key && <>
      <p>Закрытая часть: <code>{key.keyPath}</code> (путь уже подставлен в настройки ниже, не забудьте нажать «Сохранить»).</p>
      <h3>2. Установка на сервере</h3>
      <p>Зайдите на сервер своим обычным способом и выполните (подставлены ваш публичный ключ и пользователь из поля ниже). Скрипт загрузки показывает размеры и источники, скачивает с проверкой контрольных сумм.</p>
      <Code text={install} />
      <h3>3. Подключение</h3>
      <p>Укажите адрес, порт ssh и пользователя входа в форме ниже, сохраните, нажмите «Проверить отпечаток сервера» (сверьте его с панелью хостинга) и «Проверить связь».</p>
      <p>Откат: <code>sudo ./uninstall.sh --purge</code> на сервере возвращает его к состоянию до установки.</p>
    </>}
  </details>;
}
