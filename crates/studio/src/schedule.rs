//! Modes and the daily schedule (card 302).
//!
//! The owner, 2026-09-26: *"i'd like, for example, to be able to go into night
//! mode where it's a different patch at the lowest possible visible
//! brightness, then restore in the morning."*
//!
//! Three things, and nothing else:
//!
//! - A **mode** is `{name, patch, setting, brightness}` (card 179's model): what
//!   the panel should look like, by name. `setting` of `None` is the patch's
//!   working copy - what it was last left at; `brightness` of `None` leaves the
//!   panel's brightness alone.
//! - The **schedule** is a list of `{at: "HH:MM", mode}` for every day, and a
//!   switch. The entry **due** at a local time is the latest one at or before
//!   it - wrapping, so before the day's first entry the last one is due.
//! - The **run record** ([`ScheduleRun`]) says which entry was last applied, on
//!   which local date. The scheduler applies the due entry when its identity
//!   (time + date) differs from the run record, and does **nothing else**: it
//!   never compares what is playing with what the mode says. That is the whole
//!   of the owner's "hold until the next timetable entry" - a change he makes
//!   by hand stays until the next entry comes due. The run record is in the
//!   state file, so a hold survives a restart; a restart *across* an entry's
//!   time applies it, because the due entry's identity is then a new one.
//!
//! `overridden` and `until` are **computed on every read, never stored** (the
//! house rule since card 151's `modified`), and travel in the page's state
//! ([`PageState`]).
//!
//! Local time is the container's (`TZ`, see the Dockerfile), read the way the
//! clock patches read it (`screeny_art::patch::local_now`). The clock is
//! injectable ([`Clock`]) so every rule here is tested without waiting for one.

use screeny_art::patch::PatchDef;
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use crate::page::StudioState;
use crate::player::PlayerChange;
use crate::state::{is_default_name, DEFAULT_SETTING};
use crate::AppState;

/// Modes a studio may have. The file is written whole; this keeps it small.
pub const MAX_MODES: usize = 32;
/// Entries a schedule may have: one every half hour, which is already more
/// than a room needs.
pub const MAX_ENTRIES: usize = 48;
/// Characters a mode's name may have, after trimming (the settings' bound).
pub const MAX_NAME_CHARS: usize = crate::state::MAX_NAME_CHARS;
/// How often the scheduler looks at the clock. An entry is applied within this
/// of its minute; there is also one look at start-up.
pub const TICK: Duration = Duration::from_secs(30);


// ------------------------------------------------------------- the model ---

/// What the panel should look like, by name.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Mode {
    pub name: String,
    /// A patch id. Kept even when this build has not got it (a patch can come
    /// back); such a mode is skipped when it comes due, and said.
    pub patch: String,
    /// A named setting of that patch, or `Default`. `None` is the patch's
    /// working copy: whatever it was last left at.
    pub setting: Option<String>,
    /// A panel brightness, always one of `page::brightness_stops()`. `None`
    /// leaves the brightness alone. `0` is allowed: a dark panel is a
    /// legitimate night.
    pub brightness: Option<u8>,
}

/// One line of the timetable: at this local time, every day, this mode.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    /// `"HH:MM"`, 24-hour, always two digits each (normalised on save).
    pub at: String,
    /// A mode's name, spelled as the mode spells it.
    pub mode: String,
}

/// The timetable. Entries are sorted by time and their times are unique.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Schedule {
    pub enabled: bool,
    pub entries: Vec<Entry>,
}

impl Schedule {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        !self.enabled && self.entries.is_empty()
    }
}

/// The last entry the scheduler applied: its time, its mode, and the **local
/// date of that occurrence** (for an entry applied before the day's first one,
/// that is yesterday). Persisted, which is what makes a hold survive a restart.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScheduleRun {
    pub at: String,
    pub mode: String,
    /// `"YYYY-MM-DD"`, local.
    pub day: String,
}

/// Everything card 302 keeps in the state file.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Plan {
    pub modes: Vec<Mode>,
    pub schedule: Schedule,
    pub run: Option<ScheduleRun>,
}

// ------------------------------------------------------------- the clock ---

/// A local moment, as the scheduler needs it: which day, which minute.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LocalNow {
    /// Days since 1970-01-01, local.
    pub days: i64,
    /// Minutes since local midnight, `0..1440`.
    pub minute: u16,
}

impl LocalNow {
    /// From local seconds since the epoch (`screeny_art::patch::local_now`).
    #[must_use]
    pub fn from_local_secs(secs: f64) -> LocalNow {
        let secs = secs.floor() as i64;
        let days = secs.div_euclid(86_400);
        let minute = (secs.rem_euclid(86_400) / 60) as u16;
        LocalNow { days, minute }
    }

    /// A given local date and time, for tests and for reading a run record.
    #[must_use]
    pub fn at(y: i64, m: u32, d: u32, hh: u16, mm: u16) -> LocalNow {
        LocalNow { days: days_from_civil(y, m, d), minute: hh * 60 + mm }
    }

    /// `"YYYY-MM-DD"`.
    #[must_use]
    pub fn date(&self) -> String {
        date_string(self.days)
    }
}

/// Where the scheduler reads the time. The system's local clock unless a test
/// says otherwise.
#[derive(Clone)]
pub struct Clock(Arc<dyn Fn() -> LocalNow + Send + Sync>);

impl Clock {
    /// The container's local time, read as the clock patches read it.
    #[must_use]
    pub fn system() -> Clock {
        Clock(Arc::new(|| LocalNow::from_local_secs(screeny_art::patch::local_now())))
    }

