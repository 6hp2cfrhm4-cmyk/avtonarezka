import { motion } from "motion/react";
import { useUi, type AppArea } from "../store";
import Stepper from "./Stepper";
export const AREAS: { id: AppArea; label: string }[] = [
  { id: "factory", label: "Фабрика" }, { id: "channels", label: "Каналы" }, { id: "templates", label: "Шаблоны" },
  { id: "campaigns", label: "Кампании" }, { id: "calendar", label: "Календарь" }, { id: "analytics", label: "Аналитика" },
  { id: "accounts", label: "Аккаунты" }, { id: "studio", label: "Студия" }, { id: "settings", label: "Настройки" },
  { id: "disk", label: "Диск" }, { id: "trends", label: "Тренды" }, { id: "live", label: "Эфиры" }, { id: "modes", label: "Режимы" }, { id: "server", label: "Сервер" }, { id: "integrations", label: "Интеграции" },
];
export default function FactoryNav() {
  const area = useUi(s => s.area); const setArea = useUi(s => s.setArea);
  return <div className="factory-nav-wrap"><nav aria-label="Разделы Автонарезки" className="factory-nav">{AREAS.map(item => <motion.button key={item.id} whileHover={{ x: 3 }} onClick={() => setArea(item.id)} aria-current={item.id === area ? "page" : undefined}>{item.label}</motion.button>)}</nav>{area === "studio" && <div className="factory-studio-steps"><Stepper /></div>}<p className="factory-nav-note">Локальная обработка<br />Профили → ролики → публикации</p></div>;
}
