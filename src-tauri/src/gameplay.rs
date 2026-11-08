//! Лента геймплея. Фон нарезается по тем же таймкодам, что и основное видео: клип, который в исходнике идёт
//! с 10:00, берёт фон с позиции «начало ленты + 10:00». Остаток фона после видео достаётся следующему видео:
//! лента помнит, докуда дошла, и идёт «до конца» по списку фонов, затем по кругу.
//!
//! Расчёт — чистая функция [`plan`]; чтение и запись положения ленты в базе — [`allocate`].

use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use tauri::State;

use crate::{
    drafts::{kv_get, kv_set, GAMEPLAY_START as START},
    state::AppState,
};

const CURSOR_KEY: &str = "gameplay_cursor";
const BASE_PREFIX: &str = "gameplay_base:";

/// Положение ленты: в каком фоне и на какой секунде она стоит.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Cursor {
    pub asset: i64,
    pub pos: f64,
}

/// Клип основного видео: `start` — таймкод в исходнике, `need` — сколько секунд фона нужно (клип плюс баннер).
#[derive(Clone, Copy, Debug)]
pub struct Clip {
    pub id: i64,
    pub start: f64,
    pub need: f64,
}

/// Откуда брать фон для клипа.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Slice {
    pub clip: i64,
    pub asset: i64,
    pub offset: f64,
}

fn r3(x: f64) -> f64 {
    (x * 1000.0).round() / 1000.0
}

/// Раскладывает клипы по фонам, начиная с положения `from`. `assets` — `(id, длительность)` в порядке ленты.
/// Возвращает фон для каждого клипа (в порядке таймкодов) и положение ленты после последнего клипа.
///
/// Клип, которому не хватает места до конца фона, и все, что за ним, переходят на следующий фон с начала.
/// Хвост предыдущего фона короче клипа пропадает. Когда кончились все фоны, лента идёт по кругу.
/// Фон короче клипа берётся с нуля: ролик зациклит его.
pub fn plan(assets: &[(i64, f64)], from: Cursor, clips: &[Clip]) -> (Vec<Slice>, Cursor) {
    let n = assets.len();
    if n == 0 {
        return (vec![], from);
    }
    let (mut idx, mut base) = match assets.iter().position(|a| a.0 == from.asset) {
        Some(i) => (i, from.pos),
        None => (0, 0.0),
    };
    let mut order = clips.to_vec();
    order.sort_by(|a, b| a.start.total_cmp(&b.start));

    let mut out = Vec::with_capacity(order.len());
    let mut end = Cursor { asset: assets[idx].0, pos: base };
    for c in &order {
        let mut offset = (base + c.start).max(START);
        if offset + c.need > assets[idx].1 {
            // свой же фон проверяется последним: при одном фоне лента начинает новый круг
            match (1..=n).map(|k| (idx + k) % n).find(|&j| assets[j].1 >= c.need + START) {
                Some(j) => {
                    idx = j;
                    offset = START;
                    base = START - c.start;
                }
                None => {
                    offset = 0.0;
                    base = -c.start;
                }
            }
        }
        out.push(Slice { clip: c.id, asset: assets[idx].0, offset: r3(offset) });
        end = Cursor { asset: assets[idx].0, pos: r3((offset + c.need).min(assets[idx].1)) };
    }
    (out, end)
}

#[derive(Serialize, Deserialize)]
struct Saved {
    asset: i64,
    pos: f64,
    /// Какой проект последним двигал ленту.
    #[serde(default)]
    project: i64,
}

fn save(db: &Connection, key: &str, c: Cursor, project: i64) {
    if let Ok(json) = serde_json::to_string(&Saved { asset: c.asset, pos: c.pos, project }) {
        let _ = kv_set(db, key, &json);
    }
}

/// Раскладывает фон для всех клипов проекта и запоминает, докуда дошла лента.
///
/// Начало ленты для проекта запоминается при первом расчёте, поэтому повторный расчёт (новые моменты, «Повторить»)
/// даёт те же места и ленту не сдвигает. Ленту двигает только проект, который начал её последним:
/// момент, добавленный задним числом в старый проект, следующему видео не мешает.
pub fn allocate(db: &Connection, project_id: i64, assets: &[(i64, f64)], clips: &[Clip]) -> Vec<Slice> {
    let Some(first) = assets.first() else { return vec![] };
    let load = |key: &str| {
        kv_get(db, key)
            .and_then(|j| serde_json::from_str::<Saved>(&j).ok())
            .filter(|s| assets.iter().any(|a| a.0 == s.asset))
    };
    let base_key = format!("{BASE_PREFIX}{project_id}");
    let stored = load(&base_key);
    let tape = load(CURSOR_KEY);

    let from = stored
        .as_ref()
        .or(tape.as_ref())
        .map_or(Cursor { asset: first.0, pos: 0.0 }, |s| Cursor { asset: s.asset, pos: s.pos });
    let (slices, end) = plan(assets, from, clips);

    if stored.is_none() {
        save(db, &base_key, from, project_id);
    }
    if stored.is_none() || tape.as_ref().is_some_and(|t| t.project == project_id) {
        save(db, CURSOR_KEY, end, project_id);
    }
    slices
}