    /// Any function of nothing: a test's hand on the clock.
    pub fn from_fn(f: impl Fn() -> LocalNow + Send + Sync + 'static) -> Clock {
        Clock(Arc::new(f))
    }

    #[must_use]
    pub fn now(&self) -> LocalNow {
        (self.0)()
    }
}

impl Default for Clock {
    fn default() -> Self {
        Clock::system()
    }
}

impl std::fmt::Debug for Clock {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Clock({:?})", self.now())
    }
}

/// Days since 1970-01-01 of a civil date (Howard Hinnant's algorithm).
#[must_use]
pub fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let m = i64::from(m);
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + i64::from(d) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// `"YYYY-MM-DD"` of a day number.
#[must_use]
pub fn date_string(days: i64) -> String {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}")
}

/// `"7:05"` or `"07:05"` -> minutes since midnight.
///
/// # Errors
///
/// Anything that is not a 24-hour time, in words.
pub fn parse_at(text: &str) -> Result<u16, String> {
    let t = text.trim();
    let bad = || format!("`{t}` is not a time; write it as HH:MM, 24-hour (07:00, 22:30).");
    let (h, m) = t.split_once(':').ok_or_else(bad)?;
    if h.is_empty() || h.len() > 2 || m.len() != 2 || !h.bytes().chain(m.bytes()).all(|b| b.is_ascii_digit()) {
        return Err(bad());
    }
    let (h, m): (u16, u16) = (h.parse().map_err(|_| bad())?, m.parse().map_err(|_| bad())?);
    if h > 23 || m > 59 {
        return Err(bad());
    }
    Ok(h * 60 + m)
}

/// Minutes since midnight -> `"HH:MM"`.
#[must_use]
pub fn fmt_at(minute: u16) -> String {
    format!("{:02}:{:02}", minute / 60, minute % 60)
}

// ------------------------------------------------- the due-entry arithmetic ---

/// The entry due at a moment, and the local date of that occurrence.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Due {
    pub at: String,
    pub mode: String,
    pub day: String,
}

impl Due {
    #[must_use]
    pub fn run(&self) -> ScheduleRun {
        ScheduleRun { at: self.at.clone(), mode: self.mode.clone(), day: self.day.clone() }
    }
}

/// The entry due at `now`: the latest at or before it, and before the day's
/// first entry the last one, which came due **yesterday**. `None` only for a
/// schedule with no entries. (Whether the schedule is on is not asked here.)
///
/// A minute that does not exist on a day (the hour a DST change skips) needs
/// no code: at the first minute that does exist, the skipped entry is the
/// latest one at or before it, so it comes due then.
#[must_use]
pub fn due(schedule: &Schedule, now: LocalNow) -> Option<Due> {
    let timed: Vec<(u16, &Entry)> = schedule.entries.iter().filter_map(|e| parse_at(&e.at).ok().map(|m| (m, e))).collect();
    let today = timed.iter().filter(|(m, _)| *m <= now.minute).max_by_key(|(m, _)| *m);
    let (minute, entry, days) = match today {
        Some((m, e)) => (*m, *e, now.days),
        None => {
            let (m, e) = timed.iter().max_by_key(|(m, _)| *m)?;
            (*m, *e, now.days - 1)
        }
    };
    Some(Due { at: fmt_at(minute), mode: entry.mode.clone(), day: date_string(days) })
}

/// The time of the next entry after `now`, wrapping to tomorrow's first. With
/// one entry that is the same time tomorrow.
#[must_use]
pub fn next_at(schedule: &Schedule, now: LocalNow) -> Option<String> {
    let mut times: Vec<u16> = schedule.entries.iter().filter_map(|e| parse_at(&e.at).ok()).collect();
    times.sort_unstable();
    let next = times.iter().find(|m| **m > now.minute).or_else(|| times.first())?;
    Some(fmt_at(*next))
}

/// **The scheduler's one decision**: the entry to apply now, if any.
///
/// The due entry, when the schedule is on and that entry's identity (time +
/// date) is not the run record's. Nothing about what is playing is looked at:
/// that is the hold rule.
///
/// One more case: when the run record is **today and later than now**, the
/// local clock has gone backwards past it - the hour a DST change repeats, or
/// someone setting the clock - and nothing fires until the clock catches up
/// again. That is what makes an entry in a repeated hour fire once: without
/// it, an entry at 01:15 applied, then one at 01:45, would see 01:15 due again
/// on the second pass through 01:xx.
#[must_use]
pub fn to_fire(plan: &Plan, now: LocalNow) -> Option<Due> {
    if !plan.schedule.enabled {
        return None;
    }
    let due = due(&plan.schedule, now)?;
    if let Some(run) = &plan.run {
        if run.day == now.date() && parse_at(&run.at).is_ok_and(|m| now.minute < m) {
            return None;
        }
        if run.at == due.at && run.day == due.day {
            return None;
        }
    }
    Some(due)
}

// ---------------------------------------------------------------- validation ---

/// A mode's name as it will be stored, or the sentence to show.
///
/// # Errors
///
/// Empty, too long, or with a control character in it.
pub fn check_mode_name(name: &str) -> Result<String, String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("A mode needs a name.".into());
    }
    let chars = name.chars().count();
    if chars > MAX_NAME_CHARS {
        return Err(format!("A mode's name can be at most {MAX_NAME_CHARS} characters; that one is {chars}."));
    }
    if name.chars().any(char::is_control) {
        return Err("A mode's name cannot have line breaks or control characters in it.".into());
    }
    Ok(name.to_string())
}

