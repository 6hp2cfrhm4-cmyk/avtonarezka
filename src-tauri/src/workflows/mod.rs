//! Режимы: потоки работы, сцены и правила автопереключения.
//!
//! Поток — отдельная возможность фабрики (ручная Студия, мониторинг каналов, автопоиск, live, публикация,
//! приём задач на сервере); каждый включается независимо. Сцена — именованный набор потоков с размещением
//! (ПК, сервер, авто). `plan` — чистая функция: по сцене, узлам и их нагрузке говорит, что где запускать и что
//! остановить, с причиной по-русски. Правила (`pick_scene`) переключают сцены сами, ручной выбор закрепляет сцену.
pub mod runtime;

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum Flow {
    /// Приоритет в порядке объявления: раньше — важнее.
    Live,
    Studio,
    Discovery,
    Monitor,
    Publish,
    Offload,
}

pub const ALL_FLOWS: [Flow; 6] = [Flow::Live, Flow::Studio, Flow::Discovery, Flow::Monitor, Flow::Publish, Flow::Offload];

impl Flow {
    pub fn label(self) -> &'static str {
        match self {
            Flow::Live => "Эфиры (live-нарезка)",
            Flow::Studio => "Ручная Студия",
            Flow::Discovery => "Автопоиск трендов",
            Flow::Monitor => "Мониторинг каналов",
            Flow::Publish => "Публикация по календарю",
            Flow::Offload => "Приём задач с ПК",
        }
    }
    /// Что потоку нужно от узла.
    pub fn needs(self) -> Needs {
        match self {
            Flow::Studio => Needs { gpu: true, browser: false, only: None },
            Flow::Monitor => Needs { gpu: false, browser: false, only: None },
            Flow::Discovery => Needs { gpu: false, browser: false, only: None },
            Flow::Live => Needs { gpu: false, browser: false, only: None },
            Flow::Publish => Needs { gpu: false, browser: false, only: None },
            Flow::Offload => Needs { gpu: false, browser: false, only: Some(NodeKind::Server) },
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Needs {
    pub gpu: bool,
    pub browser: bool,
    pub only: Option<NodeKind>,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum NodeKind {
    Pc,
    Server,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Placement {
    Pc,
    Server,
    Auto,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct FlowSetting {
    pub flow: Flow,
    pub enabled: bool,
    pub placement: Placement,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Scene {
    pub id: i64,
    pub name: String,
    pub flows: Vec<FlowSetting>,
    #[serde(default)]
    pub builtin: bool,
}

/// Нагрузка узла для регулятора (те же пороги, что в правиле G5).
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct Load {
    pub cpu_psi: f64,
    pub mem_psi: f64,
    pub mem_available_mb: u64,
    pub disk_free_mb: u64,
}

impl Load {
    pub fn pressure(&self) -> Option<&'static str> {
        if self.cpu_psi > 10.0 {
            Some("высокая нагрузка процессора")
        } else if self.mem_psi > 3.0 {
            Some("давление по памяти")
        } else if self.mem_available_mb > 0 && self.mem_available_mb < 700 {
            Some("мало свободной памяти")
        } else if self.disk_free_mb > 0 && self.disk_free_mb < 3072 {
            Some("мало места на диске")
        } else {
            None
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct NodeState {
    pub kind: NodeKind,
    pub online: bool,
    pub gpu: bool,
    pub browser: bool,
    #[serde(default)]
    pub load: Load,
}

#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Action {
    pub flow: Flow,
    /// Где работает; `None` — нигде не запускается.
    pub node: Option<NodeKind>,
    pub run: bool,
    /// Ограничение параллельных задач (для live — число эфиров), когда узел под нагрузкой.
    pub limit: Option<u32>,
    pub reason: String,
}

fn node(nodes: &[NodeState], kind: NodeKind) -> Option<&NodeState> {
    nodes.iter().find(|n| n.kind == kind && n.online)
}

/// Куда ставить поток: явное размещение, иначе по возможностям узлов. `Err` — причина, почему негде.
fn place(flow: Flow, want: Placement, nodes: &[NodeState]) -> Result<NodeKind, String> {
    let needs = flow.needs();
    let fits = |k: NodeKind| -> Result<(), String> {
        let n = node(nodes, k).ok_or_else(|| match k {
            NodeKind::Pc => "ПК не в сети".to_string(),
            NodeKind::Server => "сервер не в сети".to_string(),
        })?;
        if needs.only.is_some_and(|o| o != k) {
            return Err("этот поток работает только на сервере".into());
        }
        if needs.gpu && !n.gpu {
            return Err("нужна видеокарта, её нет на этом узле".into());
        }
        if needs.browser && !n.browser {
            return Err("нужен браузер, его нет на этом узле".into());
        }
        Ok(())
    };
    match want {
        Placement::Pc => fits(NodeKind::Pc).map(|_| NodeKind::Pc),
        Placement::Server => fits(NodeKind::Server).map(|_| NodeKind::Server),
        Placement::Auto => {
            // сначала ПК (быстрее и сильнее), иначе сервер
            let (pc, server) = (fits(NodeKind::Pc), fits(NodeKind::Server));
            match (pc, server) {
                (Ok(_), _) => Ok(NodeKind::Pc),
                (_, Ok(_)) => Ok(NodeKind::Server),
                (Err(a), Err(b)) => Err(format!("{a}; {b}")),
            }
        }
    }
}

/// Решает, что запускать. Результат упорядочен по приоритету потоков.
/// Под нагрузкой узла: live сокращается до одного эфира, автопоиск и мониторинг ставятся на паузу.
pub fn plan(scene: &Scene, nodes: &[NodeState]) -> Vec<Action> {
    let mut out: Vec<Action> = ALL_FLOWS
        .iter()
        .map(|&flow| {
            let setting = scene.flows.iter().find(|f| f.flow == flow);
            let Some(s) = setting.filter(|s| s.enabled) else {
                return Action { flow, node: None, run: false, limit: None, reason: "выключен в сцене".into() };
            };
            match place(flow, s.placement, nodes) {
                Err(why) => Action { flow, node: None, run: false, limit: None, reason: why },
                Ok(kind) => {
                    let load = node(nodes, kind).map(|n| n.load).unwrap_or_default();
                    match (load.pressure(), flow) {
                        (Some(why), Flow::Live) => Action { flow, node: Some(kind), run: true, limit: Some(1), reason: format!("{why}: один эфир") },
                        (Some(why), Flow::Discovery | Flow::Monitor) => Action { flow, node: Some(kind), run: false, limit: None, reason: format!("пауза: {why}") },
                        _ => Action { flow, node: Some(kind), run: true, limit: None, reason: "работает".into() },
                    }
                }
            }
        })
        .collect();
    out.sort_by_key(|a| a.flow);
    out
}

/// Что изменится при смене плана: остановить (плавно) и запустить.
pub fn diff(old: &[Action], new: &[Action]) -> (Vec<Flow>, Vec<Flow>) {
    let running = |v: &[Action], f: Flow| v.iter().find(|a| a.flow == f).filter(|a| a.run).map(|a| a.node);
    let mut stop = vec![];
    let mut start = vec![];
    for &f in &ALL_FLOWS {
        match (running(old, f), running(new, f)) {
            (Some(_), None) => stop.push(f),
            (None, Some(_)) => start.push(f),
            (Some(a), Some(b)) if a != b => {
                stop.push(f);
                start.push(f);
            }
            _ => {}
        }
    }
    (stop, start)
}

// ------------------------------------------------------------------ правила

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Cond {
    PcOnline { value: bool },
    /// Часы суток, `from` включительно, `to` не включительно; через полночь тоже работает.
    Hours { from: u32, to: u32 },
    ServerBusy,
    DiskLow,
    /// Стример из списка вышел в эфир (флаг выставляет поток автопоиска).
    StreamerLive,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Rule {
    pub id: i64,
    pub name: String,
    pub cond: Cond,
    pub scene_id: i64,
    pub enabled: bool,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Context {
    pub hour: u32,
    pub pc_online: bool,
    pub server_busy: bool,
    pub disk_low: bool,
    pub streamer_live: bool,
}

pub fn holds(c: &Cond, x: &Context) -> bool {
    match *c {
        Cond::PcOnline { value } => x.pc_online == value,
        Cond::Hours { from, to } => {
            if from <= to {
                x.hour >= from && x.hour < to
            } else {
                x.hour >= from || x.hour < to
            }
        }
        Cond::ServerBusy => x.server_busy,
        Cond::DiskLow => x.disk_low,
        Cond::StreamerLive => x.streamer_live,
    }
}

/// Первое сработавшее правило выбирает сцену. Ручное переключение закрепляет сцену до `pinned_until`.
pub fn pick_scene(rules: &[Rule], x: &Context, current: i64, pinned_until: i64, now: i64) -> Option<(i64, String)> {
    if now < pinned_until {
        return None;
    }
    let hit = rules.iter().find(|r| r.enabled && holds(&r.cond, x))?;
    (hit.scene_id != current).then(|| (hit.scene_id, hit.name.clone()))
}

/// На сколько часов ручной выбор закрепляет сцену.
pub const PIN_HOURS: i64 = 4;

/// Стандартные сцены (создаются при первом запуске).
pub fn builtin_scenes() -> Vec<Scene> {
    use Flow::*;
    use Placement::*;
    let s = |name: &str, f: &[(Flow, bool, Placement)]| Scene {
        id: 0,
        name: name.into(),
        builtin: true,
        flows: f.iter().map(|&(flow, enabled, placement)| FlowSetting { flow, enabled, placement }).collect(),
    };
    vec![
        s("День за ПК", &[(Studio, true, Pc), (Monitor, true, Pc), (Discovery, true, Auto), (Live, true, Auto), (Publish, true, Pc), (Offload, false, Server)]),
        s("Ночь на сервере", &[(Studio, false, Pc), (Monitor, true, Server), (Discovery, true, Server), (Live, true, Server), (Publish, true, Server), (Offload, true, Server)]),
        s("Только live", &[(Studio, false, Pc), (Monitor, false, Pc), (Discovery, false, Auto), (Live, true, Auto), (Publish, true, Auto), (Offload, true, Server)]),
        s("Экономия", &[(Studio, true, Pc), (Monitor, false, Pc), (Discovery, false, Auto), (Live, false, Auto), (Publish, true, Pc), (Offload, true, Server)]),
        s("Пауза всего", &[(Studio, false, Pc), (Monitor, false, Pc), (Discovery, false, Auto), (Live, false, Auto), (Publish, false, Pc), (Offload, false, Server)]),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pc(online: bool) -> NodeState { NodeState { kind: NodeKind::Pc, online, gpu: true, browser: true, load: Load::default() } }
    fn srv(online: bool) -> NodeState { NodeState { kind: NodeKind::Server, online, gpu: false, browser: false, load: Load::default() } }
    fn scene(name: &str) -> Scene { builtin_scenes().into_iter().find(|s| s.name == name).unwrap() }
    fn act(p: &[Action], f: Flow) -> &Action { p.iter().find(|a| a.flow == f).unwrap() }

    #[test]
    fn day_scene_puts_gpu_work_on_pc_and_auto_flows_prefer_pc() {
        let p = plan(&scene("День за ПК"), &[pc(true), srv(true)]);
        assert_eq!(act(&p, Flow::Studio).node, Some(NodeKind::Pc));
        assert_eq!(act(&p, Flow::Live).node, Some(NodeKind::Pc));
        assert!(!act(&p, Flow::Offload).run);
        // порядок: live важнее всего
        assert_eq!(p[0].flow, Flow::Live);
    }

    #[test]
    fn auto_falls_back_to_server_when_pc_is_off_and_gpu_flows_wait() {
        let p = plan(&scene("День за ПК"), &[pc(false), srv(true)]);
        assert_eq!(act(&p, Flow::Live).node, Some(NodeKind::Server));
        let studio = act(&p, Flow::Studio);
        assert!(!studio.run && studio.reason.contains("ПК не в сети"));
    }

    #[test]
    fn gpu_flow_is_refused_on_server_with_a_reason_not_silently() {
        let mut s = scene("Ночь на сервере");
        s.flows.iter_mut().find(|f| f.flow == Flow::Studio).unwrap().enabled = true;
        s.flows.iter_mut().find(|f| f.flow == Flow::Studio).unwrap().placement = Placement::Server;
        let p = plan(&s, &[pc(false), srv(true)]);
        assert!(act(&p, Flow::Studio).reason.contains("видеокарта"));
    }

    #[test]
    fn offload_only_runs_on_server() {
        let mut s = scene("Экономия");
        s.flows.iter_mut().find(|f| f.flow == Flow::Offload).unwrap().placement = Placement::Pc;
        let p = plan(&s, &[pc(true), srv(true)]);
        assert!(!act(&p, Flow::Offload).run);
    }

    #[test]
    fn pressure_shrinks_live_and_pauses_background_flows() {
        let mut busy = srv(true);
        busy.load = Load { cpu_psi: 14.0, ..Default::default() };
        let p = plan(&scene("Ночь на сервере"), &[pc(false), busy]);
        assert_eq!(act(&p, Flow::Live).limit, Some(1));
        assert!(act(&p, Flow::Live).run);
        assert!(!act(&p, Flow::Discovery).run && act(&p, Flow::Discovery).reason.contains("пауза"));
        assert!(!act(&p, Flow::Monitor).run);
        assert!(act(&p, Flow::Publish).run, "публикация не останавливается из-за нагрузки");
        let mut low = srv(true);
        low.load = Load { disk_free_mb: 2000, ..Default::default() };
        assert!(Load { disk_free_mb: 2000, ..Default::default() }.pressure().unwrap().contains("диск"));
        let _ = low;
    }

    #[test]
    fn switching_scene_stops_and_starts_only_what_changed() {
        let nodes = [pc(true), srv(true)];
        let day = plan(&scene("День за ПК"), &nodes);
        let night = plan(&scene("Ночь на сервере"), &nodes);
        let (stop, start) = diff(&day, &night);
        assert!(stop.contains(&Flow::Studio));
        assert!(start.contains(&Flow::Offload));
        assert!(stop.contains(&Flow::Live) && start.contains(&Flow::Live), "live переезжает с ПК на сервер");
        assert!(!stop.contains(&Flow::Publish) || !start.contains(&Flow::Publish) == false);
        let (s2, t2) = diff(&day, &day);
        assert!(s2.is_empty() && t2.is_empty());
    }

    #[test]
    fn rules_pick_scenes_and_manual_pin_wins() {
        let rules = vec![
            Rule { id: 1, name: "ПК выключился".into(), cond: Cond::PcOnline { value: false }, scene_id: 2, enabled: true },
            Rule { id: 2, name: "Ночь".into(), cond: Cond::Hours { from: 23, to: 7 }, scene_id: 2, enabled: true },
            Rule { id: 3, name: "День".into(), cond: Cond::PcOnline { value: true }, scene_id: 1, enabled: true },
        ];
        let off = Context { hour: 12, pc_online: false, ..Default::default() };
        assert_eq!(pick_scene(&rules, &off, 1, 0, 100).unwrap().0, 2);
        // уже в нужной сцене: ничего не делаем
        assert!(pick_scene(&rules, &off, 2, 0, 100).is_none());
        // закреплено вручную
        assert!(pick_scene(&rules, &off, 1, 200, 100).is_none());
        assert!(pick_scene(&rules, &off, 1, 200, 200).is_some());
        let on = Context { hour: 12, pc_online: true, ..Default::default() };
        assert_eq!(pick_scene(&rules, &on, 2, 0, 0).unwrap().0, 1);
    }

    #[test]
    fn hours_wrap_over_midnight() {
        let c = Cond::Hours { from: 23, to: 7 };
        for (h, want) in [(23, true), (0, true), (6, true), (7, false), (12, false), (22, false)] {
            assert_eq!(holds(&c, &Context { hour: h, ..Default::default() }), want, "час {h}");
        }
        let day = Cond::Hours { from: 9, to: 18 };
        assert!(holds(&day, &Context { hour: 9, ..Default::default() }) && !holds(&day, &Context { hour: 18, ..Default::default() }));
    }
}