/// Начать ленту с начала: забыть, докуда она дошла и откуда начинался каждый проект.
/// Готовые черновики не меняются, пока их не пересчитают.
pub fn reset(db: &Connection) {
    let _ = db.execute("DELETE FROM kv WHERE key = ?1 OR key LIKE ?2", [CURSOR_KEY, &format!("{BASE_PREFIX}%")]);
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TapeDto {
    pub asset_id: i64,
    /// Секунда фона, до которой лента уже использована.
    pub pos: f64,
}

#[tauri::command]
pub fn gameplay_tape(state: State<'_, AppState>) -> Option<TapeDto> {
    let db = state.db.0.lock().unwrap();
    kv_get(&db, CURSOR_KEY)
        .and_then(|j| serde_json::from_str::<Saved>(&j).ok())
        .map(|s| TapeDto { asset_id: s.asset, pos: s.pos })
}

#[tauri::command]
pub fn gameplay_tape_reset(state: State<'_, AppState>) {
    reset(&state.db.0.lock().unwrap());
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clip(id: i64, start: f64, need: f64) -> Clip {
        Clip { id, start, need }
    }

    fn at(asset: i64, pos: f64) -> Cursor {
        Cursor { asset, pos }
    }

    fn places(s: &[Slice]) -> Vec<(i64, i64, f64)> {
        s.iter().map(|x| (x.clip, x.asset, x.offset)).collect()
    }

    #[test]
    fn clips_take_the_background_at_their_own_timecodes() {
        let (s, end) = plan(&[(2, 900.0)], at(2, 0.0), &[clip(1, 100.0, 44.0), clip(2, 300.0, 44.0), clip(3, 600.0, 54.0)]);
        assert_eq!(places(&s), vec![(1, 2, 100.0), (2, 2, 300.0), (3, 2, 600.0)]);
        assert_eq!(end, at(2, 654.0), "лента остановилась в конце последнего клипа");
    }

    #[test]
    fn clips_are_laid_out_in_timecode_order_whatever_the_input_order() {
        let (s, _) = plan(&[(2, 900.0)], at(2, 0.0), &[clip(3, 600.0, 50.0), clip(1, 100.0, 40.0), clip(2, 300.0, 40.0)]);
        assert_eq!(places(&s).iter().map(|p| p.0).collect::<Vec<_>>(), vec![1, 2, 3]);
    }

    #[test]
    fn a_clip_at_the_very_start_does_not_begin_on_the_black_zero_frame() {
        let (s, _) = plan(&[(2, 900.0)], at(2, 0.0), &[clip(1, 0.0, 30.0)]);
        assert_eq!(s[0].offset, START);
    }

    #[test]
    fn the_next_video_continues_where_the_previous_one_ended() {
        let assets = [(2, 900.0)];
        let (_, end) = plan(&assets, at(2, 0.0), &[clip(1, 100.0, 44.0), clip(2, 600.0, 54.0)]);
        assert_eq!(end, at(2, 654.0));
        let (s, end2) = plan(&assets, end, &[clip(1, 100.0, 44.0), clip(2, 200.0, 44.0)]);
        assert_eq!(places(&s), vec![(1, 2, 754.0), (2, 2, 854.0)]);
        assert_eq!(end2, at(2, 898.0));
    }

    #[test]
    fn a_clip_that_does_not_fit_moves_the_tape_to_the_next_background_and_the_rest_follow() {
        let assets = [(1, 100.0), (2, 500.0)];
        let (s, end) = plan(&assets, at(1, 0.0), &[clip(1, 10.0, 30.0), clip(2, 50.0, 30.0), clip(3, 80.0, 30.0), clip(4, 120.0, 30.0)]);
        // 80 + 30 > 100: третий клип начинает второй фон, четвёртый идёт в нём через 40 с (120 − 80)
        assert_eq!(places(&s), vec![(1, 1, 10.0), (2, 1, 50.0), (3, 2, START), (4, 2, 40.01)]);
        assert_eq!(end, at(2, 70.01));
    }

    #[test]
    fn the_tail_shorter_than_a_clip_is_dropped_at_the_start_of_the_next_video() {
        let assets = [(1, 100.0), (2, 500.0)];
        let (s, _) = plan(&assets, at(1, 90.0), &[clip(1, 0.0, 30.0)]);
        assert_eq!(places(&s), vec![(1, 2, START)]);
    }

    #[test]
    fn after_the_last_background_the_tape_starts_a_new_lap() {
        let assets = [(1, 100.0), (2, 100.0)];
        let (s, end) = plan(&assets, at(2, 80.0), &[clip(1, 0.0, 30.0), clip(2, 50.0, 30.0)]);
        // в хвосте второго фона 20 с, клипу нужно 30: круг начинается с первого
        assert_eq!(places(&s), vec![(1, 1, START), (2, 1, 50.01)]);
        assert_eq!(end, at(1, 80.01));
    }

    #[test]
    fn a_single_background_laps_over_itself() {
        let (s, _) = plan(&[(2, 900.0)], at(2, 654.0), &[clip(1, 100.0, 44.0), clip(2, 200.0, 44.0), clip(3, 210.0, 44.0), clip(4, 300.0, 44.0)]);
        assert_eq!(places(&s), vec![(1, 2, 754.0), (2, 2, 854.0), (3, 2, START), (4, 2, 90.01)]);
    }

    #[test]
    fn a_background_shorter_than_the_clip_is_not_chosen_while_another_one_fits() {
        let assets = [(1, 100.0), (3, 30.0)];
        let (s, _) = plan(&assets, at(1, 0.0), &[clip(1, 10.0, 44.0), clip(2, 70.0, 44.0)]);
        // второй клип не помещается в первый фон, а третий короче клипа: новый круг по первому
        assert_eq!(places(&s), vec![(1, 1, 10.0), (2, 1, START)]);
    }

    #[test]
    fn when_every_background_is_shorter_than_the_clip_they_start_from_zero_and_loop() {
        let (s, _) = plan(&[(3, 30.0)], at(3, 0.0), &[clip(1, 100.0, 44.0), clip(2, 300.0, 44.0)]);
        assert_eq!(places(&s), vec![(1, 3, 0.0), (2, 3, 0.0)]);
    }

    #[test]
    fn a_cursor_on_a_removed_background_means_the_start_of_the_list() {
        let (s, _) = plan(&[(2, 900.0), (5, 900.0)], at(77, 500.0), &[clip(1, 100.0, 40.0)]);
        assert_eq!(places(&s), vec![(1, 2, 100.0)]);
    }

    #[test]
    fn no_backgrounds_means_no_places() {
        let (s, end) = plan(&[], at(1, 5.0), &[clip(1, 100.0, 40.0)]);
        assert!(s.is_empty());
        assert_eq!(end, at(1, 5.0));
    }

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        crate::db::migrate(&c).unwrap();
        c
    }

    #[test]
    fn recalculating_a_project_gives_the_same_places_and_does_not_move_the_tape() {
        let db = db();
        let assets = [(2, 900.0)];
        let clips = [clip(1, 100.0, 44.0), clip(2, 600.0, 54.0)];
        let first = allocate(&db, 1, &assets, &clips);
        let again = allocate(&db, 1, &assets, &clips);
        assert_eq!(first, again);
        assert_eq!(places(&first), vec![(1, 2, 100.0), (2, 2, 600.0)]);
    }

    #[test]
    fn the_second_project_starts_after_the_first_and_the_tape_survives_a_late_moment_in_the_old_one() {
        let db = db();
        let assets = [(2, 900.0)];
        allocate(&db, 1, &assets, &[clip(1, 100.0, 44.0), clip(2, 600.0, 54.0)]);

        let second = allocate(&db, 2, &assets, &[clip(1, 100.0, 44.0)]);
        assert_eq!(places(&second), vec![(1, 2, 754.0)]);

        // в первом проекте добавили ещё один момент: лента второго проекта не двигается
        let first = allocate(&db, 1, &assets, &[clip(1, 100.0, 44.0), clip(2, 600.0, 54.0), clip(3, 800.0, 40.0)]);
        assert_eq!(places(&first)[..2], [(1, 2, 100.0), (2, 2, 600.0)]);
        let third = allocate(&db, 3, &assets, &[clip(1, 0.0, 30.0)]);
        assert_eq!(places(&third), vec![(1, 2, 798.0)], "третье видео идёт сразу за вторым");
    }

    #[test]
    fn the_last_project_keeps_moving_the_tape_when_it_gets_more_moments() {
        let db = db();
        let assets = [(2, 900.0)];
        allocate(&db, 1, &assets, &[clip(1, 100.0, 44.0)]);
        allocate(&db, 1, &assets, &[clip(1, 100.0, 44.0), clip(2, 400.0, 44.0)]);
        let next = allocate(&db, 2, &assets, &[clip(1, 0.0, 30.0)]);
        assert_eq!(places(&next), vec![(1, 2, 444.0)]);
    }

    #[test]
    fn reset_starts_the_tape_over() {
        let db = db();
        let assets = [(2, 900.0)];
        allocate(&db, 1, &assets, &[clip(1, 100.0, 44.0)]);
        reset(&db);
        let again = allocate(&db, 2, &assets, &[clip(1, 100.0, 44.0)]);
        assert_eq!(places(&again), vec![(1, 2, 100.0)]);
        // и первый проект после сброса тоже считается заново с нуля
        let p1 = allocate(&db, 1, &assets, &[clip(1, 100.0, 44.0)]);
        assert_eq!(places(&p1), vec![(1, 2, 244.0)], "ленту уже взял второй проект: она дошла до 144-й секунды");
    }
}