/// The nearest real brightness stop to `level` (card 187): a mode never asks
/// for a value that lands on a picture its neighbour already showed. A tie
/// goes to the dimmer stop.
///
/// **Zero is dark and nothing else is.** A nonzero level snaps to the nearest
/// *nonzero* stop, because that is what the firmware does with it (it raises
/// anything dim-but-nonzero to its floor, card 136/187): `1` means "the
/// dimmest the panel can show", which is exactly the owner's "lowest possible
/// visible brightness", and must not round down to a dark panel.
#[must_use]
pub fn snap_brightness(level: u8) -> u8 {
    if level == 0 {
        return 0;
    }
    crate::page::brightness_stops()
        .into_iter()
        .filter(|s| *s > 0)
        .min_by_key(|s| (i16::from(*s) - i16::from(level)).unsigned_abs())
        .unwrap_or(level)
}

impl Plan {
    /// The mode called `name`, however it is spelled.
    #[must_use]
    pub fn mode(&self, name: &str) -> Option<&Mode> {
        let name = name.trim();
        self.modes.iter().find(|m| m.name.eq_ignore_ascii_case(name))
    }

    /// Save a mode: a new one, or - by the same name in any spelling - over the
    /// one that has it, which then takes the new spelling everywhere.
    ///
    /// The caller has already checked the patch and the setting (they need the
    /// studio's memory); this checks the name, the bound and the brightness.
    ///
    /// # Errors
    ///
    /// A name no mode may have, or [`MAX_MODES`] already.
    pub fn save_mode(&mut self, mode: Mode) -> Result<String, String> {
        let name = check_mode_name(&mode.name)?;
        let mode = Mode { name: name.clone(), brightness: mode.brightness.map(snap_brightness), ..mode };
        if let Some(i) = self.modes.iter().position(|m| m.name.eq_ignore_ascii_case(&name)) {
            let old = std::mem::replace(&mut self.modes[i], mode);
            if old.name != name {
                self.rename_references(&old.name, &name);
            }
        } else {
            if self.modes.len() >= MAX_MODES {
                return Err(format!("There are already {MAX_MODES} modes, which is as many as a studio may have."));
            }
            self.modes.push(mode);
        }
        Ok(name)
    }

    /// Rename a mode; the schedule and the run record follow it.
    ///
    /// # Errors
    ///
    /// No such mode, a name no mode may have, or one another mode has.
    pub fn rename_mode(&mut self, from: &str, to: &str) -> Result<String, String> {
        let i = self.index(from)?;
        let to = check_mode_name(to)?;
        if let Some(clash) = self.modes.iter().enumerate().find(|(j, m)| *j != i && m.name.eq_ignore_ascii_case(&to)) {
            return Err(format!("There is already a mode called `{}`.", clash.1.name));
        }
        let old = std::mem::replace(&mut self.modes[i].name, to.clone());
        self.rename_references(&old, &to);
        Ok(to)
    }

    /// Delete a mode - **not one the schedule names**: that is refused, with
    /// where it is named, rather than leaving the schedule pointing at nothing.
    ///
    /// # Errors
    ///
    /// No such mode, or the schedule names it.
    pub fn delete_mode(&mut self, name: &str) -> Result<String, String> {
        let i = self.index(name)?;
        let name = self.modes[i].name.clone();
        let at: Vec<&str> = self.schedule.entries.iter().filter(|e| e.mode == name).map(|e| e.at.as_str()).collect();
        if !at.is_empty() {
            return Err(format!(
                "`{name}` is in the schedule at {}; take it out of the schedule first.",
                at.join(", ")
            ));
        }
        self.modes.remove(i);
        Ok(name)
    }

    /// Replace the timetable. Every entry's time is parsed and normalised,
    /// every mode must exist (and takes the mode's own spelling), times are
    /// unique, and the list is sorted.
    ///
    /// # Errors
    ///
    /// Too many entries, a time that is not one, a mode there is not, or two
    /// entries at the same time.
    pub fn set_schedule(&mut self, enabled: bool, entries: &[Entry]) -> Result<(), String> {
        if entries.len() > MAX_ENTRIES {
            return Err(format!("A schedule can have at most {MAX_ENTRIES} entries; that one has {}.", entries.len()));
        }
        let mut clean: Vec<(u16, Entry)> = Vec::with_capacity(entries.len());
        for e in entries {
            let minute = parse_at(&e.at)?;
            let mode = self.mode(&e.mode).ok_or_else(|| format!("There is no mode called `{}`.", e.mode.trim()))?.name.clone();
            if clean.iter().any(|(m, _)| *m == minute) {
                return Err(format!("Two entries are at {}; one time, one mode.", fmt_at(minute)));
            }
            clean.push((minute, Entry { at: fmt_at(minute), mode }));
        }
        clean.sort_by_key(|(m, _)| *m);
        self.schedule = Schedule { enabled, entries: clean.into_iter().map(|(_, e)| e).collect() };
        Ok(())
    }

    fn index(&self, name: &str) -> Result<usize, String> {
        let name = name.trim();
        self.modes
            .iter()
            .position(|m| m.name.eq_ignore_ascii_case(name))
            .ok_or_else(|| format!("There is no mode called `{name}`."))
    }

    fn rename_references(&mut self, from: &str, to: &str) {
        for e in &mut self.schedule.entries {
            if e.mode == from {
                e.mode = to.to_string();
            }
        }
        if let Some(run) = &mut self.run {
            if run.mode == from {
                run.mode = to.to_string();
            }
        }
    }
}

// ------------------------------------------------------ reading the file ---

/// Read card 302's three keys out of a state file as forgivingly as the rest
/// of it (card 106's rule): one bad mode costs that mode, one bad entry that
/// entry, and never the file.
pub fn clean(
    modes: Option<&serde_json::Value>,
    schedule: Option<&serde_json::Value>,
    run: Option<&serde_json::Value>,
    repaired: &mut Vec<String>,
) -> Plan {
    let mut plan = Plan::default();
    match modes {
        None | Some(serde_json::Value::Null) => {}
        Some(serde_json::Value::Array(list)) => {
            for v in list {
                let Ok(mode) = serde_json::from_value::<Mode>(v.clone()) else {
                    repaired.push(format!("a mode that could not be read was dropped: {v}"));
                    continue;
                };
                let name = mode.name.clone();
                if plan.mode(&name).is_some() {
                    repaired.push(format!("there were two modes called `{}`; the second was dropped", name.trim()));
                    continue;
                }
                if let Err(why) = plan.save_mode(mode) {
                    repaired.push(format!("the mode `{}` was dropped: {why}", name.trim()));
                }
            }
        }
        Some(_) => repaired.push("the modes were not a list; forgotten".into()),
    }
    match schedule {
        None | Some(serde_json::Value::Null) => {}
        Some(v) => {
            let enabled = v.get("enabled").and_then(serde_json::Value::as_bool).unwrap_or(false);
            let mut entries = Vec::new();
            if let Some(serde_json::Value::Array(list)) = v.get("entries") {
                for e in list {
                    let Ok(entry) = serde_json::from_value::<Entry>(e.clone()) else {
                        repaired.push(format!("a schedule entry that could not be read was dropped: {e}"));
                        continue;
                    };
                    // One at a time, so one bad entry costs only itself.
                    let mut trial = entries.clone();
                    trial.push(entry.clone());
                    let mut probe = Plan { modes: plan.modes.clone(), ..Plan::default() };
                    match probe.set_schedule(enabled, &trial) {
                        Ok(()) => entries = trial,
                        Err(why) => repaired.push(format!("the schedule entry {} `{}` was dropped: {why}", entry.at, entry.mode)),
                    }
                }
            }
            // Cannot fail: every entry kept has just passed.
            let _ = plan.set_schedule(enabled, &entries);
        }
    }
    plan.run = run.and_then(|v| serde_json::from_value::<ScheduleRun>(v.clone()).ok());
    plan
}

// ---------------------------------------------------------- the studio's ---

/// The plan, as the running studio holds it, and what the last application
/// had to say (runtime only: a note is about now, not about the file).
#[derive(Debug, Default)]
pub struct Book {
    plan: Mutex<Plan>,
    note: Mutex<Option<String>>,
}

impl Book {
    #[must_use]
    pub fn new(plan: Plan) -> Book {
        Book { plan: Mutex::new(plan), note: Mutex::new(None) }
    }

    /// The plan, to read or change. Changes are the caller's to publish and
    /// persist, like every other change in the studio.
    pub fn plan(&self) -> MutexGuard<'_, Plan> {
        self.plan.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    #[must_use]
    pub fn snapshot(&self) -> Plan {
        self.plan().clone()
    }

    #[must_use]
    pub fn note(&self) -> Option<String> {
        self.note.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone()
    }

    pub fn set_note(&self, note: Option<String>) {
        *self.note.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = note;
    }
}

/// What the page is told: the player's state, flat, with card 302's fields
/// beside it. On the wire this *is* the studio's state - one object - and
/// every route and socket message that carried `StudioState` carries this.
///
/// (A wrapper rather than new fields on `StudioState` because that struct is
/// built in `player.rs`; `flatten` makes the difference invisible to a reader.)
#[derive(Clone, Debug, Serialize)]
pub struct PageState {
    #[serde(flatten)]
    pub studio: StudioState,
    /// Every mode, in the order they were made.
    pub modes: Vec<Mode>,
    /// The timetable and its switch.
    pub schedule: Schedule,
    /// The mode the schedule says should be on now; `null` when the schedule
    /// is off or empty.
    pub mode: Option<String>,
    /// The schedule is on and what is playing is not what its due mode says -
    /// by patch, by named setting or by brightness. Computed, never stored.
    pub overridden: bool,
    /// When the next entry comes due, `"HH:MM"`; `null` when the schedule is
    /// off or empty.
    pub until: Option<String>,
    /// What the last application of a mode had to say: a mode skipped because
    /// its patch cannot play here, a setting that is not there any more.
    pub schedule_note: Option<String>,
}

/// Build the page's state for the focused player.
#[must_use]
pub fn page_state(st: &AppState) -> PageState {
    let player = st.page();
    let studio = player.state();
    let plan = st.book.snapshot();
    let now = st.cfg.clock.now();
    let (mode, overridden, until) = if plan.schedule.enabled {
        let due = due(&plan.schedule, now);
        let overridden = due.as_ref().and_then(|d| plan.mode(&d.mode)).is_some_and(|m| differs(st, m, &studio));
        (due.map(|d| d.mode), overridden, next_at(&plan.schedule, now))
    } else {
        (None, false, None)
    };
    PageState {
        studio,
        modes: plan.modes,
        schedule: plan.schedule,
        mode,
        overridden,
        until,
        schedule_note: st.book.note(),
    }
}

/// Is the focused player somewhere other than where `mode` would put it?
fn differs(st: &AppState, mode: &Mode, now: &StudioState) -> bool {
    let Some(def) = crate::player::find_patch(&mode.patch, st.cfg.fault_patches) else {
        // A mode that cannot play here is skipped; there is nothing to be
        // overriding.
        return false;
    };
    if unplayable(def).is_some() {
        return false;
    }
    if now.patch != def.id {
        return true;
    }
    if let Some(name) = resolve_setting(st, def, mode.setting.as_deref()).0 {
        if !now.setting.eq_ignore_ascii_case(&name) || now.modified {
            return true;
        }
    }
    if let Some(want) = mode.brightness {
        let player = st.page();
        let mut want = snap_brightness(want);
        // A panel whose own cap is below what the mode asks for has the policy
        // pulled down to the cap (`Player::brightness_applied`); that is the
        // mode, as well as this panel can do it, not an override.
        if want > 0 {
            if let Some(cap) = player.status().health.brightness_cap {
                want = want.min(cap);
            }
        }
        if player.stored().brightness != Some(want) {
            return true;
        }
    }
    false
}

/// Why a patch cannot play in this process, or `None`.
fn unplayable(def: &PatchDef) -> Option<String> {
    if screeny_art::patches::needs_gpu(def.id) && !screeny_art::gpu_status().available {
        return Some(format!("`{}` needs a graphics adapter and this studio has none", def.name));
    }
    None
}

/// The setting to load for a mode, and a sentence when it had to fall back.
///
/// `None` in is the working copy, and so is a setting that has since been
/// deleted - said, never an error.
fn resolve_setting(st: &AppState, def: &PatchDef, setting: Option<&str>) -> (Option<String>, Option<String>) {
    let Some(name) = setting.map(str::trim).filter(|n| !n.is_empty()) else {
        return (None, None);
    };
    if is_default_name(name) {
        return (Some(DEFAULT_SETTING.to_string()), None);
    }
    match st.memory.setting_names(def.id).into_iter().find(|n| n.eq_ignore_ascii_case(name)) {
        Some(found) => (Some(found), None),
        None => (
            None,
            Some(format!("`{}` has no setting called `{name}` any more, so it is playing as it was last left", def.name)),
        ),
    }
}

/// Put the focused player into a mode: **one** `Player::configure` - the
/// patch, then the setting, then the brightness policy - so the panel moves in
/// one step. Returns what had to be said on the way, if anything.
///
/// `scheduled` is true when the timetable did it, false for a hand (the page,
/// `/mode/apply`, the smart home). The run record is not this function's
/// business: only the scheduler and "back to schedule" write it.
///
/// # Errors
///
/// No such mode, or its patch cannot play here - "skipped", in a sentence.
pub fn apply(st: &AppState, name: &str, scheduled: bool) -> Result<Option<String>, String> {
    let mode = st.book.plan().mode(name).cloned().ok_or_else(|| format!("There is no mode called `{}`.", name.trim()))?;
    let def = crate::player::find_patch(&mode.patch, st.cfg.fault_patches)
        .ok_or_else(|| format!("`{}` plays `{}`, which is not a patch this build has; skipped.", mode.name, mode.patch))?;
    if let Some(why) = unplayable(def) {
        return Err(format!("`{}` plays {why}; skipped.", mode.name));
    }
    let (load_setting, note) = resolve_setting(st, def, mode.setting.as_deref());
    let change = PlayerChange {
        patch: Some(def.id.to_string()),
        load_setting,
        brightness: mode.brightness.map(|b| Some(snap_brightness(b))),
        ..PlayerChange::default()
    };
    // A scheduled change fades over 5 s; a hand change keeps the manual fade
    // (`None`, 2 s). `PlayerChange` has no `fade` until card 304 lands.
    let _fade = scheduled.then_some(5.0_f32);
    let player = st.page();
    // card 304: pass fade Some(5.0) here
    player.configure(&change)?;
    player.ensure_running();
    Ok(note.map(|n| format!("`{}`: {n}.", mode.name)))
}

/// Apply `due`, write the run record, and say what happened - once, in the log
/// and on the page. Returns whether the mode was applied.
fn run_due(st: &AppState, due: &Due) -> Result<(), String> {
    let outcome = apply(st, &due.mode, true);
    // Recorded whether it played or was skipped: a skipped entry is said once
    // and not retried every tick until the next one.
    st.book.plan().run = Some(due.run());
    match &outcome {
        Ok(note) => {
            eprintln!("studio: schedule: {} `{}`{}", due.at, due.mode, note.as_ref().map_or(String::new(), |n| format!(" ({n})")));
            st.book.set_note(note.clone());
        }
        Err(why) => {
            eprintln!("studio: schedule: {} `{}`: {why}", due.at, due.mode);
            st.book.set_note(Some(why.clone()));
        }
    }
    st.publish_state(None, st.page_state());
    st.persist();
    outcome.map(|_| ())
}

/// One look at the clock: apply the due entry if it has not been applied.
/// Returns whether anything came due.
pub fn tick(st: &AppState) -> bool {
    let now = st.cfg.clock.now();
    let Some(due) = to_fire(&st.book.plan(), now) else { return false };
    let _ = run_due(st, &due);
    true
}

/// "Back to schedule": apply the entry due now, whatever the run record says,
/// and make it the run record.
///
/// # Errors
///
/// The schedule is off or empty, or the due mode cannot play here.
pub fn resume(st: &AppState) -> Result<(), String> {
    let now = st.cfg.clock.now();
    let due = {
        let plan = st.book.plan();
        if !plan.schedule.enabled {
            return Err("The schedule is off, so there is nothing to go back to.".into());
        }
        due(&plan.schedule, now).ok_or_else(|| "The schedule has no entries, so there is nothing to go back to.".to_string())?
    };
    run_due(st, &due)
}

/// The scheduler: once at start-up (the players are up by then), and every
/// `schedule_every` (30 s) after.
pub fn spawn(st: AppState) {
    let mut stop = st.stop.clone();
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(st.cfg.schedule_every);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                // The first tick of an interval is immediate: that is the look
                // at start-up.
                _ = ticker.tick() => { tick(&st); }
                () = async { drop(stop.wait_for(|s| *s).await) } => break,
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan(entries: &[(&str, &str)]) -> Plan {
        let mut p = Plan {
            modes: vec![
                Mode { name: "Day".into(), patch: "flock".into(), setting: None, brightness: None },
                Mode { name: "Night".into(), patch: "vesta".into(), setting: None, brightness: Some(6) },
                Mode { name: "Late".into(), patch: "vesta".into(), setting: None, brightness: Some(0) },
            ],
            ..Plan::default()
        };
        let entries: Vec<Entry> = entries.iter().map(|(at, mode)| Entry { at: (*at).into(), mode: (*mode).into() }).collect();
        p.set_schedule(true, &entries).expect("a good schedule");
        p
    }

    fn at(hh: u16, mm: u16) -> LocalNow {
        LocalNow::at(2026, 9, 26, hh, mm)
    }

    /// Run the scheduler's decision at `now` and, if it fires, record it -
    /// what `tick` does, minus the player.
    fn step(p: &mut Plan, now: LocalNow) -> Option<String> {
        let due = to_fire(p, now)?;
        p.run = Some(due.run());
        Some(format!("{} {}", due.at, due.mode))
    }

    #[test]
    fn dates_round_trip() {
        assert_eq!(date_string(0), "1970-01-01");
        assert_eq!(LocalNow::at(2026, 9, 26, 0, 0).date(), "2026-09-26");
        assert_eq!(date_string(days_from_civil(2024, 2, 29) + 1), "2024-03-01");
        assert_eq!(date_string(days_from_civil(2027, 1, 1) - 1), "2026-12-31");
        let n = LocalNow::from_local_secs(86_400.0 * 3.0 + 3_600.0 * 22.0 + 60.0 * 5.0 + 59.0);
        assert_eq!(n, LocalNow { days: 3, minute: 22 * 60 + 5 });
    }

    #[test]
    fn times_are_parsed_strictly_and_written_two_digits() {
        assert_eq!(parse_at("07:00"), Ok(420));
        assert_eq!(parse_at(" 7:05 "), Ok(425));
        assert_eq!(parse_at("23:59"), Ok(1439));
        for bad in ["24:00", "7", "07:5", "07:60", "a:bc", "", "07-00", "-1:00", "007:00"] {
            let e = parse_at(bad).expect_err(bad);
            assert!(e.ends_with('.'), "{e}");
        }
        assert_eq!(fmt_at(425), "07:05");
    }

    /// The latest entry at or before now; and before the day's first, the
    /// last one - which came due yesterday.
    #[test]
    fn the_due_entry_wraps_to_yesterday() {
        let p = plan(&[("07:00", "Day"), ("22:00", "Night")]);
        let d = due(&p.schedule, at(12, 0)).expect("due");
        assert_eq!((d.at.as_str(), d.mode.as_str(), d.day.as_str()), ("07:00", "Day", "2026-09-26"));
        let d = due(&p.schedule, at(7, 0)).expect("due");
        assert_eq!(d.mode, "Day", "an entry is due on its own minute");
        let d = due(&p.schedule, at(6, 59)).expect("due");
        assert_eq!((d.at.as_str(), d.mode.as_str(), d.day.as_str()), ("22:00", "Night", "2026-09-25"));
        let d = due(&p.schedule, at(23, 0)).expect("due");
        assert_eq!((d.mode.as_str(), d.day.as_str()), ("Night", "2026-09-26"));
        assert_eq!(next_at(&p.schedule, at(12, 0)).as_deref(), Some("22:00"));
        assert_eq!(next_at(&p.schedule, at(23, 0)).as_deref(), Some("07:00"), "wrapping to tomorrow");
        assert_eq!(next_at(&p.schedule, at(22, 0)).as_deref(), Some("07:00"), "not the one due now");
        let one = plan(&[("22:00", "Night")]);
        assert_eq!(due(&one.schedule, at(3, 0)).expect("due").day, "2026-09-25");
        assert_eq!(next_at(&one.schedule, at(23, 0)).as_deref(), Some("22:00"));
        assert!(due(&plan(&[]).schedule, at(3, 0)).is_none());
    }

    /// The first look after start-up with no run record applies what is due;
    /// the next looks do nothing until the next entry.
    #[test]
    fn the_first_tick_applies_what_is_due_and_then_nothing_until_the_next() {
        let mut p = plan(&[("07:00", "Day"), ("22:00", "Night")]);
        assert_eq!(step(&mut p, at(12, 0)).as_deref(), Some("07:00 Day"));
        assert_eq!(step(&mut p, at(12, 0)), None);
        assert_eq!(step(&mut p, at(21, 59)), None);
        assert_eq!(step(&mut p, at(22, 0)).as_deref(), Some("22:00 Night"));
        assert_eq!(step(&mut p, at(23, 30)), None);
        // Past midnight: still yesterday's 22:00, so nothing.
        assert_eq!(step(&mut p, LocalNow::at(2026, 9, 27, 3, 0)), None);
        assert_eq!(step(&mut p, LocalNow::at(2026, 9, 27, 7, 0)).as_deref(), Some("07:00 Day"));
    }

    /// **Hold until the next entry**, including across a restart. The rule
    /// never looks at what is playing, so a hand change is simply not undone
    /// until an entry with a new identity comes due; and the run record is in
    /// the file, so a restart in between changes nothing.
    #[test]
    fn a_hand_change_holds_until_the_next_entry_even_across_a_restart() {
        let mut p = plan(&[("07:00", "Day"), ("22:00", "Night")]);
        assert_eq!(step(&mut p, at(22, 0)).as_deref(), Some("22:00 Night"));
        // 22:30: the owner puts Day back on by hand. Nothing here knows, and
        // nothing needs to: 23:00, 02:00 are the same identity.
        assert_eq!(step(&mut p, at(23, 0)), None);
        // A restart: the plan is what the file said, run record included.
        let mut restarted = Plan { run: p.run.clone(), ..plan(&[("07:00", "Day"), ("22:00", "Night")]) };
        assert_eq!(step(&mut restarted, LocalNow::at(2026, 9, 27, 2, 0)), None, "the hold survives a restart");
        assert_eq!(step(&mut restarted, LocalNow::at(2026, 9, 27, 7, 0)).as_deref(), Some("07:00 Day"));
    }

    /// A restart **across** an entry applies it at start-up: the due entry's
    /// identity is a new one.
    #[test]
    fn a_restart_across_an_entry_applies_it_at_start_up() {
        let mut p = plan(&[("07:00", "Day"), ("22:00", "Night")]);
        assert_eq!(step(&mut p, at(22, 0)).as_deref(), Some("22:00 Night"));
        // Down from 23:00 to 09:30 the next day.
        let mut restarted = Plan { run: p.run.clone(), ..plan(&[("07:00", "Day"), ("22:00", "Night")]) };
        assert_eq!(step(&mut restarted, LocalNow::at(2026, 9, 27, 9, 30)).as_deref(), Some("07:00 Day"));
        // Down across two entries: only the latest is applied, once.
        let mut long = Plan { run: p.run, ..plan(&[("07:00", "Day"), ("22:00", "Night")]) };
        assert_eq!(step(&mut long, LocalNow::at(2026, 9, 28, 23, 0)).as_deref(), Some("22:00 Night"));
        assert_eq!(step(&mut long, LocalNow::at(2026, 9, 28, 23, 1)), None);
    }

    /// Spring forward: 02:00 -> 03:00, so an entry at 02:30 has no minute that
    /// day. It is due at the first tick after the jump.
    #[test]
    fn an_entry_in_a_skipped_hour_fires_at_the_next_tick() {
        let mut p = plan(&[("02:30", "Late"), ("07:00", "Day"), ("22:00", "Night")]);
        let day = |hh, mm| LocalNow::at(2027, 3, 14, hh, mm);
        p.run = Some(ScheduleRun { at: "22:00".into(), mode: "Night".into(), day: "2027-03-13".into() });
        assert_eq!(step(&mut p, day(1, 59)), None);
        // The clock goes 01:59 -> 03:00.
        assert_eq!(step(&mut p, day(3, 0)).as_deref(), Some("02:30 Late"));
        assert_eq!(step(&mut p, day(3, 0)), None);
    }

    /// Fall back: 01:00-02:00 happens twice. Each entry in it fires once.
    #[test]
    fn an_entry_in_a_repeated_hour_fires_once() {
        let mut p = plan(&[("01:15", "Late"), ("01:45", "Night"), ("07:00", "Day")]);
        let day = |hh, mm| LocalNow::at(2026, 11, 1, hh, mm);
        p.run = Some(ScheduleRun { at: "07:00".into(), mode: "Day".into(), day: "2026-10-31".into() });
        assert_eq!(step(&mut p, day(1, 15)).as_deref(), Some("01:15 Late"));
        assert_eq!(step(&mut p, day(1, 45)).as_deref(), Some("01:45 Night"));
        // The clock goes 01:59 -> 01:00 and walks the hour again.
        for m in [0, 15, 20, 45, 59] {
            assert_eq!(step(&mut p, day(1, m)), None, "01:{m:02} the second time");
        }
        assert_eq!(step(&mut p, day(2, 0)), None);
        assert_eq!(step(&mut p, day(7, 0)).as_deref(), Some("07:00 Day"));
    }

    #[test]
    fn a_schedule_that_is_off_or_empty_does_nothing() {
        let mut p = plan(&[("07:00", "Day")]);
        p.schedule.enabled = false;
        assert!(to_fire(&p, at(8, 0)).is_none());
        assert!(to_fire(&plan(&[]), at(8, 0)).is_none());
    }

    /// Editing the schedule is not the clock going back: a new entry earlier
    /// than the run record, with now past both, comes due.
    #[test]
    fn a_new_schedule_applies_its_due_entry() {
        let mut p = plan(&[("07:00", "Day"), ("22:00", "Night")]);
        assert_eq!(step(&mut p, at(22, 10)).as_deref(), Some("22:00 Night"));
        let entries = vec![Entry { at: "07:00".into(), mode: "Day".into() }, Entry { at: "21:00".into(), mode: "Late".into() }];
        p.set_schedule(true, &entries).expect("ok");
        assert_eq!(step(&mut p, at(22, 30)).as_deref(), Some("21:00 Late"));
    }

    #[test]
    fn modes_are_named_bounded_and_unique_however_they_are_spelled() {
        let mut p = Plan::default();
        let m = |name: &str| Mode { name: name.into(), patch: "flock".into(), setting: None, brightness: None };
        assert_eq!(p.save_mode(m("  Night  ")), Ok("Night".into()));
        assert_eq!(p.modes.len(), 1);
        // The same name in another spelling is the same mode, and takes it.
        assert_eq!(p.save_mode(m("NIGHT")), Ok("NIGHT".into()));
        assert_eq!(p.modes.len(), 1);
        assert_eq!(p.modes[0].name, "NIGHT");
        assert!(p.save_mode(m("   ")).unwrap_err().ends_with('.'));
        assert!(p.save_mode(m(&"x".repeat(41))).is_err());
        assert!(p.save_mode(m(&"x".repeat(40))).is_ok());
        assert!(p.save_mode(m("a\nb")).is_err());
        for i in p.modes.len()..MAX_MODES {
            p.save_mode(m(&format!("m{i}"))).expect("room");
        }
        let full = p.save_mode(m("one more")).unwrap_err();
        assert!(full.contains(&MAX_MODES.to_string()), "{full}");
        // Overwriting an existing one is still fine when full.
        assert!(p.save_mode(m("night")).is_ok());
    }

    #[test]
    fn brightness_is_snapped_to_a_real_stop() {
        let stops = crate::page::brightness_stops();
        assert_eq!(snap_brightness(0), 0, "dark is a stop");
        assert_eq!(snap_brightness(1), stops[1], "dim-but-on is the dimmest visible stop, never dark");
        assert!(stops.contains(&snap_brightness(97)));
        assert!(stops.contains(&snap_brightness(255)));
        assert_eq!(snap_brightness(stops[1]), stops[1], "the dimmest visible stop is itself");
        let mut p = Plan::default();
        p.save_mode(Mode { name: "Dim".into(), patch: "vesta".into(), setting: None, brightness: Some(97) }).expect("ok");
        assert!(stops.contains(&p.modes[0].brightness.expect("kept")));
    }

    #[test]
    fn a_mode_the_schedule_names_cannot_be_deleted_and_a_rename_follows_it() {
        let mut p = plan(&[("07:00", "Day"), ("22:00", "Night")]);
        p.run = Some(ScheduleRun { at: "22:00".into(), mode: "Night".into(), day: "2026-09-26".into() });
        let e = p.delete_mode("night").unwrap_err();
        assert!(e.contains("22:00") && e.ends_with('.'), "{e}");
        assert_eq!(p.delete_mode("Late"), Ok("Late".into()));
        assert!(p.delete_mode("Late").unwrap_err().contains("no mode"));
        assert_eq!(p.rename_mode("night", "Sleep"), Ok("Sleep".into()));
        assert_eq!(p.schedule.entries[1].mode, "Sleep", "the schedule follows the name");
        assert_eq!(p.run.as_ref().expect("run").mode, "Sleep", "and so does the run record");
        assert!(p.rename_mode("Sleep", "day").unwrap_err().contains("already"));
        assert_eq!(p.rename_mode("Sleep", "SLEEP"), Ok("SLEEP".into()), "a change of case is not a clash with itself");
    }

    #[test]
    fn a_schedule_is_validated_normalised_and_sorted() {
        let mut p = plan(&[]);
        let e = |at: &str, mode: &str| Entry { at: at.into(), mode: mode.into() };
        p.set_schedule(true, &[e("22:00", "night"), e("7:00", "Day")]).expect("ok");
        assert_eq!(p.schedule.entries, vec![e("07:00", "Day"), e("22:00", "Night")]);
        assert!(p.set_schedule(true, &[e("07:00", "Day"), e("7:00", "Night")]).unwrap_err().contains("07:00"));
        assert!(p.set_schedule(true, &[e("07:00", "Nope")]).unwrap_err().contains("Nope"));
        assert!(p.set_schedule(true, &[e("25:00", "Day")]).is_err());
        let many: Vec<Entry> = (0..=MAX_ENTRIES as u16).map(|i| e(&fmt_at(i * 10), "Day")).collect();
        assert!(p.set_schedule(true, &many).unwrap_err().contains(&MAX_ENTRIES.to_string()));
        assert_eq!(p.schedule.entries.len(), 2, "a refused schedule changes nothing");
        assert!(p.set_schedule(false, &many[..MAX_ENTRIES]).is_ok());
    }

    #[test]
    fn a_hand_edited_plan_costs_only_what_is_wrong() {
        let modes = serde_json::json!([
            { "name": "Day", "patch": "flock", "setting": null, "brightness": null },
            { "name": "day", "patch": "vesta", "setting": null, "brightness": null },
            { "name": "", "patch": "vesta", "setting": null, "brightness": null },
            { "name": "Night", "patch": "vesta", "setting": "Dim", "brightness": 97 },
            "rubbish"
        ]);
        let schedule = serde_json::json!({ "enabled": true, "entries": [
            { "at": "22:00", "mode": "Night" },
            { "at": "7:00", "mode": "Day" },
            { "at": "08:00", "mode": "Gone" },
            { "at": "22:00", "mode": "Day" },
            { "at": 7 }
        ]});
        let run = serde_json::json!({ "at": "22:00", "mode": "Night", "day": "2026-09-25" });
        let mut repaired = Vec::new();
        let p = clean(Some(&modes), Some(&schedule), Some(&run), &mut repaired);
        assert_eq!(p.modes.iter().map(|m| m.name.as_str()).collect::<Vec<_>>(), ["Day", "Night"]);
        assert!(crate::page::brightness_stops().contains(&p.modes[1].brightness.expect("kept")));
        assert!(p.schedule.enabled);
        assert_eq!(p.schedule.entries.iter().map(|e| e.at.as_str()).collect::<Vec<_>>(), ["07:00", "22:00"]);
        assert_eq!(p.run.as_ref().expect("run").day, "2026-09-25");
        assert_eq!(repaired.len(), 6, "{repaired:#?}");
    }
}
