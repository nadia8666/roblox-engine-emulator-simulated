//! The Issues list: code-flow, allocation and frame problems named from measured patterns only. Each issue states the
//! numbers it rests on and a price. A link to the view and line that show it, and a note for every check that could not
//! run on this profile (never a guess), complete the list.
//!
//! # The cost model
//!
//! Prices are milliseconds per physical frame of the reported phases. Every price is one of three kinds, and the issue
//! says which one beside the price:
//! - **Exact.** The work the clock charged at identified sites, less the cost of a known replacement. Only exact
//!   prices are clock savings, and only they count in totals and in the strongest-saving claim.
//! - **Estimated.** Work the clock did not charge, taken from the calibration and shown with its low and high spread
//!   (`estimate_range`). It stays out of clock totals and out of the strongest-saving claim, even when it is larger
//!   than the function's charged work.
//! - **Bounded.** A price whose full effect the profile cannot see. There are two kinds:
//!   - A source edit whose setup instructions cannot be recovered saves anywhere from zero to its price (`at_most`).
//!   - A source edit can change more than the site the profile recorded. Removing `table.sort(Build())` also removes the
//!     call to `Build`, which the profile books to another function. So the issue states the recorded part as its price
//!     and, beside it, an upper bound (`upper_ms`) for the complete edit.
//!
//!   The second kind is the default for every advised source edit: sort removals, repeat reads, unchanged writes, cache
//!   fixes, environment fixes and builtin rewrites. Its bound is all the scoped code work, and a recorded part that is
//!   zero or negative does not lower it. Only a rewrite that keeps every original operand and adds nothing else is bounded
//!   by its site price alone, and only it may say that fixing it saves nothing. [`edit_reach`] is the allowlist of these
//!   rewrites, with the reason for each: `string.byte(s)` to `string.byte(s, 1)`, and `string.sub(s, i)` to
//!   `string.sub(s, i, #s)`. Any other rewrite, and any advice added later, takes the default.
//!
//!   A recorded part is never sold as the complete source edit.
//!
//! - **Work set.** A fix is described by the work it removes, a [`WorkSet`]: call-tree nodes it removes whole (a node's
//!   own work), and charged sites it changes (the site's charged work less what the replacement costs). Work is
//!   identified by node and by site, never by function: that a function did a lot of work says nothing about which of
//!   it two fixes share.
//! - **Price.** An issue's price is its standalone saving: the work set's total over the reported frames, divided by
//!   their number. It is the complete observed mean, however the work is spread over frames, and it is the same however
//!   long the run is. Moving work to another frame saves nothing; removing it, keeping a result until its input
//!   changes, or doing it before the reported phases saves it.
//! - **Cadence.** How often a function was active, and its largest frame, describe an issue in words. They never
//!   change a price.
//! - **Events.** A function active in one frame only is a one-off: it is priced at its mean like any other, and it also
//!   says what it cost in that frame. A hitch (a frame over budget) claims no work: its functions' own issues do.
//! - **Floor.** Every priced fix is listed from the same absolute floor, [`LIST_FLOOR_MS`], whatever the run's work or
//!   length. A fix under it is still listed when its largest frame reaches [`ONE_OFF_FLOOR`] of a frame.
//! - **Overlap.** Fixes can contain each other (a sort's price includes its comparator's declined builtin calls). Each
//!   keeps its standalone price and names the overlap, but the totals count the union of the work sets:
//!   [`Issue::counted_ms`] is only the work no earlier fix had already booked.
//! - **Closing row.** One last row accounts for the work no listed issue holds: the code's work less the union of the
//!   listed work sets. Every issue's `counted_ms` and the closing row add up to the measured work.
//! - **Severity.** A priced issue is as severe as the more severe of its own saving against a frame, and its own share
//!   of the work in frames that overran the budget ([`Issue::over_budget_ms`]), never another function's hitch. A hitch
//!   is graded by its frame's overrun.
//! - **Not priced.** A price needs every site to sit in one known call-tree context, with a saving no larger than that
//!   function's recorded work. Otherwise the issue says why it has no price and ranks below the priced ones.
//! - **Strongest saving.** The one issue that saves the most says "nothing elsewhere can save as much".

use std::collections::{BTreeMap, BTreeSet};

mod work;
pub use work::{SiteWork, WorkSet};
use work::Work;

use crate::{
    fmt,
    model::{Builtin, Kind, Profile, Scope},
    sites::{AllocSite, ENGINE, site_label},
    tree::Tree,
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    Critical,
    Serious,
    Warning,
    #[default]
    Info,
}

impl Severity {
    /// The icon beside the word (status colours never carry meaning alone)
    pub fn icon(self) -> &'static str {
        match self {
            Severity::Critical => "■",
            Severity::Serious => "▲",
            Severity::Warning => "●",
            Severity::Info => "○",
        }
    }

    pub fn word(self) -> &'static str {
        match self {
            Severity::Critical => "critical",
            Severity::Serious => "serious",
            Severity::Warning => "warning",
            Severity::Info => "info",
        }
    }

    /// The severity of `ms` of work every frame against a frame of `budget` ms: a tenth of it or more is critical, a
    /// fortieth serious, a two-hundredth a warning, less info
    pub fn of(ms: f64, budget: f64) -> Severity {
        let share = ms / budget.max(1e-9);
        if share >= 0.10 {
            Severity::Critical
        } else if share >= 0.025 {
            Severity::Serious
        } else if share >= 0.005 {
            Severity::Warning
        } else {
            Severity::Info
        }
    }

    /// The severity of a hitch: a frame's `frame_ms` of work against a frame of `budget` ms. Over by a whole frame or more
    /// (a frame dropped at least) is critical, by a quarter of one serious, at all a warning; within it, info
    pub fn of_hitch(frame_ms: f64, budget: f64) -> Severity {
        let over = (frame_ms - budget) / budget.max(1e-9);
        if over >= 1.0 {
            Severity::Critical
        } else if over >= 0.25 {
            Severity::Serious
        } else if over > 0.0 {
            Severity::Warning
        } else {
            Severity::Info
        }
    }
}

/// Where an issue's evidence is
#[derive(Clone, Debug, Default, PartialEq)]
pub enum Target {
    #[default]
    None,
    /// A function (Calls)
    Func(u32),
    /// A line of a function (Source)
    Line(u32, i64),
    /// A timeline row (Timeline)
    Frame(usize),
    /// An allocation site (Allocations)
    Site(u32, i64),
    /// A Luau inline cache site, by its index in `luau_caches.sites` (Caches)
    Cache(usize),
    /// A heap snapshot (Heap)
    Snapshot(usize),
    /// A variant of a comparison (Compare)
    Variant(String),
}

/// The `kind` of the closing row: the work no listed issue holds
const CLOSING_ROW_KIND: &str = "work below floor";

/// What an issue is, in the order the list ranks them within a severity
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Class {
    /// Has a price a frame
    Priced,
    /// A one-off or a hitch: a measured size with no rate
    Event,
    /// The profile cannot price it
    Unpriced,
    /// The closing row, always last
    ClosingRow,
}

#[derive(Clone, Debug, Default)]
pub struct Issue {
    /// The key of this issue's entry in the producer's `timeline.issue_stats`, which holds its complete frame summary
    pub summary_key: String,
    /// The replacement's setup instructions are unknown, so the fix saves anywhere from zero to `ms`
    pub at_most: bool,
    /// For a source edit's recorded part (`ms`): the most the complete edit can save, counting the work it also changes.
    /// Every advised source edit has one except an operand-preserving rewrite ([`edit_reach`])
    pub upper_ms: Option<f64>,
    /// What `upper_ms` adds to the subtotal, in words; empty says the caller's instructions
    pub bound_reason: &'static str,
    /// The calibration's low and high for the uncharged estimate, ms a frame. Never part of a clock saving
    pub estimate_range: Option<(f64, f64)>,
    pub severity: Severity,
    /// Which detector: `sort every frame`, `repeat reads`, ...
    pub kind: &'static str,
    pub title: String,
    /// The measured numbers, a line each
    pub evidence: Vec<String>,
    /// What would remove it, said as a condition where the profile cannot see the cause
    pub advice: String,
    pub target: Target,
    /// The price: the work the clock charged, ms a frame, with `at_most` and `upper_ms` saying how far to trust it.
    /// None when the profile cannot price it
    pub ms: Option<f64>,
    /// An operand-preserving rewrite costs at least as much on this profile's clock.
    pub no_saving: bool,
    /// `ms` is host time measured on this machine (an older profile's cache misses and declined builtin calls), not the
    /// work clock's: it claims no work, and is not weighed against the work clock's issues
    pub host_time: bool,
    /// The estimate of work the clock did not charge, ms a frame. It is kept apart from `ms`, the claims and the totals
    pub uncharged: f64,
    /// For an event: its frame, that frame's work (ms), and how many frames it stands for (a hitch: the frames that
    /// overrun)
    pub once: Option<f64>,
    pub frame_ms: Option<f64>,
    pub recurs: usize,
    /// For a function active once: its total work in that frame, beside its honest mean rate
    pub total_ms: Option<f64>,
    /// The charged saving by script function, derived from its node/site set. These explain ownership, not overlap
    pub claims: Vec<(u32, f64)>,
    pub work: WorkSet,
    pub active_frames: usize,
    pub reported_frames: f64,
    pub saving_ceiling: f64,
    /// The part of its claims that earlier fixes had already booked, ms a frame. The standalone price includes it; the
    /// totals do not
    pub overlap: f64,
    /// The work this issue newly books, ms a frame: its claims less its overlap. Events and the closing row book theirs
    /// too, so every issue's `counted_ms` added up is the measured work
    pub counted_ms: f64,
    /// Largest own contribution, in ms, to a frame whose total exceeds the recorded budget
    pub over_budget_ms: f64,
    /// What `ms` leaves out (or why there is none, or that it is another clock's), said beside it
    pub unpriced: Option<String>,
    /// Ranks issues with no price among themselves (bytes, counts)
    pub weight: f64,
}

impl Issue {
    /// Its price in words: `92.5 µs a frame`, `19.26 ms once (frame 60)`, `frame 60: 23.40 ms`, `not priced`
    pub fn cost_text(&self) -> String {
        if self.no_saving { return FIX_SAVES_NOTHING.into(); }
        if self.host_time && let Some(ms) = self.ms {
            return format!("estimated host time {} a frame (calibration spread unavailable); outside the work clock", fmt::ms(ms));
        }
        let only_an_estimate = self.ms.is_none() && self.uncharged > 0.0;
        if only_an_estimate {
            return self.estimate_text();
        }
        if self.bound_without_price().is_some() {
            return self.unpriced_line();
        }
        if let Some(ms) = self.ms {
            // (an upper bound is stated even when the price is also an `at_most`: the bound is the wider claim)
            if let Some(upper) = self.upper_ms {
                let remainder = self.bound_remainder("caller instructions");
                return format!("{} a frame in recorded work; at most {} including {remainder}", fmt::ms(ms), fmt::ms(upper));
            }
            if self.at_most {
                return format!("at most {} a frame", fmt::ms(ms));
            }
        }
        match (self.ms, self.total_ms, self.once, self.frame_ms) {
            (Some(ms), Some(total), Some(_), _) => format!("{} once in this run: {} a frame over {} frames", fmt::ms(total), fmt::ms(ms), self.reported_frames),
            (Some(ms), ..) => format!("{} a frame", fmt::ms(ms)),
            (None, Some(total), Some(frame), _) => format!("{} once (frame {frame})", fmt::ms(total)),
            (None, None, Some(frame), Some(frame_ms)) if self.unpriced.is_none() => format!("frame {frame}: {}", fmt::ms(frame_ms)),
            _ => "not priced".into(),
        }
    }

    /// Its price in a list's cell: `92.5 µs/f`, `19.26 ms once`, `+6.730 ms` (a hitch's overrun), `not priced`
    pub fn cost_short(&self, budget: f64) -> String {
        if self.no_saving { return "no saving".into(); }
        if self.host_time && let Some(ms) = self.ms {
            return format!("est. {}/f", fmt::ms(ms));
        }
        let only_an_estimate = self.ms.is_none() && self.uncharged > 0.0;
        if only_an_estimate {
            return format!("est. {}/f", fmt::ms(self.uncharged));
        }
        if let Some(upper) = self.bound_without_price() {
            return format!("≤{}/f", fmt::ms(upper));
        }
        if let Some(ms) = self.ms {
            // (an issue with an upper bound shows its price as a part, even when the price is also an `at_most`)
            if self.upper_ms.is_some() {
                return format!("part {}/f", fmt::ms(ms));
            }
            if self.at_most {
                return format!("≤{}/f", fmt::ms(ms));
            }
        }
        match (self.ms, self.total_ms, self.frame_ms) {
            (Some(ms), ..) => format!("{}/f", fmt::ms(ms)),
            (None, Some(total), _) => format!("{} once", fmt::ms(total)),
            (None, None, Some(frame_ms)) if self.unpriced.is_none() && self.once.is_some() => format!("+{}", fmt::ms((frame_ms - budget).max(0.0))),
            _ => "not priced".into(),
        }
    }

    /// The uncharged estimate in words, with the calibration's spread when the profile has one
    fn estimate_text(&self) -> String {
        let estimate = fmt::ms(self.uncharged);
        match self.estimate_range {
            Some((low, high)) => format!("estimated {estimate} a frame (calibration {}–{}); outside the work clock", fmt::ms(low), fmt::ms(high)),
            None => format!("estimated {estimate} a frame (calibration spread unavailable); outside the work clock"),
        }
    }

    /// What the upper bound adds to the recorded subtotal, in words: the issue's own reason, else `default`
    fn bound_remainder(&self, default: &'static str) -> &'static str {
        if self.bound_reason.is_empty() { default } else { self.bound_reason }
    }

    /// The complete edit's upper bound when the recorded site part has no price (it is missing, or saves nothing)
    fn bound_without_price(&self) -> Option<f64> {
        if self.ms.is_none() { self.upper_ms } else { None }
    }

    /// The most this issue could save. Its upper bound counts even when the site part has no price, because the
    /// complete edit can still save work outside the site. Host time is not work-clock time, so it counts as nothing.
    fn largest_possible_saving(&self) -> f64 {
        if self.host_time {
            return 0.0;
        }
        match (self.upper_ms, self.ms) {
            (Some(upper), _) => upper,
            (None, Some(ms)) => ms,
            (None, None) => 0.0,
        }
    }

    /// Whether this issue's price is exact, so it may be the one that says "nothing elsewhere can save as much". A bound
    /// never is: the complete edit might save less, or more, than the figure
    fn can_claim_strongest_saving(&self) -> bool {
        let is_exact = !self.host_time && !self.at_most && self.upper_ms.is_none();
        is_exact && !self.is_closing_row()
    }

    /// Whether this is the closing row that accounts for the work no listed issue holds
    fn is_closing_row(&self) -> bool {
        self.kind == CLOSING_ROW_KIND
    }

    fn class(&self) -> Class {
        if self.is_closing_row() {
            Class::ClosingRow
        } else if self.ms.is_some() {
            Class::Priced
        } else if self.once.is_some() && self.unpriced.is_none() {
            Class::Event
        } else {
            Class::Unpriced
        }
    }

    /// The line an unpriced issue's evidence starts with
    fn unpriced_line(&self) -> String {
        let reason = self.unpriced.as_deref().unwrap_or_else(|| unpriced_reason(self.kind));
        let Some(upper) = self.bound_without_price() else {
            return format!("not priced: {reason}");
        };
        let remainder = self.bound_remainder("caller instructions");
        format!("not priced: {reason}; complete edit at most {} a frame including {remainder}", fmt::ms(upper))
    }
}

/// What an issue says when its fix saves nothing, and may only say of an operand-preserving rewrite ([`edit_reach`])
const FIX_SAVES_NOTHING: &str = "fixing it saves nothing on this clock";

/// What any other source edit says when its recorded site part saves nothing: the complete edit keeps its bound
const SITE_PART_SAVES_NOTHING: &str = "the recorded site part saves nothing on this clock";

/// Why an issue of a kind has no price: what its check measures instead (its severity is that measure's)
fn unpriced_reason(kind: &str) -> &'static str {
    match kind {
        "steady allocation" | "allocation bursts" => "this check counts the bytes a frame allocates, not work: what making them costs the work clock and what the collector spends on them are not counted per site yet (its severity is by bytes)",
        "table growth" => "this check counts table storage allocations, not work (its severity is by regrowths a frame)",
        "gc spike" => "the collector's steps are counted, not priced on the work clock (its severity is by steps)",
        "growth" => "a count that keeps growing costs memory, not a frame's work (its severity is by the count a frame)",
        "heap growth" => "reachable bytes, not work (its severity is by bytes)",
        _ => "this check measures something other than a frame's work (its severity is its own measure)",
    }
}

/// The issues as the views list them: an unpriced issue's evidence starts by saying why it has no price; the priced issue
/// that saves the most work is told so (among every issue the work clock prices a frame); then [`rank`]
pub fn finish(issues: &mut [Issue]) {
    const MOST: &str = "; nothing elsewhere can save as much";
    for issue in issues.iter_mut() {
        if issue.advice.ends_with(MOST) { issue.advice.truncate(issue.advice.len() - MOST.len()); }
        if issue.class() == Class::Unpriced && !issue.evidence.first().is_some_and(|line| line.starts_with("not priced:")) {
            issue.evidence.insert(0, issue.unpriced_line());
        }
    }
    if let Some(index) = strongest_saving(issues) {
        issues[index].advice.push_str(MOST);
    }
    rank(issues);
}

/// The issue that saves the most work, if there is one that nothing else could beat. An issue qualifies when its price is
/// exact ([`Issue::can_claim_strongest_saving`]) and reaches the largest figure any other issue might save
/// ([`Issue::saving_ceiling`]). The larger price wins, and the earlier issue when prices are equal.
fn strongest_saving(issues: &[Issue]) -> Option<usize> {
    let mut strongest: Option<(usize, f64)> = None;
    for (index, issue) in issues.iter().enumerate() {
        if !issue.can_claim_strongest_saving() {
            continue;
        }
        let Some(ms) = issue.ms else { continue };
        let another_could_save_more = ms + 1e-12 < issue.saving_ceiling;
        if another_could_save_more {
            continue;
        }
        let beats_the_strongest = match strongest {
            None => true,
            Some((_, strongest_ms)) => ms.total_cmp(&strongest_ms).is_gt(),
        };
        if beats_the_strongest {
            strongest = Some((index, ms));
        }
    }
    strongest.map(|(index, _)| index)
}

pub fn rank(issues: &mut [Issue]) {
    let size = |issue: &Issue| match issue.class() {
        Class::Priced => issue.ms.unwrap_or(0.0),
        Class::Event => issue.total_ms.or(issue.frame_ms).unwrap_or(0.0),
        Class::Unpriced | Class::ClosingRow => 0.0,
    };
    issues.sort_by(|a, b| {
        let (a_class, b_class) = (a.class(), b.class());
        (a_class == Class::ClosingRow)
            .cmp(&(b_class == Class::ClosingRow))
            .then((a_class == Class::Unpriced).cmp(&(b_class == Class::Unpriced)))
            .then(a.severity.cmp(&b.severity))
            .then(a_class.cmp(&b_class))
            .then(size(b).total_cmp(&size(a)))
            .then(b.weight.total_cmp(&a.weight))
            .then(a.title.cmp(&b.title))
    });
}

/// `1 call`, `3 calls`, `2,000 frames`
fn counted(count: f64, one: &str, many: &str) -> String {
    format!("{} {}", fmt::grouped(count), if count.round() == 1.0 { one } else { many })
}

/// A site's allocation a frame below which it is not listed
const SMALL_BYTES: f64 = 256.0;

/// Every priced fix is listed from this much work a frame (ms): 0.25 % of a 60 fps frame, 41.7 µs. It is absolute, so it
/// does not move with the run's work or length. The closing row holds work outside the listed union
const LIST_FLOOR_MS: f64 = 1000.0 / 60.0 * 0.0025;
/// A fix below the mean floor is still displayed when its own peak reaches this share of the recorded frame budget
const ONE_OFF_FLOOR: f64 = 0.025;
/// The frames that overrun a frame named as hitches, the worst first
const HITCHES: usize = 3;
/// A profile from before ree recorded each function's work a frame: a frame stands out when its work is this many times
/// the other frames' mean and [`SPIKE_FLOOR`] of a frame above it
const BURST: f64 = 3.0;
const SPIKE_FLOOR: f64 = 0.025;
/// The fewest frames a check across frames reads
const MIN_FRAMES: f64 = 10.0;

/// What the checks read: the profile's reported phases, their frames, a frame's time at the run's rate, the work of the
/// code the issues cover in them, and the identity of its charged work. Every check takes its frames and
/// phases from here, never the run's
pub(crate) struct Scoped<'a> {
    pub(crate) p: &'a Profile,
    pub(crate) scope: Scope,
    /// The frames the reported phases ran in (None: the profile cannot count them)
    pub(crate) frames: Option<f64>,
    /// A frame at the run's rate (ms)
    pub(crate) budget: f64,
    /// The harness's code is set apart: what it does is left out
    pub(crate) harness_out: bool,
    /// The work of the code the issues cover in the reported phases (ms, all their frames): the game's when the harness's
    /// is apart, else all of the code's
    pub(crate) code_work: f64,
    /// What the run's clock multiplied desktop prices by (profile.json `work.units.scale`: a phone run's 2.77; 1 for a
    /// desktop run and before ree stated it). A price in ns (`work.prices`, boundary.json's costs) times it is the run's;
    /// the profile's work figures are the run's already
    pub(crate) scale: f64,
    work: Option<Work>,

}

impl Scoped<'_> {
    /// `the 120 frames of the steady phase`, `the 1 frame`
    fn frames_words(&self, frames: f64) -> String {
        let phases = match &self.scope.phases {
            Some(phases) if phases.len() == 1 => self.p.phase_words(phases[0]).map(|words| format!(" of {words}")).unwrap_or_default(),
            Some(_) => format!(" of {}", self.scope.label),
            None => String::new(),
        };
        format!("the {}{phases}", counted(frames, "frame", "frames"))
    }

    /// `the game's`, or `the code's` when the harness's is not set apart
    fn whose(&self) -> &'static str {
        if self.harness_out { "the game's" } else { "the code's" }
    }

    /// Every timeline row of the reported phases, including startup when explicitly requested
    pub(crate) fn rows(&self) -> Vec<usize> {
        let Some(timeline) = &self.p.raw.timeline else { return Vec::new() };
        let phases = timeline.values("phase").unwrap_or_default();
        (0..phases.len()).filter(|&row| self.scope.covers(phases[row] as u32)).collect()
    }

    /// A site's allocation a frame in the reported phases: (bytes, count), its figures in those phases over their frames
    /// (zero when the profile keeps a site's figures over the run only). (For the memory checks the memory rework is
    /// rewriting on it.)
    #[allow(dead_code)]
    pub(crate) fn site_per_frame(&self, site: &AllocSite) -> (f64, f64) {
        let Some(frames) = self.frames.filter(|frames| *frames > 0.0) else { return (0.0, 0.0) };
        let (bytes, count) = site.phases.iter().filter(|(phase, _)| self.scope.covers(**phase)).fold((0.0, 0.0), |sum, (_, (bytes, count))| (sum.0 + bytes, sum.1 + count));
        (bytes / frames, count / frames)
    }

    /// The reported phases' frames as a divisor: at least one, so a profile that cannot count them divides by one
    fn frame_count(&self) -> f64 {
        self.frames.unwrap_or(1.0).max(1.0)
    }

    /// Prices an issue from the work it removes. A fix given only as a figure for a function (a `claims` entry, or its
    /// target) becomes one site in that function's call-tree context. The price is then the work set's total, so an
    /// issue whose sites cannot be placed or exceed what their function did loses its price and says why.
    pub(crate) fn priced(&self, mut issue: Issue) -> Issue {
        let frames = self.frame_count();
        issue.reported_frames = frames;
        if let (Some(ms), Some(work)) = (issue.ms.filter(|_| !issue.host_time), &self.work) {            if issue.work.nodes.is_empty() && issue.work.sites.is_empty() && ms > 0.0 {
                self.place_claims_as_sites(&mut issue, ms, work);
            }
            check_sites_against_work(&mut issue, work);
            if issue.ms.is_some() {
                issue.ms = Some(issue.work.total(&work.amounts) / frames);
                issue.claims = self.claims_of(&issue, work);
            }
        }
        match issue.ms {
            Some(ms) => {
                issue.severity = Severity::of(ms, self.budget);
                let price_line = self.price_line(&issue, ms, frames);
                issue.evidence.insert(0, price_line);
            }
            None => {
                issue.claims.clear();
                issue.evidence.insert(0, issue.unpriced_line());
            }
        }
        if issue.uncharged > 0.0 { issue.evidence.push(issue.estimate_text()); }
        if !issue.claims.is_empty() {
            let named: Vec<String> = issue.claims.iter().map(|(func, ms)| format!("{} {} a frame", self.p.funcs[*func as usize].label().replace("  ", " "), fmt::ms(*ms))).collect();
            issue.evidence.push(format!("functions whose work this fix saves: {}", named.join("; ")));
        }
        issue
    }

    /// The evidence line that states a priced issue's price, with its bound when it has one
    fn price_line(&self, issue: &Issue, ms: f64, frames: f64) -> String {
        if let Some(upper) = issue.upper_ms {
            let remainder = issue.bound_remainder("unknown caller/receiver setup");
            return format!("recorded removable subtotal {} a frame; source fix bounded from zero to {} including {remainder}", fmt::ms(ms), fmt::ms(upper));
        }
        let qualifier = if issue.at_most { "at most " } else { "" };
        format!("saves {qualifier}{} a frame over {}; standalone recorded work, before overlaps", fmt::ms(ms), self.frames_words(frames))
    }

    /// An aggregate figure for a function is one site in the function's only context. With several contexts the profile
    /// does not say which one the saving is in, and it could overlap another fix, so the issue stays unpriced.
    fn place_claims_as_sites(&self, issue: &mut Issue, ms: f64, work: &Work) {
        let claims = if issue.claims.is_empty() {
            match issue.target {
                Target::Func(func) | Target::Line(func, _) | Target::Site(func, _) => vec![(func, ms)],
                _ => Vec::new(),
            }
        } else {
            issue.claims.clone()
        };
        for (func, saving_per_frame) in claims {
            let contexts = work.contexts.get(&(func as usize)).map(Vec::as_slice).unwrap_or_default();
            if contexts.len() != 1 {
                issue.ms = None;
                issue.unpriced = Some("needs per-context site records".into());
                break;
            }
            issue.work.sites.push(SiteWork {
                node: contexts[0],
                key: format!("{}:{}:{:?}", issue.kind, issue.title, issue.target),
                saving: saving_per_frame * self.frame_count(),
                ..SiteWork::default()
            });
        }
    }

    /// The work the fix saves in each function (ms a frame): the whole own work of the nodes it removes and the saving
    /// of its sites, less the replacement work each edited site pays. A function's claims are what the issue says the
    /// fix saves there; they are not used to find overlaps.
    fn claims_of(&self, issue: &Issue, work: &Work) -> Vec<(u32, f64)> {
        let frames = self.frame_count();
        let mut by_function: BTreeMap<u32, f64> = BTreeMap::new();
        for &node in &issue.work.nodes {
            if let Some(func) = work.owner[node] {
                *by_function.entry(func as u32).or_default() += work.amounts[node] / frames;
            }
        }
        for site in &issue.work.sites {
            if let Some(func) = work.owner[site.node] {
                *by_function.entry(func as u32).or_default() += site.saving / frames;
            }
        }
        for (fix, replacement_cost) in &issue.work.replacements {
            // (the replacement is paid in the first site of the edit that still needs it)
            let site = issue.work.sites.iter().find(|site| site.fix_key() == fix.as_str() && !issue.work.nodes.contains(&site.node));
            if let Some(func) = site.and_then(|site| work.owner[site.node]) {
                *by_function.entry(func as u32).or_default() -= replacement_cost / frames;
            }
        }
        by_function.into_iter().collect()
    }
}

/// Takes the price from an issue whose sites cannot be placed, or claim more than their function did. Both checks run
/// on every site, so the later reason replaces an earlier one, as the checks always have.
fn check_sites_against_work(issue: &mut Issue, work: &Work) {
    let mut saving_by_function: BTreeMap<usize, f64> = BTreeMap::new();
    for site in &issue.work.sites {
        match work.owner_of(site.node) {
            Some(func) => *saving_by_function.entry(func).or_default() += site.saving,
            None => {
                issue.ms = None;
                issue.unpriced = Some("the site has no charged script context in the reported phases".into());
            }
        }
    }
    for (func, saving) in saving_by_function {
        // (a tiny excess is the rounding of the call tree's microseconds)
        let rounding_slack = 1e-9;
        if saving > work.function_total(func) + rounding_slack {
            issue.ms = None;
            issue.unpriced = Some("the site's saving exceeds its function's recorded work".into());
        }
    }
}

/// `frame 60's 19.40 ms overruns a 16.67 ms frame by 2.730 ms`, `frame 60's 3.531 ms fits in a 16.67 ms frame`
fn hitch_words(frame: f64, frame_ms: f64, budget: f64) -> String {
    if frame_ms > budget {
        format!("frame {frame}'s {} overruns a {} frame by {}", fmt::ms(frame_ms), fmt::ms(budget), fmt::ms(frame_ms - budget))
    } else {
        format!("frame {frame}'s {} fits in a {} frame", fmt::ms(frame_ms), fmt::ms(budget))
    }
}

/// A script function of a module: the code the issues are about (not a C function, nor the engine's own Luau)
fn is_code(p: &Profile, func: usize) -> bool {
    p.funcs.get(func).is_some_and(|f| f.kind == Kind::Script && !f.module.is_empty())
}

/// The profile's issues, most severe first, and the patterns its kinds could not check
pub fn detect(p: &Profile, work: Option<&Tree>, sites: &[AllocSite]) -> (Vec<Issue>, Vec<String>) {
    let mut issues = Vec::new();
    let mut unchecked = Vec::new();
    let steady = p.steady_phase();
    if steady.is_none() && (work.is_some() || p.raw.alloc.is_some() || p.raw.timeline.is_some()) {
        unchecked.push("the steady state's sorts, allocation and growth: the run marks no phase after startup (the mount)".into());
    }

    // the harness's own code is not the game's: what it sorts, allocates, misses or reads again is left out, as ree's
    // summary leaves it out of its tables, unless the run's code is all the harness's
    let sides = p.scope_work().or_else(|| {
        work.map(|tree| {
            let maps = tree.sided(&p.scope(), |func| p.funcs.get(func as usize).filter(|f| f.kind == Kind::Script && !f.module.is_empty()).map(|f| f.harness));
            (maps[0].values().sum(), maps[1].values().sum())
        })
    });
    let harness_out = p.has_harness() && sides.is_some_and(|(game, harness)| crate::model::harness_apart(game, harness));
    let scope = p.scope();
    let frames = p.scope_frames(&scope);
    let code_work = sides.map_or(0.0, |(game, harness)| if harness_out { game } else { game + harness });
    let mut s = Scoped {
        p,
        scope,
        frames,
        budget: p.budget_ms(),
        harness_out,
        code_work,
        scale: p.price_scale(),
        work: None,
    };
    // (a what-if run's variant runs once, in one frame: what a frame's figures say does not apply to it; the comparison
    // reads it per call)
    let what_if = p.is_what_if();
    if what_if {
        unchecked.push(format!("hot functions, sorts every frame, bursts and counts that keep growing: {}", p.per_frame_unchecked().unwrap_or_default()));
    }
    if !what_if && work.is_some() {
        s.work = Some(measured_work(&s));
        sorts(&s, &mut issues, &mut unchecked);
    } else if !what_if {
        unchecked.push("hot functions and lines, sorts every frame: need work".into());
    }
    let frame_work = if what_if { None } else { FrameWork::of(&s, &mut unchecked) };
    // (the checks across the steady state's frames need ten of them: fewer say so, never nothing)
    let steady_short = steady.map(|(_, frames)| frames as f64).filter(|frames| *frames < MIN_FRAMES);
    if p.raw.alloc.is_some() {
        if let Some(frames) = steady_short {
            unchecked.push(format!("steady allocation: the steady phase holds {} ({MIN_FRAMES} or more needed)", counted(frames, "frame", "frames")));
        }
        steady_allocation(&s, sites, &mut issues, &mut unchecked);
        table_growth(&s, sites, &mut issues);
    } else {
        unchecked.push("allocation and table growth: need alloc".into());
    }
    match p.per_frame_unchecked() {
        Some(_) if what_if => {}
        Some(why) => unchecked.push(format!("collector spikes and counts that keep growing: {why}")),
        None => {
            if let Some(frames) = steady_short {
                unchecked.push(format!("counts that keep growing: the steady phase holds {} ({MIN_FRAMES} or more needed)", counted(frames, "frame", "frames")));
            }
            gc_spikes(&s, &mut issues);
            growth(&s, &mut issues, &mut unchecked);
        }
    }
    if p.raw.heap.is_some() {
        heap_growth(&s, &mut issues);
    }
    if p.raw.luau_caches.is_some() {
        cache_misses(&s, &mut issues);
    }
    if let Some(builtins) = &p.raw.builtins {
        builtin_declines(&s, builtins, &mut issues);
    }
    match &p.boundary {
        Some(boundary) => repeat_reads(&s, boundary, &mut issues),
        // (a profile that ran boundary keeps its sites in boundary.json beside profile.json)
        None if p.has_kind("boundary") || p.raw.boundary.is_some() => unchecked.push("repeat reads and unchanged writes: need boundary.json, which this folder does not keep (the profile ran boundary)".into()),
        None => unchecked.push("repeat reads and unchanged writes: need boundary".into()),
    }
    if !what_if && work.is_some() {
        hot_code(&s, frame_work.as_ref(), &mut issues, &mut unchecked);
    }
    if !what_if && p.per_frame_unchecked().is_none() {
        match &frame_work {
            Some(frames) => hitches(&s, frames, &mut issues),
            None => legacy_spikes(&s, &mut issues, &mut unchecked),
        }
    }
    match p.raw.counters.as_ref().filter(|c| c.error.is_none()) {
        Some(c) if c.names.iter().any(|name| name == "load-stall-cycles") && c.names.iter().any(|name| name == "cycles") => memory_waits(p, c, harness_out, &mut issues),
        Some(_) => unchecked.push("code that waits on memory: needs the counters' load-stall-cycles and cycles (the default set)".into()),
        None => unchecked.push("code that waits on memory: needs hw (the CPU's counters), in a run of its own".into()),
    }
    let harness = p.funcs.iter().filter(|f| f.harness).count();
    if harness_out {
        unchecked.push(format!("the harness's code ({}): the issues cover the game's code", counted(harness as f64, "function", "functions")));
    }
    finalize(&s, frame_work.as_ref(), &mut issues);
    (issues, unchecked)
}

/// The call-tree work the fixes can claim: each node's own work in the reported phases, grouped by the script function that
/// owns it. ree's own code is left out, and the harness's when it is set apart.
fn measured_work(s: &Scoped) -> Work {
    let p = s.p;
    let nodes = p.raw_nodes();
    let owner = owners(p, &nodes);
    let mut measured = Work { amounts: vec![0.0; nodes.len()], owner, functions: BTreeMap::new(), contexts: BTreeMap::new() };
    for (index, node) in nodes.iter().enumerate() {
        let Some(func) = measured.owner[index] else { continue };
        let is_left_out = !s.scope.covers(node.phase) || p.funcs[func].tool || (s.harness_out && p.funcs[func].harness);
        if is_left_out {
            continue;
        }
        measured.amounts[index] = node.own;
        measured.functions.entry(func).or_default().nodes.insert(index);
        // (the nodes where the function itself ran, not the C functions' and engine code's under it)
        if node.func == Some(func) {
            measured.contexts.entry(func).or_default().push(index);
        }
    }
    measured
}

/// The last steps once every check has run: grade each priced issue by its share of the hitches, drop what is under
/// the listing floor, rank, say which issues overlap and what each newly counts, and close with the work nobody holds
fn finalize(s: &Scoped, frames: Option<&FrameWork>, issues: &mut Vec<Issue>) {
    if let Some(frames) = frames {
        grade_by_hitch_share(s, frames, issues);
    }
    let largest_price = largest_clock_price(issues);
    set_saving_ceilings(issues);
    issues.retain(|issue| is_listed(s, issue));
    rank(issues);
    if let Some(work) = &s.work {
        let priced = clock_priced_indexes(issues);
        note_overlaps(s, work, &priced, issues);
        let union = count_union(s, work, &priced, issues);
        issues.push(closing_row(s, work, &union, largest_price));
    }
    finish(issues);
}

/// The largest price among the issues the work clock priced (host time is another clock's)
fn largest_clock_price(issues: &[Issue]) -> f64 {
    let mut largest: f64 = 0.0;
    for issue in issues {
        if let Some(ms) = issue.ms && !issue.host_time {
            largest = largest.max(ms);
        }
    }
    largest
}

/// The positions of the issues the work clock priced
fn clock_priced_indexes(issues: &[Issue]) -> Vec<usize> {
    let mut indexes = Vec::new();
    for (index, issue) in issues.iter().enumerate() {
        if issue.ms.is_some() && !issue.host_time {
            indexes.push(index);
        }
    }
    indexes
}

/// Gives each issue the most any other issue could save, which its own price must reach to be the strongest saving. An
/// issue that states an upper bound counts at the bound, since the edit may save that much. This runs before the
/// listing floor drops issues, so an unlisted issue can still stand in the way of a strongest-saving claim.
fn set_saving_ceilings(issues: &mut [Issue]) {
    let possible_savings: Vec<f64> = issues.iter().map(Issue::largest_possible_saving).collect();
    for (index, issue) in issues.iter_mut().enumerate() {
        let mut ceiling: f64 = 0.0;
        for (other, saving) in possible_savings.iter().enumerate() {
            if other != index {
                ceiling = ceiling.max(*saving);
            }
        }
        issue.saving_ceiling = ceiling;
    }
}

/// Whether an issue is above the listing floor: its mean price, or its largest frame as a share of the frame budget
fn is_listed(s: &Scoped, issue: &Issue) -> bool {
    let Some(ms) = issue.ms else { return true };
    ms >= LIST_FLOOR_MS || issue.uncharged >= LIST_FLOOR_MS || issue.frame_ms.unwrap_or(0.0) >= ONE_OFF_FLOOR * s.budget
}

/// A priced issue is as severe as the more severe of its saving and its own work in the frames over the budget. Its
/// largest frame is the most of its work any one frame held.
fn grade_by_hitch_share(s: &Scoped, frames: &FrameWork, issues: &mut [Issue]) {
    for issue in issues.iter_mut().filter(|issue| issue.ms.is_some() && !issue.host_time) {
        // A newer ree summarises every frame before it drops detail, so the summary is complete where the kept frames
        // may not be
        if let Some(stat) = issue_summary(s.p, &issue.summary_key) {
            issue.active_frames = stat.active;
            issue.frame_ms = Some(stat.largest_us / 1000.0);
            issue.over_budget_ms = stat.over_budget_us / 1000.0;
            issue.severity = issue.severity.min(Severity::of_hitch(issue.over_budget_ms, s.budget));
            issue.evidence.push(format!("largest own contribution to an over-budget frame: {}; complete producer summary", fmt::ms(issue.over_budget_ms)));
            continue;
        }
        let mut largest_frame: f64 = 0.0;
        let mut largest_over_budget: f64 = 0.0;
        for (&frame, &frame_work) in &frames.frame_ms {
            let own = frames.issue_work(s, issue, frame);
            largest_frame = largest_frame.max(own);
            if frame_work > s.budget {
                largest_over_budget = largest_over_budget.max(own);
            }
        }
        issue.frame_ms = Some(largest_frame.max(issue.frame_ms.unwrap_or(0.0)));
        issue.over_budget_ms = largest_over_budget;
        issue.severity = issue.severity.min(Severity::of_hitch(largest_over_budget, s.budget));
        issue.evidence.push(format!("largest own contribution to an over-budget frame: {}; budget {}", fmt::ms(largest_over_budget), fmt::ms(s.budget)));
    }
}

/// The producer's frame summary for the issue with this key, if the profile has one
fn issue_summary<'a>(p: &'a Profile, key: &str) -> Option<&'a crate::model::IssueStats> {
    let timeline = p.raw.timeline.as_ref()?;
    timeline.issue_stats.iter().find(|stat| stat.key == key)
}

/// Tells each pair of priced issues how much work they share (ms a frame), so a reader knows their prices do not add up
fn note_overlaps(s: &Scoped, work: &Work, priced: &[usize], issues: &mut [Issue]) {
    let frames = s.frame_count();
    for (position, &left) in priced.iter().enumerate() {
        for &right in &priced[position + 1..] {
            let mut both = issues[left].work.clone();
            both.union(&issues[right].work);
            let separate = issues[left].work.total(&work.amounts) + issues[right].work.total(&work.amounts);
            let shared = (separate - both.total(&work.amounts)) / frames;
            if shared <= 1e-12 {
                continue;
            }
            let left_name = format!("{}: {}", issues[left].kind, issues[left].title);
            let right_name = format!("{}: {}", issues[right].kind, issues[right].title);
            issues[left].evidence.push(format!("overlaps {right_name} by {} a frame; totals count the node/site union", fmt::ms(shared)));
            issues[right].evidence.push(format!("overlaps {left_name} by {} a frame; totals count the node/site union", fmt::ms(shared)));
        }
    }
}

/// Walks the priced issues in list order, each booking the work no earlier issue held. Returns the union of them all.
fn count_union(s: &Scoped, work: &Work, priced: &[usize], issues: &mut [Issue]) -> WorkSet {
    let frames = s.frame_count();
    let mut union = WorkSet::default();
    for &index in priced {
        let issue = &mut issues[index];
        let before = union.total(&work.amounts);
        union.union(&issue.work);
        issue.counted_ms = (union.total(&work.amounts) - before) / frames;
        issue.overlap = (issue.work.total(&work.amounts) / frames - issue.counted_ms).max(0.0);
    }
    union
}

/// The row that accounts for the code's work no listed issue holds
fn closing_row(s: &Scoped, work: &Work, listed: &WorkSet, ceiling: f64) -> Issue {
    let rest = (s.code_work - listed.total(&work.amounts)).max(0.0) / s.frame_count();
    Issue {
        kind: CLOSING_ROW_KIND,
        title: format!("Work below listing floor: {} a frame (floor {} a frame)", fmt::ms(rest), fmt::ms(LIST_FLOOR_MS)),
        ms: Some(rest),
        counted_ms: rest,
        saving_ceiling: ceiling,
        ..Default::default()
    }
}

/// Each node's owner in the calling-context tree: the nearest script function on its path, itself for one. A C function's,
/// the engine's own Luau's and mlua's work goes with the code that called them, as the summary puts it
fn owners(p: &Profile, nodes: &[crate::model::RawNode]) -> Vec<Option<usize>> {
    let mut owner: Vec<Option<usize>> = Vec::with_capacity(nodes.len());
    for node in nodes {
        let inherited = node.parent.and_then(|parent| owner[parent]);
        owner.push(node.func.filter(|&func| is_code(p, func)).or(inherited));
    }
    owner
}

/// The opcodes a line ran most, `GETTABLEKS 2,000 · ADD 1,000`
fn line_ops(fig: &crate::model::LineFig) -> String {
    let mut ops: Vec<(&String, &u64)> = fig.ops.iter().collect();
    ops.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
    ops.iter().take(3).map(|(op, count)| format!("{op} {}", fmt::grouped(**count as f64))).collect::<Vec<_>>().join(" · ")
}

/// How often a function worked: describes an issue in words and never changes a price
#[derive(Clone, Debug, Default)]
struct Activity {
    /// The frames in which it did any work
    active: usize,
    /// Its work in its busiest frame (ms), and that frame's number
    largest: f64,
    frame: i64,
}

/// ms of work by frame number
type ByFrame = BTreeMap<i64, f64>;

/// What each frame of the reported phases did, as the timeline records it. The checks that grade hitches and one-offs
/// read it; the prices come from the call tree and never from here.
pub(crate) struct FrameWork {
    /// The code's work in each frame (ms)
    pub(crate) frame_ms: ByFrame,
    /// The timeline row of each frame
    row_of: BTreeMap<i64, usize>,
    /// Each script function's work in each frame
    pub(crate) by_func: BTreeMap<usize, ByFrame>,
    /// Each call-tree node's work in each frame
    by_node: BTreeMap<usize, ByFrame>,
    /// Each (node, site key)'s saving in each frame
    by_site: BTreeMap<(usize, String), ByFrame>,
    /// Each edit's replacement work in each frame
    by_replacement: BTreeMap<String, ByFrame>,
    activity: BTreeMap<usize, Activity>,
}

impl FrameWork {
    fn of(s: &Scoped, unchecked: &mut Vec<String>) -> Option<Self> {
        let timeline = s.p.raw.timeline.as_ref()?;
        let function_rows = timeline.function_rows();
        if function_rows.is_none() && timeline.function_stats.is_empty() {
            unchecked.push("function cadence and hitch attribution: needs frame summaries from a newer profile".into());
            return None;
        }
        let mut frames = Self {
            frame_ms: BTreeMap::new(),
            row_of: BTreeMap::new(),
            by_func: BTreeMap::new(),
            by_node: BTreeMap::new(),
            by_site: BTreeMap::new(),
            by_replacement: BTreeMap::new(),
            activity: BTreeMap::new(),
        };
        let rows = s.rows();
        frames.read_rows(s, timeline, &rows, function_rows.as_deref());
        frames.read_node_frames(timeline, &rows);
        frames.read_site_frames(timeline, &rows);
        frames.read_replacement_frames(timeline, &rows);
        if function_rows.is_none() {
            frames.sum_nodes_into_functions(s);
        }
        frames.find_activity(s.p, timeline);
        Some(frames)
    }

    /// Each row's frame, its work, and (where the profile lists them) its functions' work
    fn read_rows(&mut self, s: &Scoped, timeline: &crate::model::Columns, rows: &[usize], function_rows: Option<&[Vec<(i64, f64)>]>) {
        for &row in rows {
            let frame = frame_of_row(timeline, row);
            *self.frame_ms.entry(frame).or_default() += code_work_of_row(s, timeline, row);
            self.row_of.insert(frame, row);
            let functions_in_row = function_rows.and_then(|rows| rows.get(row)).into_iter().flatten();
            for &(id, ms) in functions_in_row {
                let Some(func) = s.p.func_of_id(id) else { continue };
                let func_data = &s.p.funcs[func];
                let is_covered = is_code(s.p, func) && !func_data.tool && !(s.harness_out && func_data.harness);
                if is_covered {
                    *self.by_func.entry(func).or_default().entry(frame).or_default() += ms;
                }
            }
        }
    }

    /// Each node's work by frame. A cell is a row, then (node, microseconds) pairs.
    fn read_node_frames(&mut self, timeline: &crate::model::Columns, rows: &[usize]) {
        for cells in &timeline.node_frames {
            let Some(&row_cell) = cells.first() else { continue };
            let row = row_cell as usize;
            if !rows.contains(&row) {
                continue;
            }
            let frame = frame_of_row(timeline, row);
            let (pairs, _) = cells[1..].as_chunks::<2>();
            for &[node, microseconds] in pairs {
                *self.by_node.entry(node as usize).or_default().entry(frame).or_default() += microseconds / 1000.0;
            }
        }
    }

    fn read_site_frames(&mut self, timeline: &crate::model::Columns, rows: &[usize]) {
        for site in &timeline.site_frames {
            if !rows.contains(&site.row) {
                continue;
            }
            let frame = frame_of_row(timeline, site.row);
            let key = format!("{}:{}:{}", site.kind, site.function, site.pc);
            *self.by_site.entry((site.node, key)).or_default().entry(frame).or_default() += site.saving_us / 1000.0;
        }
    }

    fn read_replacement_frames(&mut self, timeline: &crate::model::Columns, rows: &[usize]) {
        for (row, key, microseconds) in &timeline.replacement_frames {
            if !rows.contains(row) {
                continue;
            }
            let frame = frame_of_row(timeline, *row);
            *self.by_replacement.entry(key.clone()).or_default().entry(frame).or_default() += microseconds / 1000.0;
        }
    }

    /// A profile that lists no function's work per frame still has its nodes' work: a function's work in a frame is
    /// the sum of the nodes it owns
    fn sum_nodes_into_functions(&mut self, s: &Scoped) {
        let Some(work) = &s.work else { return };
        for (&node, series) in &self.by_node {
            let Some(func) = work.owner_of(node) else { continue };
            let owns_node = work.functions.get(&func).is_some_and(|set| set.nodes.contains(&node));
            if !owns_node {
                continue;
            }
            for (&frame, &ms) in series {
                *self.by_func.entry(func).or_default().entry(frame).or_default() += ms;
            }
        }
    }

    /// Each function's active frames and busiest frame: from its per-frame work, then the profile's own summary of it
    /// where it has one (a summary keeps small events without keeping every frame)
    fn find_activity(&mut self, p: &Profile, timeline: &crate::model::Columns) {
        for (&func, series) in &self.by_func {
            let mut activity = Activity::default();
            for (&frame, &ms) in series {
                if ms > 0.0 {
                    activity.active += 1;
                }
                if ms > activity.largest {
                    activity.largest = ms;
                    activity.frame = frame;
                }
            }
            self.activity.insert(func, activity);
        }
        for stat in &timeline.function_stats {
            if let Some(func) = p.func_of_id(stat.function) {
                self.activity.insert(func, Activity { active: stat.active, largest: stat.largest_us / 1000.0, frame: stat.frame });
            }
        }
    }

    /// The work of an issue's fix in one frame (ms): its removed nodes' work, its sites' savings, less the replacement
    /// work its edits paid in that frame
    fn issue_work(&self, s: &Scoped, issue: &Issue, frame: i64) -> f64 {
        let in_frame = |series: Option<&ByFrame>| series.and_then(|series| series.get(&frame)).copied().unwrap_or(0.0);
        let mut amount = 0.0;
        if !self.by_node.is_empty() {
            amount = issue.work.nodes.iter().filter_map(|node| self.by_node.get(node)?.get(&frame)).sum();
        } else if let Some(work) = &s.work {
            // (no node history: a function whose every node the fix removes counts whole)
            for (func, set) in &work.functions {
                if set.nodes.is_subset(&issue.work.nodes) {
                    amount += in_frame(self.by_func.get(func));
                }
            }
        }
        let removed_nodes = &issue.work.nodes;
        for site in issue.work.sites.iter().filter(|site| !removed_nodes.contains(&site.node)) {
            amount += in_frame(self.by_site.get(&(site.node, site.key.clone())));
        }
        for fix in issue.work.replacements.keys() {
            let edit_still_needed = issue.work.sites.iter().any(|site| site.fix_key() == fix.as_str() && !removed_nodes.contains(&site.node));
            if edit_still_needed {
                amount -= in_frame(self.by_replacement.get(fix));
            }
        }
        amount
    }
}

/// A row's frame number (a row without one is its own index)
fn frame_of_row(timeline: &crate::model::Columns, row: usize) -> i64 {
    timeline.value(row, "frame").unwrap_or(row as f64) as i64
}

/// A row's work in the code the issues cover (ms): the game's when the harness's is set apart, else the game's and the
/// harness's together, else the row's whole work where the timeline does not split it
fn code_work_of_row(s: &Scoped, timeline: &crate::model::Columns, row: usize) -> f64 {
    let game = timeline.value(row, "game_ms");
    let split = if s.harness_out { game } else { game.zip(timeline.value(row, "harness_ms")).map(|(game, harness)| game + harness) };
    split.or_else(|| timeline.value(row, "work_ms")).unwrap_or(0.0)
}

/// `the game's work`, `the code's work`, or a frame's whole `work` where the timeline does not split it
fn frame_what(s: &Scoped) -> &'static str {
    match s.p.raw.timeline.as_ref().and_then(|timeline| timeline.index("game_ms")) {
        None => "work",
        Some(_) if s.harness_out => "the game's work",
        Some(_) => "the code's work",
    }
}

/// `, 2.300 ms over a 16.67 ms frame`, or nothing within one
fn over_words(frame_ms: f64, budget: f64) -> String {
    if frame_ms > budget { format!(", {} over a {} frame", fmt::ms(frame_ms - budget), fmt::ms(budget)) } else { String::new() }
}

/// `30, 60, 90 and 4 more`
fn list_frames(frames: &[f64]) -> String {
    let shown = frames.iter().take(8).map(|frame| format!("{frame}")).collect::<Vec<_>>().join(", ");
    if frames.len() > 8 { format!("{shown} and {} more", frames.len() - 8) } else { shown }
}

/// Removing a function's computation saves all of its owned nodes. A hot line is a navigation aid; cadence only
/// changes the description and advice.
fn hot_code(s: &Scoped, fw: Option<&FrameWork>, issues: &mut Vec<Issue>, _unchecked: &mut Vec<String>) {
    let Some(work) = &s.work else { return };
    let raw = s.p.raw_nodes();
    for (&func, owned) in &work.functions {
        let total = owned.total(&work.amounts);
        if total <= 0.0 {
            continue;
        }
        let rate = total / s.frame_count();
        let activity = fw.and_then(|fw| fw.activity.get(&func));
        let once = activity.is_some_and(|activity| activity.active == 1);
        let lines = hot_lines(s, func);
        let hottest_line = lines.first().map(|(line, _, _)| *line);
        let kind = if once {
            "one-off"
        } else if hottest_line.is_some() {
            "hot line"
        } else {
            "hot function"
        };
        let advice = if once {
            "Remove this computation when its result is not needed, or do it before the reported phases; spreading it only moves work between frames"
        } else {
            "Remove this computation when its result is not needed; spreading it only moves work between frames"
        };
        let label = s.p.funcs[func].label().replace("  ", " ");
        let mut issue = Issue {
            kind,
            summary_key: format!("function:{}", s.p.funcs[func].id),
            title: format!("{label}: {} a frame; {}", fmt::ms(rate), cadence_words(s, activity)),
            evidence: vec![format!("{} of {} work; the complete node set is priced", fmt::share(total, s.code_work), s.whose())],
            advice: advice.into(),
            target: hottest_line.map_or(Target::Func(func as u32), |line| Target::Line(func as u32, line)),
            ms: Some(rate),
            work: owned.clone(),
            active_frames: activity.map_or(0, |activity| activity.active),
            frame_ms: activity.map(|activity| activity.largest),
            once: activity.filter(|_| once).map(|activity| activity.frame as f64),
            total_ms: activity.filter(|_| once).map(|activity| activity.largest),
            ..Default::default()
        };
        let calls: u64 = owned.nodes.iter().filter(|&&node| raw[node].func == Some(func)).map(|&node| raw[node].calls).sum();
        if calls > 0 {
            issue.evidence.push(format!("{}, {:.1} a frame", counted(calls as f64, "call", "calls"), calls as f64 / s.frame_count()));
        }
        if let Some(line) = lines_evidence(s, func, &lines) {
            issue.evidence.push(line);
        }
        if let Some(line) = c_calls_evidence(s, work, &raw, owned) {
            issue.evidence.push(line);
        }
        issues.push(s.priced(issue));
    }
}

/// How a function's work was spread over frames, in words
fn cadence_words(s: &Scoped, activity: Option<&Activity>) -> String {
    match activity {
        Some(activity) if activity.active == 1 => format!("{} once in this run, in frame {}", fmt::ms(activity.largest), activity.frame),
        Some(activity) => format!("active in {} of {} frames; largest {} in frame {}", activity.active, s.frame_count(), fmt::ms(activity.largest), activity.frame),
        None => "cadence not recorded".into(),
    }
}

/// A function's lines that did work, the busiest first, with their work in the reported phases (ms). A profile that
/// counts lines per phase is read for the reported phases; an older one only has the whole run.
fn hot_lines<'a>(s: &'a Scoped, func: usize) -> Vec<(i64, f64, &'a crate::model::LineFig)> {
    let counted_per_phase = s.p.lines_of(func).any(|(_, fig)| !fig.phases.is_empty());
    let mut lines: Vec<_> = s
        .p
        .lines_of(func)
        .map(|(line, fig)| {
            let ms = if counted_per_phase { fig.phases.iter().filter(|(phase, _)| s.scope.covers(**phase)).map(|(_, ms)| *ms).sum() } else { fig.work };
            (line, ms, fig)
        })
        .filter(|(_, ms, _)| *ms > 0.0)
        .collect();
    lines.sort_by(|a, b| b.1.total_cmp(&a.1));
    lines
}

/// The evidence line naming a function's three busiest lines and the opcodes each ran most
fn lines_evidence(s: &Scoped, func: usize, lines: &[(i64, f64, &crate::model::LineFig)]) -> Option<String> {
    let counted_per_phase = s.p.lines_of(func).any(|(_, fig)| !fig.phases.is_empty());
    let all_lines_ms: f64 = lines.iter().map(|(_, ms, _)| *ms).sum();
    let module = &s.p.funcs[func].short;
    let busiest: Vec<String> = lines
        .iter()
        .take(3)
        .map(|(line, ms, fig)| format!("{module}:{line} {} of it ({})", fmt::share(*ms, all_lines_ms), line_ops(fig)))
        .collect();
    if busiest.is_empty() {
        return None;
    }
    let scope_note = if counted_per_phase { "" } else { " (lines are counted over the whole run)" };
    Some(format!("its lines{scope_note}: {}", busiest.join("; ")))
}

/// The evidence line naming the C functions a function called (or that this profile does not record them apart)
fn c_calls_evidence(s: &Scoped, work: &Work, raw: &[crate::model::RawNode], owned: &WorkSet) -> Option<String> {
    let records_c_calls = s.p.raw.work.as_ref().is_some_and(|work| work.c_calls);
    if !records_c_calls {
        return Some("the C functions it calls are in its lines' work: this profile does not record C calls apart (needs C call rows)".into());
    }
    // (c function, line it was called at, calls, work in ms)
    let mut called: Vec<(usize, Option<i64>, u64, f64)> = Vec::new();
    for &node in &owned.nodes {
        let Some(c_function) = raw[node].func.filter(|&c| s.p.funcs[c].kind == Kind::C) else { continue };
        called.push((c_function, raw[node].line, raw[node].calls, work.amounts[node]));
    }
    called.sort_by(|a, b| b.3.total_cmp(&a.3).then(a.0.cmp(&b.0)));
    let named: Vec<String> = called
        .iter()
        .take(3)
        .map(|(c_function, line, calls, ms)| {
            let name = s.p.funcs[*c_function].name.trim_end_matches(" [C]");
            let at_line = line.map_or(String::new(), |line| format!(" at line {line}"));
            format!("{name}{at_line} ×{} {}", fmt::grouped(*calls as f64), fmt::ms(ms / s.frame_count()))
        })
        .collect();
    if named.is_empty() {
        return None;
    }
    Some(format!("the C functions it called, a frame: {}", named.join("; ")))
}

fn hitches(s: &Scoped, fw: &FrameWork, issues: &mut Vec<Issue>) {
    let p = s.p;
    let what = frame_what(s);
    let mut over: Vec<(i64, f64)> = fw.frame_ms.iter().filter(|(_, ms)| **ms > s.budget).map(|(&frame, &ms)| (frame, ms)).collect();
    over.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
    let all: Vec<f64> = {
        let mut frames: Vec<f64> = over.iter().map(|(frame, _)| *frame as f64).collect();
        frames.sort_by(f64::total_cmp);
        frames
    };
    for (index, &(frame, frame_ms)) in over.iter().take(HITCHES).enumerate() {
        let mut ran: Vec<(usize, f64)> = fw.by_func.iter().filter_map(|(&func, series)| series.get(&frame).map(|ms| (func, *ms))).collect();
        ran.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
        let named: Vec<String> = ran.iter().map(|(func, ms)| format!("{} {} ({})", p.funcs[*func].label().replace("  ", " "), fmt::ms(*ms), fmt::share(*ms, frame_ms))).collect();
        let named_work: f64 = ran.iter().map(|(_, ms)| ms).sum();
        // Old profiles can lack some contributors. Keep the unnamed remainder visible
        let rest = (frame_ms - named_work).max(0.0);
        let mut evidence = vec![
            format!("{}: its severity; it prices nothing, its work is its functions' (their issues)", hitch_words(frame as f64, frame_ms, s.budget)),
            format!("what worked in it: {}{}", named.join("; "), if rest > 1e-6 { format!("; other work without frame history {} ({})", fmt::ms(rest), fmt::share(rest, frame_ms)) } else { String::new() }),
        ];
        if index == 0 && all.len() > 1 {
            evidence.push(format!("frames that overrun: {} ({} in all)", list_frames(&all), all.len()));
        }
        issues.push(Issue {
            severity: Severity::of_hitch(frame_ms, s.budget),
            kind: "frame hitch",
            title: format!("Frame hitch: frame {frame} took {} of {what}, {} over a {} frame", fmt::ms(frame_ms), fmt::ms(frame_ms - s.budget), fmt::ms(s.budget)),
            evidence,
            advice: "Its functions' issues say what to change: the biggest share first".into(),
            target: Target::Frame(fw.row_of.get(&frame).copied().unwrap_or(0)),
            once: Some(frame as f64),
            frame_ms: Some(frame_ms),
            recurs: all.len(),
            weight: frame_ms,
            ..Default::default()
        });
    }
}

/// A profile from before ree recorded each function's work a frame sees only the frames' work: a frame that stands out
/// (three times the other frames' mean, [`SPIKE_FLOOR`] of a frame above it) is named, not priced, since what made it is
/// not known; its severity is its hitch
fn legacy_spikes(s: &Scoped, issues: &mut Vec<Issue>, unchecked: &mut Vec<String>) {
    let p = s.p;
    let Some(timeline) = &p.raw.timeline else { return };
    let rows = s.rows();
    if (rows.len() as f64) < MIN_FRAMES {
        unchecked.push(format!("frame spikes: the reported phases hold {} ({MIN_FRAMES} or more needed)", counted(rows.len() as f64, "frame", "frames")));
        return;
    }
    let frame_of = |row: usize| timeline.value(row, "frame").unwrap_or(row as f64);
    let what = frame_what(s);
    let values = match (timeline.values("game_ms"), timeline.values("harness_ms")) {
        (Some(game), _) if s.harness_out => game,
        (Some(game), Some(harness)) => game.iter().zip(&harness).map(|(a, b)| a + b).collect(),
        _ => timeline.values("work_ms").unwrap_or_default(),
    };
    let all: f64 = rows.iter().map(|&row| values[row]).sum();
    let others = |row: usize| (all - values[row]) / (rows.len() - 1) as f64;
    let standing: Vec<usize> = rows.iter().copied().filter(|&row| values[row] - BURST * others(row) >= SPIKE_FLOOR * s.budget).collect();
    let Some(&worst) = standing.iter().max_by(|&&a, &&b| values[a].total_cmp(&values[b]).then(b.cmp(&a))) else { return };
    let mut evidence = vec![format!("frame {}: {} of {what}, against a mean frame of {} in {}", frame_of(worst), fmt::ms(values[worst]), fmt::ms(others(worst)), s.scope.label)];
    evidence.push(format!("frames that stand out: {}", list_frames(&standing.iter().map(|&row| frame_of(row)).collect::<Vec<_>>())));
    let hint = if timeline.index("game_ms").is_some() {
        "a newer ree records each function's work a frame"
    } else if p.has_kind("work") && p.has_kind("timeline") {
        "a newer ree splits a frame's work between the game's code, the harness's and the engine, and records each function's"
    } else {
        "needs work with timeline"
    };
    let phases = timeline.values("phase").unwrap_or_default();
    if timeline.index("part").is_none() && (0..worst).all(|row| phases[row] != phases[worst]) {
        evidence.push(format!(
            "frame {} is the first of {}: this profile, from before a frame's row split at a phase mark, books all of it to that phase, so work done before the mark in it (a setup) is counted here",
            frame_of(worst),
            p.phase_words(phases[worst] as u32).unwrap_or_else(|| p.phase_name(phases[worst] as u32).to_string())
        ));
    }
    issues.push(Issue {
        severity: Severity::of_hitch(values[worst], s.budget),
        kind: "frame spike",
        title: format!("Frame spike: frame {} took {} of {what}{}", frame_of(worst), fmt::ms(values[worst]), over_words(values[worst], s.budget)),
        evidence,
        advice: "Do the work ahead (at load, or when its input changes), or spread it over frames".into(),
        target: Target::Frame(worst),
        once: Some(frame_of(worst)),
        frame_ms: Some(values[worst]),
        recurs: standing.len(),
        unpriced: Some(format!("what made it, so what doing it ahead saves, is not in this profile ({hint}); its severity is its hitch")),
        weight: values[worst],
        ..Default::default()
    });
}

/// Whether a function is `table.sort` (a newer ree names a library's C functions in full; an older one by the bare name)
fn is_sort(p: &Profile, func: usize) -> bool {
    let f = &p.funcs[func];
    f.kind == Kind::C && matches!(f.name.trim_end_matches(" [C]"), "table.sort" | "sort")
}

/// What the calls to `table.sort` at one place (a calling function and a line, -1 when the line is unknown) did
#[derive(Default)]
struct SortsAtSite {
    /// Calls to table.sort
    sorts: u64,
    /// Calls to the comparator functions
    comparator_runs: u64,
    /// The work of the sorts' whole subtrees, by the function that owns it (ms over the reported frames)
    work_by_function: BTreeMap<usize, f64>,
    /// Every call-tree node in those subtrees: removing the sorts removes all of them
    nodes: BTreeSet<usize>,
}

/// Removing the sort calls removes their actual subtrees, including every comparator context. Counts describe the
/// observed calls and do not gate their price
fn sorts(s: &Scoped, issues: &mut Vec<Issue>, unchecked: &mut Vec<String>) {
    let p = s.p;
    let work = p.raw.work.as_ref();
    let (has_c_rows, has_invocations) = (work.is_some_and(|w| w.c_calls), work.is_some_and(|w| w.c_invocations));
    if !has_c_rows {
        unchecked.push("sorts without a comparator: need C call rows (this profile records a C function only when Luau runs under it)".into());
    }
    let frames = match s.frames {
        Some(frames) if frames > 0.0 => frames,
        _ => {
            unchecked.push("sort calls: the reported phases' frame count is missing or zero".into());
            return;
        }
    };
    for ((caller, line), site) in sorts_by_site(s) {
        if site.sorts == 0 {
            continue;
        }
        let per_frame = site.sorts as f64 / frames;
        let work: f64 = site.work_by_function.values().sum();
        let ms = work / frames;
        let f = &p.funcs[caller];
        let at = if line >= 0 { format!(" at {}:{line}", f.short) } else { String::new() };
        let comparator = if site.comparator_runs == 0 {
            String::new()
        } else if has_invocations {
            let runs = site.comparator_runs as f64;
            format!("; its comparator ran {} times, {} a sort", fmt::grouped(runs), fmt::grouped(runs / site.sorts as f64))
        } else {
            "; its comparator's calls are counted once a sort, not once a run (needs invocation counts)".into()
        };
        issues.push(s.priced(Issue {
            kind: "sort calls",
            summary_key: format!("sort:{}:{line}", f.id),
            title: format!("Sort calls: {}{}: {} a frame", f.label(), if line >= 0 { format!(", line {line}") } else { String::new() }, fmt::ms(ms)),
            evidence: vec![
                format!("table.sort ran {}{at} in {}, {per_frame:.1} a frame", counted(site.sorts as f64, "time", "times"), s.frames_words(frames)),
                format!("{} a frame in the sorts, {} of {} work{comparator}", fmt::ms(ms), fmt::share(work, s.code_work), s.whose()),
            ],
            advice: "Remove these sort calls when the ordering is not needed; this price is the whole removed subtree".into(),
            target: if line >= 0 { Target::Line(caller as u32, line) } else { Target::Func(caller as u32) },
            ms: Some(ms),
            // The sorts' subtrees are the price. Removing a sort statement also removes the evaluation of its arguments,
            // which can run in other functions' nodes, so the complete edit is bounded by all the code's work.
            upper_ms: Some(s.code_work / frames),
            bound_reason: "the caller's instructions and the evaluation of its arguments",
            claims: site.work_by_function.iter().map(|(&func, &work)| (func as u32, work / frames)).collect(),
            work: WorkSet { nodes: site.nodes, ..WorkSet::default() },
            weight: work,
            ..Default::default()
        }));
    }
}

/// The sorts of the reported phases grouped by where they are called from, as (caller, line) -> what they did. Sorts called
/// from ree's own code, or from the harness's when that is set apart, are left out.
fn sorts_by_site(s: &Scoped) -> BTreeMap<(usize, i64), SortsAtSite> {
    let p = s.p;
    let nodes = p.raw_nodes();
    let owner = owners(p, &nodes);
    let mut children: Vec<Vec<usize>> = vec![Vec::new(); nodes.len()];
    for (index, node) in nodes.iter().enumerate() {
        if let Some(parent) = node.parent {
            children[parent].push(index);
        }
    }
    let mut by_site: BTreeMap<(usize, i64), SortsAtSite> = BTreeMap::new();
    for (sort_node, node) in nodes.iter().enumerate() {
        if !node.func.is_some_and(|func| is_sort(p, func)) || !s.scope.covers(node.phase) {
            continue;
        }
        let Some(caller) = node.parent.and_then(|parent| owner[parent]) else { continue };
        if p.funcs[caller].tool || (s.harness_out && p.funcs[caller].harness) {
            continue;
        }
        let site = by_site.entry((caller, node.line.unwrap_or(-1))).or_default();
        site.sorts += node.calls;
        // (the sort's subtree: its own C work goes to the caller; its comparator's runs and work, to the comparator)
        let mut to_visit = vec![sort_node];
        while let Some(at) = to_visit.pop() {
            let is_comparator_call = nodes[at].parent == Some(sort_node) && nodes[at].func.is_some_and(|func| is_code(p, func));
            if is_comparator_call {
                site.comparator_runs += nodes[at].calls;
            }
            *site.work_by_function.entry(owner[at].unwrap_or(caller)).or_default() += nodes[at].own;
            site.nodes.insert(at);
            to_visit.extend(children[at].iter().copied());
        }
    }
    by_site
}

// The memory checks below read the steady phase (`Profile::steady_phase`) as they did before the framework read the
// reported phases; the memory rework (rework/memory-profiling) is rewriting them on `Scoped`'s frames, rows and per-site
// figures (`Scoped::frames`, `Scoped::rows`, `Scoped::site_per_frame`) and prices, through `Scoped::priced`

/// `the 240 frames`, `the 150 frames of the steady phase`
fn steady_words(p: &Profile, steady: Option<(u32, u64)>) -> String {
    steady.map(|(phase, frames)| format!("the {}{}", counted(frames as f64, "frame", "frames"), p.phase_words(phase).map(|words| format!(" of {words}")).unwrap_or_default())).unwrap_or_default()
}

fn median(values: &[f64]) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    sorted[sorted.len() / 2]
}

/// The bytes the phase's frames allocated in all, (smallest, median, largest), from the timeline. (Its `allocations`
/// column counts objects without their table storage, so counts are not compared with a site's.)
fn frame_allocation(p: &Profile, phase: u32) -> Option<(f64, f64, f64)> {
    let timeline = p.raw.timeline.as_ref()?;
    let phases = timeline.values("phase")?;
    let bytes = timeline.values("allocated_bytes")?;
    let rows: Vec<f64> = (0..phases.len()).filter(|&i| phases[i] as u32 == phase).map(|i| bytes[i]).collect();
    if rows.is_empty() {
        return None;
    }
    let smallest = rows.iter().copied().fold(f64::INFINITY, f64::min);
    let largest = rows.iter().copied().fold(0.0, f64::max);
    Some((smallest, median(&rows), largest))
}

/// Sites that allocate through the steady state. A site whose average a frame is more than the phase's smallest frame
/// allocates in all cannot allocate that every frame: it comes in bursts
fn steady_allocation(s: &Scoped, sites: &[AllocSite], issues: &mut Vec<Issue>, unchecked: &mut Vec<String>) {
    let (p, steady, harness_out) = (s.p, s.p.steady_phase(), s.harness_out);
    let steady_frames = steady_words(p, steady);
    let Some((phase, frames)) = steady else { return };
    if frames < 10 {
        return;
    }
    // (a site's figures by phase come from the folded allocation stacks: a folder that kept profile.json alone has its
    // sites over the run only, which says nothing of a phase's frames)
    if !sites.is_empty() && sites.iter().all(|site| site.phases.is_empty()) {
        unchecked.push("steady allocation per site: needs the folded allocation stacks (alloc.folded), which this folder does not keep, or a newer ree's per-phase site table".into());
        return;
    }
    let frame_bytes = frame_allocation(p, phase);
    let mut rows: Vec<(&AllocSite, f64, f64)> = sites
        .iter()
        .filter(|site| site.func != ENGINE && !(harness_out && p.funcs[site.func as usize].harness))
        .map(|site| {
            let (bytes, count) = site.per_frame(phase, frames);
            (site, bytes, count)
        })
        .filter(|(_, bytes, count)| *count >= 1.0 && *bytes >= SMALL_BYTES)
        .collect();
    rows.sort_by(|a, b| b.1.total_cmp(&a.1).then(b.2.total_cmp(&a.2)));
    let all_bytes: f64 = sites.iter().map(|site| site.per_frame(phase, frames).0).sum();
    for (site, bytes, count) in rows.into_iter().take(6) {
        let types: Vec<String> = site.types.iter().take(3).map(|(ty, b, _)| format!("{ty} {}", fmt::share(*b, site.bytes.max(1.0)))).collect();
        let types = if types.is_empty() { String::new() } else { format!("; {}", types.join(", ")) };
        let target = if site.line >= 0 { Target::Site(site.func, site.line) } else { Target::Func(site.func) };
        let label = site_label(p, site);
        let (total, objects) = site.phases.get(&phase).copied().unwrap_or_default();
        if let Some((smallest, median_frame, largest)) = frame_bytes.filter(|(smallest, _, _)| bytes > *smallest) {
            issues.push(Issue {
                severity: if total >= 8.0 * 1024.0 * 1024.0 { Severity::Serious } else { Severity::Warning },
                kind: "allocation bursts",
                title: format!("Allocation bursts: {label}"),
                evidence: vec![
                    format!("{} in {steady_frames}, {} a frame on average, but some frames allocate {} in all", fmt::bytes(total), fmt::bytes(bytes), fmt::bytes(smallest)),
                    format!("frames allocate {} in the median, {} at most · {} objects{types}", fmt::bytes(median_frame), fmt::bytes(largest), fmt::grouped(objects)),
                ],
                advice: "A burst is collector work a few frames later: make less of it, or spread it over frames".into(),
                target,
                weight: total,
                ..Default::default()
            });
            continue;
        }
        issues.push(Issue {
            severity: if bytes >= 1024.0 * 1024.0 {
                Severity::Critical
            } else if bytes >= 64.0 * 1024.0 {
                Severity::Serious
            } else {
                Severity::Warning
            },
            kind: "steady allocation",
            title: format!("{} a frame on average: {label}", fmt::bytes(bytes)),
            evidence: vec![
                format!("{} objects a frame over {steady_frames}; {} of what they allocate", fmt::grouped(count), fmt::share(bytes, all_bytes)),
                format!("over the run{}", if types.is_empty() { ": types not in the kept stacks".to_string() } else { types }),
            ],
            advice: "Reuse them between frames (clear and refill a kept table), or build them only when the input changes".into(),
            target,
            weight: bytes,
            ..Default::default()
        });
    }
}

/// Table storage allocated past the new tables' own: tables grown a step at a time (each growth rehashes the keys)
fn table_growth(s: &Scoped, sites: &[AllocSite], issues: &mut Vec<Issue>) {
    let (p, harness_out) = (s.p, s.harness_out);
    let frames = p.raw.run.frames.max(1) as f64;
    let mut rows: Vec<(&AllocSite, f64, f64, f64)> = sites
        .iter()
        .filter(|site| site.func != ENGINE && !(harness_out && p.funcs[site.func as usize].harness))
        .map(|site| {
            let (_, tables) = site.of_type("table");
            let (storage_bytes, storage) = site.of_type("table storage");
            (site, (storage - tables).max(0.0), tables, storage_bytes)
        })
        .filter(|(_, excess, _, _)| *excess >= frames.max(10.0))
        .collect();
    rows.sort_by(|a, b| b.1.total_cmp(&a.1));
    for (site, excess, tables, storage_bytes) in rows {
        let per_frame = excess / frames;
        issues.push(Issue {
            severity: if per_frame >= 100.0 {
                Severity::Serious
            } else if per_frame >= 5.0 {
                Severity::Warning
            } else {
                Severity::Info
            },
            kind: "table growth",
            title: format!("Tables regrown: {}", site_label(p, site)),
            evidence: vec![
                format!("{} table storage allocations past the {} tables' first: {per_frame:.1} regrowths a frame over {} frames", fmt::grouped(excess), fmt::grouped(tables), fmt::grouped(frames)),
                format!("{} of table storage; each regrowth moves every key", fmt::bytes(storage_bytes)),
            ],
            advice: "Make the table at its final size (table.create(n), or a constructor with every field), or keep it between frames".into(),
            target: if site.line >= 0 { Target::Site(site.func, site.line) } else { Target::Func(site.func) },
            weight: excess,
            ..Default::default()
        });
    }
}

/// Frames whose collector steps stand far above the run's typical frame
fn gc_spikes(s: &Scoped, issues: &mut Vec<Issue>) {
    let p = s.p;
    let Some(timeline) = &p.raw.timeline else { return };
    let frames = timeline.values("frame").unwrap_or_default();
    let frame = |i: usize| frames.get(i).copied().unwrap_or(i as f64);
    // (past the mount when the run marks a phase after startup: the mount's frame is expected to stand out)
    let phases = timeline.values("phase").unwrap_or_default();
    let later = phases.iter().any(|phase| *phase > 0.0);
    let kept = |i: usize| !later || phases.get(i).is_some_and(|phase| *phase > 0.0);
    let list = |rows: &[usize]| {
        let shown = rows.iter().take(8).map(|&i| format!("{}", frame(i))).collect::<Vec<_>>().join(", ");
        if rows.len() > 8 { format!("{shown} and {} more", rows.len() - 8) } else { shown }
    };
    // (by the collector's steps, which are the same every run of the same code: its milliseconds are host time)
    if let (Some(gc), Some(freed), Some(steps)) = (timeline.values("gc_ms"), timeline.values("gc_freed"), timeline.values("gc_steps")) {
        let typical = median(&(0..steps.len()).filter(|&i| kept(i)).map(|i| steps[i]).collect::<Vec<_>>());
        let floor = (typical * 5.0).max(50.0);
        let spikes: Vec<usize> = (0..steps.len()).filter(|&i| kept(i) && steps[i] >= floor).collect();
        if let Some(&worst) = spikes.iter().max_by(|&&a, &&b| steps[a].total_cmp(&steps[b]).then(b.cmp(&a))) {
            issues.push(Issue {
                severity: if steps[worst] >= 1000.0 { Severity::Serious } else { Severity::Warning },
                kind: "gc spike",
                title: format!("Collector spikes: {}, worst {} steps (frame {})", counted(spikes.len() as f64, "frame", "frames"), fmt::grouped(steps[worst]), frame(worst)),
                evidence: vec![
                    format!("frame {}: {} collector steps, {} freed, {} of host time; a frame's median is {} steps", frame(worst), fmt::grouped(steps[worst]), fmt::bytes(freed[worst]), fmt::ms(gc[worst]), fmt::grouped(typical)),
                    format!("frames of {} steps or more: {}", fmt::grouped(floor), list(&spikes)),
                ],
                advice: "The collector works off what was allocated: see the allocation issues".into(),
                target: Target::Frame(worst),
                weight: steps[worst],
                ..Default::default()
            });
        }
    }
}

/// Instances, connections or threads that keep growing through the steady state. (A newer ree counts the instances in the
/// DataModel and those alive outside it apart, and the threads made in each frame rather than those alive: the threads
/// still alive are the heap snapshots', what a full collection keeps.)
fn growth(s: &Scoped, issues: &mut Vec<Issue>, unchecked: &mut Vec<String>) {
    let p = s.p;
    let steady = p.steady_phase();
    let steady_frames = steady_words(p, steady);
    let (Some(timeline), Some((phase, frames))) = (&p.raw.timeline, steady) else { return };
    if frames < 10 {
        return;
    }
    let phases = timeline.values("phase").unwrap_or_default();
    let rows: Vec<usize> = (0..phases.len()).filter(|&i| phases[i] as u32 == phase).collect();
    let (Some(&first), Some(&last)) = (rows.first(), rows.last()) else { return };
    let instances = timeline.values("instances").or_else(|| {
        let (inside, outside) = (timeline.values("instances_in_datamodel")?, timeline.values("instances_outside")?);
        Some(inside.iter().zip(&outside).map(|(a, b)| a + b).collect())
    });
    for (what, values) in [("connections", timeline.values("connections")), ("instances", instances), ("threads", timeline.values("threads"))] {
        let Some(values) = values else {
            if what == "threads" && timeline.index("threads_made").is_some() {
                thread_growth(s, timeline.value(first, "frame").unwrap_or(0.0), timeline.value(last, "frame").unwrap_or(0.0), &steady_frames, issues, unchecked);
            }
            continue;
        };
        let delta = values[last] - values[first];
        let per_frame = delta / (rows.len().max(2) - 1) as f64;
        if delta < 20.0 || per_frame < 0.5 {
            continue;
        }
        // (growth in most frames, not one step: a leak keeps adding)
        let rising = rows.windows(2).filter(|pair| values[pair[1]] > values[pair[0]]).count();
        if rising * 4 < rows.len() {
            continue;
        }
        // (a thread counts from when it is made to when the collector frees it: with the last heap snapshot, the threads a
        // full collection keeps say whether the count is held or garbage the collector had not swept yet; without one,
        // finished threads read as growth, so it is not checked)
        let reachable = (what == "threads").then(|| reachable_threads(p)).flatten();
        if what == "threads" && reachable.is_none() {
            unchecked.push("threads keep growing: needs heap (the timeline counts threads the collector has not swept yet, so threads that finished read as growth)".into());
            continue;
        }
        if reachable.is_some_and(|kept| kept * 2.0 < values[last]) {
            continue;
        }
        let kept = reachable.map(|kept| format!("; {} reachable at the end, what a full collection keeps", fmt::grouped(kept))).unwrap_or_default();
        issues.push(Issue {
            severity: if per_frame >= 10.0 { Severity::Serious } else { Severity::Warning },
            kind: "growth",
            title: format!("{} keep growing: +{} over {steady_frames}", capitalize(what), fmt::grouped(delta)),
            evidence: vec![format!(
                "{} at frame {} → {} at frame {}: {per_frame:+.1} a frame, up in {rising} of {} frames",
                fmt::grouped(values[first]),
                timeline.value(first, "frame").unwrap_or(0.0),
                fmt::grouped(values[last]),
                timeline.value(last, "frame").unwrap_or(0.0),
                rows.len() - 1
            ) + &kept],
            advice: format!("Frames add {what} that nothing removes: disconnect or destroy what a frame makes"),
            target: Target::Frame(last),
            weight: per_frame,
            ..Default::default()
        });
    }
}

/// Threads that stay alive through the steady state, from a newer ree's heap snapshots (the threads a full collection
/// keeps): those taken at the phase's start and end, from frame `from` to frame `to`
fn thread_growth(s: &Scoped, from: f64, to: f64, steady_frames: &str, issues: &mut Vec<Issue>, unchecked: &mut Vec<String>) {
    let p = s.p;
    let threads = |snapshot: &crate::model::Snapshot| snapshot.reachable.map(|_| snapshot.by_type.iter().filter(|row| row.ty == "thread").map(|row| row.count).sum::<f64>());
    let snapshots: Vec<(usize, f64)> = p.raw.heap.as_ref().map_or(Vec::new(), |heap| heap.snapshots.iter().enumerate().filter(|(_, snapshot)| (snapshot.frame as f64) >= from - 1.0 && (snapshot.frame as f64) <= to + 1.0).filter_map(|(index, snapshot)| threads(snapshot).map(|count| (index, count))).collect());
    let (Some(&(start, before)), Some(&(end, after))) = (snapshots.first(), snapshots.last()) else {
        unchecked.push("threads keep growing: needs heap snapshots at the steady phase's start and end (the timeline counts the threads made a frame, not those alive)".into());
        return;
    };
    let heap = &p.raw.heap.as_ref().expect("snapshots").snapshots;
    let frames = (heap[end].frame as f64 - heap[start].frame as f64).max(1.0);
    let (delta, per_frame) = (after - before, (after - before) / frames);
    if start == end || delta < 20.0 || per_frame < 0.5 {
        return;
    }
    issues.push(Issue {
        severity: if per_frame >= 10.0 { Severity::Serious } else { Severity::Warning },
        kind: "growth",
        title: format!("Threads keep growing: +{} over {steady_frames}", fmt::grouped(delta)),
        evidence: vec![format!(
            "{} threads reachable at frame {} → {} at frame {}: {per_frame:+.1} a frame (what a full collection keeps, at the heap snapshots)",
            fmt::grouped(before),
            heap[start].frame,
            fmt::grouped(after),
            heap[end].frame
        )],
        advice: "Frames start threads that never finish, or keep finished ones: end or drop what a frame starts".into(),
        target: Target::Snapshot(end),
        weight: per_frame,
        ..Default::default()
    });
}

/// The threads the last heap snapshot found reachable
fn reachable_threads(p: &Profile) -> Option<f64> {
    let snapshot = p.raw.heap.as_ref()?.snapshots.last()?;
    snapshot.reachable?;
    Some(snapshot.by_type.iter().filter(|row| row.ty == "thread").map(|row| row.count).sum())
}

fn capitalize(word: &str) -> String {
    let mut chars = word.chars();
    chars.next().map_or(String::new(), |first| first.to_uppercase().collect::<String>() + chars.as_str())
}

/// The reachable heap growing from the first snapshot past startup to the last
fn heap_growth(s: &Scoped, issues: &mut Vec<Issue>) {
    let p = s.p;
    let Some(heap) = &p.raw.heap else { return };
    let snapshots = &heap.snapshots;
    if snapshots.len() < 3 {
        return;
    }
    // (from the first mark after the start, so the mount's own memory is not counted as growth)
    let (from, to) = (1, snapshots.len() - 1);
    let (before, _, _) = snapshots[from].split();
    let (after, _, _) = snapshots[to].split();
    let delta = after - before;
    if delta < 256.0 * 1024.0 || delta < before * 0.25 {
        return;
    }
    issues.push(Issue {
        severity: if delta >= 8.0 * 1024.0 * 1024.0 { Severity::Serious } else { Severity::Warning },
        kind: "heap growth",
        title: format!("Reachable heap grows: +{} from {} to {}", fmt::bytes(delta), snapshots[from].label, snapshots[to].label),
        evidence: vec![format!("{} at {} (frame {}) → {} at {} (frame {})", fmt::bytes(before), snapshots[from].label, snapshots[from].frame, fmt::bytes(after), snapshots[to].label, snapshots[to].frame)],
        advice: "Heap's biggest holders say what keeps it".into(),
        target: Target::Snapshot(to),
        weight: delta,
        ..Default::default()
    });
}

/// The charged split of a cache site's work (us), in the reported phases, as a newer ree records it for a whole site or
/// for one calling context of it
struct ChargeSplit<'a> {
    /// The charge for a table's first touch in a frame, which a fix keeps paying
    first_touch_us: Option<f64>,
    /// The charge for the outcomes themselves
    outcome_charged_us: Option<f64>,
    /// What building the table at its final size costs instead of a rehash, which a fix also keeps paying
    presized_us: Option<f64>,
    outcomes: Option<&'a BTreeMap<String, f64>>,
}

impl<'a> ChargeSplit<'a> {
    fn of_site(site: &'a crate::model::CacheSite) -> Self {
        Self { first_touch_us: site.first_touch_us, outcome_charged_us: site.outcome_charged_us, presized_us: site.presized_us, outcomes: site.outcomes.as_ref() }
    }

    fn of_context(context: &'a crate::model::CacheContext) -> Self {
        Self { first_touch_us: context.first_touch_us, outcome_charged_us: context.outcome_charged_us, presized_us: context.presized_us, outcomes: context.outcomes.as_ref() }
    }

    fn rehashed(&self) -> bool {
        self.outcomes.is_some_and(|outcomes| outcomes.get("rehashed").copied().unwrap_or(0.0) > 0.0)
    }

    /// What fixing the site saves on the work clock (us), if the profile splits the charge: the charged outcomes, less
    /// what the replacement costs (building the table presized, where the outcome was a rehash). None when the charge is
    /// not split, or the replacement costs more than the outcomes did.
    fn saving_us(&self) -> Option<f64> {
        self.first_touch_us?;
        let charged = self.outcome_charged_us?;
        let replacement = if self.rehashed() { self.presized_us? } else { 0.0 };
        (replacement <= charged).then_some(charged - replacement)
    }
}

/// A cache site's figure in ms a frame, from its per-frame microseconds, else its microseconds over all `frames`
fn per_frame_ms(per_frame_us: Option<f64>, all_us: Option<f64>, frames: f64) -> Option<f64> {
    per_frame_us.or_else(|| all_us.map(|us| us / frames)).map(|us| us / 1000.0)
}

/// What fixing a cache site saves from the work the clock charged, ms a frame
fn charged_cache_saving_ms(site: &crate::model::CacheSite, frames: f64) -> Option<f64> {
    ChargeSplit::of_site(site).saving_us().map(|us| us / frames / 1000.0)
}

/// The work a cache fix removes, by calling context. Aggregate site rows keep their old totals; only their context
/// records establish intersections, so a site without them has no work set.
fn cache_context_work(site: &crate::model::CacheSite) -> Option<WorkSet> {
    let key = format!("cache:{}:{}", site.function, site.pc);
    let mut work = WorkSet::default();
    if let Some(contexts) = &site.nodes {
        for context in contexts {
            let saving_us = ChargeSplit::of_context(context).saving_us()?;
            work.sites.push(SiteWork { node: context.node, key: key.clone(), saving: saving_us / 1000.0, ..SiteWork::default() });
        }
    } else {
        let node = site.node?;
        let saving_us = ChargeSplit::of_site(site).saving_us()?;
        work.sites.push(SiteWork { node, key, saving: saving_us / 1000.0, ..SiteWork::default() });
    }
    Some(work)
}

// The cost over hits of the outcomes the work clock did not charge at a cache site, ms a frame: the profile's own figure,
/// else the committed price of each outcome the VM says is uncharged and not within its noise
fn uncharged_cache_work_ms(site: &crate::model::CacheSite, caches: &crate::model::LuauCaches, frames: f64) -> Option<f64> {
    if let Some(ms) = per_frame_ms(site.extra_work_us_per_frame, site.extra_work_us, frames) {
        return Some(ms);
    }
    let kind = caches.prices.as_ref()?.kinds.get(&site.kind)?;
    let outcomes = site.outcomes.as_ref()?;
    let mut total_ns = 0.0;
    for (name, count) in outcomes {
        let Some(price) = kind.outcomes.get(name) else { continue };
        if price.charged || price.within_noise {
            continue;
        }
        if let Some(ns) = price.ns {
            total_ns += ns * count;
        }
    }
    Some(total_ns / 1e6 / frames)
}

/// The calibration's low and high for the uncharged work at a cache site, ms a frame. This is the spread of an estimate,
/// so it is never added to a clock saving. None when the site has no uncharged outcome with a calibrated spread.
fn cache_estimate_range(site: &crate::model::CacheSite, caches: &crate::model::LuauCaches, frames: f64) -> Option<(f64, f64)> {
    let kind = caches.prices.as_ref()?.kinds.get(&site.kind)?;
    let mut low_ns = 0.0;
    let mut high_ns = 0.0;
    let mut any_uncharged = false;
    for (outcome, count) in site.outcomes.as_ref()? {
        for (price, lookups) in outcome_prices(site, kind, outcome, *count)? {
            if price.charged || price.within_noise || lookups <= 0.0 {
                continue;
            }
            any_uncharged = true;
            low_ns += price.low_ns? * lookups;
            high_ns += price.high_ns? * lookups;
        }
    }
    any_uncharged.then_some((low_ns / 1e6 / frames, high_ns / 1e6 / frames))
}

/// The prices that apply to `count` lookups with one outcome, each with the number of lookups it prices. A metatable
/// lookup is priced by how deep its walk went; None when a recorded depth has no price. An outcome with no price in the
/// calibration has none to give.
fn outcome_prices<'a>(site: &crate::model::CacheSite, kind: &'a crate::model::CachePriceKind, outcome: &str, count: f64) -> Option<Vec<(&'a crate::model::CacheOutcomePrice, f64)>> {
    if outcome != "metatable_table" {
        return Some(match kind.outcomes.get(outcome) {
            Some(price) => vec![(price, count)],
            None => Vec::new(),
        });
    }
    let mut walks = Vec::new();
    let mut walks_with_depth = 0.0;
    for (depth, walks_at_depth) in &site.depth {
        walks.push((kind.metatable_depths.get(depth)?, *walks_at_depth));
        walks_with_depth += walks_at_depth;
    }
    // Walks whose depth was not recorded are priced at depth 1
    if walks_with_depth < count {
        walks.push((kind.metatable_depths.get("1")?, count - walks_with_depth));
    }
    Some(walks)
}

/// A cache site's price on the work clock as (charged, uncharged) ms a frame, or None when it cannot be priced
fn work_clock_cache_price(site: &crate::model::CacheSite, caches: &crate::model::LuauCaches, frames: f64) -> Option<(f64, f64)> {
    let charged = charged_cache_saving_ms(site, frames);
    let uncharged = uncharged_cache_work_ms(site, caches, frames);
    let has_unsplit_charge = site.charged_work_us.unwrap_or(0.0) > 0.0;
    match (charged, uncharged) {
        // Only an unsplit charge: a newer profile is needed to say what fixing the site saves
        (None, Some(extra)) if extra <= 0.0 && has_unsplit_charge => None,
        (None, None) => None,
        _ => Some((charged.unwrap_or(0.0), uncharged.unwrap_or(0.0))),
    }
}

/// The outcome a cache site's lookups had most often besides a hit, with its count (a newer ree names what happened at each
/// lookup and what removes it). Lookups on a value that is not a table, or on a read-only one, are not misses to fix.
fn commonest_miss_outcome(site: &crate::model::CacheSite) -> Option<(String, f64)> {
    let outcomes = site.outcomes.as_ref()?;
    let mut commonest: Option<(&String, f64)> = None;
    for (name, &count) in outcomes {
        if matches!(name.as_str(), "hit" | "not_table" | "readonly") || count <= 0.0 {
            continue;
        }
        // (the larger count wins; equal counts go to the alphabetically earlier name)
        let beats = match commonest {
            None => true,
            Some((best_name, best_count)) => count.total_cmp(&best_count).then(best_name.cmp(name)).is_gt(),
        };
        if beats {
            commonest = Some((name, count));
        }
    }
    commonest.map(|(name, count)| (name.clone(), count))
}

/// What a cache site's lookups did, in words: each outcome but a hit with its count (an older ree counts only the
/// predicted wrong and the slow)
fn missed_outcomes_words(site: &crate::model::CacheSite) -> String {
    match &site.outcomes {
        Some(outcomes) => {
            let mut words = Vec::new();
            for (name, count) in outcomes {
                if name.as_str() != "hit" && *count > 0.0 {
                    words.push(format!("{} {}", name.replace('_', " "), fmt::grouped(*count)));
                }
            }
            words.join(", ")
        }
        None => format!("{} predicted wrong, {} slow", fmt::grouped(site.misses), fmt::grouped(site.slow)),
    }
}

/// A cache site's price, ms a frame. On the work clock it is only what the clock charged, so a site that charged nothing
/// has no price however large its uncharged estimate is. In host time (an older profile) it is what the misses cost.
fn cache_price_ms(work_clock: bool, charged: f64, uncharged: f64) -> Option<f64> {
    if !work_clock {
        return Some(uncharged);
    }
    (charged > 0.0).then_some(charged)
}

/// What a cache issue says its price rests on, or why it has none
fn cache_unpriced_reason(ms: Option<f64>, work_clock: bool, context_missing: bool, charge_unsplit: bool) -> &'static str {
    if context_missing {
        return "needs per-context site records";
    }
    match (ms, work_clock) {
        (Some(_), true) if charge_unsplit => "not priced: needs a newer profile for the charged part (first touch and replacement cost are not split out)",
        (Some(_), true) => "first touch retained; charged outcomes less the replacement constructor; uncharged estimates shown separately",
        (Some(_), false) => "measured host time on this machine, a frame over the run (the work clock charges a miss as a hit)",
        (None, true) => "needs a newer profile: first touch and replacement cost are not split out",
        (None, false) => "the work clock charges a miss as a hit, and this profile has no price of one",
    }
}

/// Luau inline cache sites whose lookups miss often, by what happened at them (a newer ree splits the outcomes: a key
/// predicted at the wrong slot, absent, inserted, a rehash, a metatable walked, an unsafe environment). Priced in a newer
/// ree by what the work clock charged at the site past its dispatch (a rehash, a looked-up import: the function's work,
/// claimed from it) and what the outcomes it does not charge cost over hits at the committed prices (beside the game's
/// measured work, claiming none of it); in an older one by host time measured over the run, which claims no work
fn cache_misses(s: &Scoped, issues: &mut Vec<Issue>) {
    let p = s.p;
    let Some(caches) = &p.raw.luau_caches else { return };
    // (a newer ree counts a site's outcomes in the reported phases and prices them on the work clock; an older one counts
    // the run and measured host time)
    let work_clock = p.cache_prices_work();
    let frames = if work_clock { s.frames.unwrap_or(p.raw.run.frames as f64).max(1.0) } else { p.raw.run.frames.max(1) as f64 };
    let all: f64 = caches.sites.iter().map(|site| site.missed()).sum();
    // (charged, uncharged) ms a frame, or None
    let priced = |site: &crate::model::CacheSite| -> Option<(f64, f64)> {
        if work_clock {
            work_clock_cache_price(site, caches, frames)
        } else {
            per_frame_ms(site.extra_us_per_frame, site.extra_us, frames).map(|ms| (0.0, ms))
        }
    };
    let total = |site: &crate::model::CacheSite| priced(site).map(|(charged, uncharged)| charged + uncharged);
    let mut rows: Vec<(usize, f64, f64)> = caches
        .sites
        .iter()
        .enumerate()
        .map(|(index, site)| (index, site.missed(), site.runs()))
        .filter(|(index, missed, runs)| match total(&caches.sites[*index]) {
            Some(_) => *missed > 0.0,
            None => *missed >= 1000.0 && *missed >= 0.2 * *runs,
        })
        .collect();
    // (the engine's own Luau, ree's own code and the harness's are left out before the top sites are taken)
    rows.retain(|(index, _, _)| {
        let site = &caches.sites[*index];
        p.func_of_id(site.function).is_some_and(|func| {
            let f = &p.funcs[func];
            !(f.kind == Kind::Engine || f.tool || site.tool == Some(true) || (s.harness_out && (f.harness || site.harness == Some(true))))
        })
    });
    rows.sort_by(|a, b| total(&caches.sites[b.0]).unwrap_or(0.0).total_cmp(&total(&caches.sites[a.0]).unwrap_or(0.0)).then(b.1.total_cmp(&a.1)).then(a.0.cmp(&b.0)));
    for (index, missed, runs) in rows {
        let site = &caches.sites[index];
        let Some(func) = p.func_of_id(site.function) else { continue };
        let f = &p.funcs[func];
        // (a newer ree names what happened at each lookup and what removes it: the commonest outcome other than a hit)
        let outcome = commonest_miss_outcome(site);
        let keyed = outcome.as_ref().and_then(|(name, _)| caches.outcome_info.get(name)).filter(|info| !info.advice.is_empty());
        // (what a miss is depends on the cache: the profiler reads each as the interpreter will, lprofile.cpp)
        let advice = match outcome.as_ref().map(|(name, _)| name.as_str()) {
            Some("rehashed") => outcome_advice("rehashed").to_string(),
            Some(name) => keyed.map_or_else(|| outcome_advice(name).to_string(), |info| info.advice.clone()),
            None => legacy_advice(site).to_string(),
        };
        let split = priced(site);
        let context_work = cache_context_work(site);
        let charged = split.map_or(0.0, |(charged, _)| charged);
        let context_missing = work_clock && charged > 0.0 && site.node.is_none() && site.nodes.is_none();
        let context_unpriced = work_clock && charged > 0.0 && context_work.is_none();
        let ms = match split {
            Some((charged, uncharged)) if !context_unpriced => cache_price_ms(work_clock, charged, uncharged),
            _ => None,
        };
        // (a work-clock site whose charge is not split into first touch, outcomes and replacement: what fixing it saves is unknown)
        let charge_unsplit = site.outcome_charged_us.is_none() && site.charged_work_us.unwrap_or(0.0) > 0.0;
        let key = site.key.as_ref().filter(|key| !key.is_empty()).map(|key| format!(" {key}")).unwrap_or_default();
        let what = missed_outcomes_words(site);
        let over = if work_clock { s.frames_words(frames) } else { format!("the run's {} (caches are counted over the whole run)", counted(frames, "frame", "frames")) };
        let mut evidence = vec![format!("function: {}", f.label().replace("  ", " ")), format!("{} of {} missed ({what}), {:.0} a frame over {over}; {} of all misses", fmt::grouped(missed), fmt::grouped(runs), missed / frames, fmt::share(missed, all))];
        if let (Some((name, count)), Some(info)) = (&outcome, keyed) {
            evidence.push(format!("most often {} ({}): {}", name.replace('_', " "), fmt::grouped(*count), info.meaning));
        }
        if let (true, Some((charged, uncharged))) = (work_clock, split) {
            evidence.push(format!("a frame: {} saved from charged outcomes after retaining first touch and paying the replacement constructor; {} uncharged work beyond the measured clock", fmt::ms(charged), fmt::ms(uncharged)));
            if charge_unsplit {
                evidence.push("not priced: needs a newer profile for the charged part (first touch and replacement cost are not split out)".into());
            }
        }
        if let Some(noise) = site.within_noise.filter(|noise| *noise > 0.0) {
            evidence.push(format!("{} priced within their noise, left out of the price", counted(noise, "outcome", "outcomes")));
        }
        let only_an_estimate = work_clock && charged == 0.0 && split.is_some_and(|(_, uncharged)| uncharged > 0.0);
        let unpriced = if only_an_estimate {
            "uncharged calibration estimate; no saving from the work clock is recorded"
        } else {
            cache_unpriced_reason(ms, work_clock, context_missing, charge_unsplit)
        };
        // Every cache fix is a source edit that is not on the allowlist ([`edit_reach`]), so it takes the default bound: all
        // the code's work. (Older profiles price cache misses in host time, which is no work-clock claim, so no bound.)
        let upper_ms = work_clock.then_some(s.code_work / frames);
        issues.push(s.priced(Issue {
            severity: Severity::Info,
            kind: "cache misses",
            summary_key: format!("cache:{}:{}", site.function, site.pc),
            upper_ms,
            bound_reason: "other code changed by the advised source edit",
            title: format!("{}{key} at {}:{} misses {} of its lookups", site.op, f.short, site.line, fmt::share(missed, runs)),
            evidence,
            advice,
            target: Target::Cache(index),
            ms,
            host_time: ms.is_some() && !work_clock,
            uncharged: if work_clock { split.map_or(0.0, |(_, uncharged)| uncharged) } else { 0.0 },
            estimate_range: cache_estimate_range(site, caches, frames),
            work: context_work.unwrap_or_default(),
            claims: match split {
                Some((charged, _)) if work_clock && charged > 0.0 => vec![(func as u32, charged)],
                _ => Vec::new(),
            },
            unpriced: Some(unpriced.into()),
            weight: missed,
            ..Default::default()
        }));
    }
}

/// The builtins whose fast path needs more arguments than a script passes by default: the fix adds instructions (and,
/// for some, cold code), which the price pays for
fn builtin_fix_adds_code(builtin_name: &str) -> bool {
    matches!(builtin_name, "string.sub" | "string.byte" | "table.insert" | "table.unpack")
}

/// What a builtin fix adds beyond the fast path (instructions or cold code), us. A builtin whose fix adds nothing has none
/// to measure. For one whose fix adds code the profile has to say, unless the setup is unknown (`bounded_setup`): then the
/// price assumes the fix keeps none of it. None when the profile should have said and did not.
fn added_fix_cost_us(measured: Option<f64>, needs_setup: bool, bounded_setup: bool) -> Option<f64> {
    if bounded_setup {
        return Some(0.0);
    }
    if needs_setup {
        return measured;
    }
    Some(measured.unwrap_or(0.0))
}

/// What an edit that stops one line's builtin calls being declined costs and saves, added up over the line's sites.
/// Units are the profile's: us over the reported frames, except `cold_ms`.
#[derive(Default)]
struct DeclinedEdit {
    /// The declined calls (all contexts)
    calls: f64,
    /// What the work clock charged those calls
    charged_us: f64,
    /// What the same calls cost on the fast path, plus the fix's added instructions and cold code
    replacement_us: f64,
    /// The cold code the fix adds, ms (paid once while any context still needs the fix)
    cold_ms: f64,
    /// The work the fix removes
    work: WorkSet,
    /// The decline reasons the calls had
    reasons: BTreeSet<String>,
    /// Why the edit cannot be priced; the last reason found wins
    missing: Option<&'static str>,
}

impl DeclinedEdit {
    /// Adds one compiled site: each of its calling contexts, then the cold code its fix adds
    fn add_site(&mut self, s: &Scoped, builtin_name: &str, fix: &str, site: &crate::model::BuiltinSite) {
        let needs_setup = builtin_fix_adds_code(builtin_name);
        // The source rewrite's receiver loads and register moves cannot be recovered from the counters, so the price
        // assumes the fix keeps none of its added instructions and is only an upper bound
        let bounded_setup = needs_setup && !site.fix_cost_exact;
        if site.fix_unpriced.is_some() && !bounded_setup {
            self.missing = Some("the advised replacement's instruction cost is unavailable");
        }
        let site_key = format!("builtin:{}:{}", site.function, site.pc);
        for context in site.contexts() {
            let declines: f64 = context.declines.values().sum();
            if declines <= 0.0 {
                continue;
            }
            self.calls += declines;
            self.reasons.extend(context.declines.iter().filter(|(_, count)| **count > 0.0).map(|(reason, _)| reason.clone()));
            let Some(node) = context.node else {
                self.missing = Some("needs per-context site records");
                continue;
            };
            let (Some(charged_us), Some(fast_us)) = (context.decline_charged_us, context.fast_equivalent_us) else {
                self.missing = Some("needs a newer profile: the same calls' fast-path replacement cost is missing");
                continue;
            };
            let Some(added_instructions_us) = added_fix_cost_us(context.fix_instructions_us, needs_setup, bounded_setup) else {
                self.missing = Some("needs a newer profile: the fix's added instruction cost is missing");
                continue;
            };
            self.charged_us += charged_us;
            self.replacement_us += fast_us + added_instructions_us;
            let saving_us = charged_us - fast_us - added_instructions_us;
            let shared_us = self.add_shared_cache_work(s, site, node, fix);
            self.work.sites.push(SiteWork { node, key: site_key.clone(), saving: (saving_us - shared_us) / 1000.0, fix: fix.to_string() });
        }
        match added_fix_cost_us(site.fix_cold_us, needs_setup, bounded_setup) {
            Some(cold_us) => {
                self.replacement_us += cold_us;
                self.cold_ms += cold_us / 1000.0;
            }
            None if self.missing.is_none() => self.missing = Some("needs a newer profile: the fix's added cold-code cost is missing"),
            None => {}
        }
    }

    /// Moves the cache lookups that sit in a declined call's fallback into the edit's work, as sites of their own, and
    /// returns the saving moved (us). The fallback runs the lookups' instructions inside the decline's charge, so booking
    /// them again at the decline would count that work twice. The lookups are those of the same function after the call,
    /// up to the fallback's last instruction, in the same call-tree context.
    fn add_shared_cache_work(&mut self, s: &Scoped, site: &crate::model::BuiltinSite, node: usize, fix: &str) -> f64 {
        let Some(caches) = &s.p.raw.luau_caches else { return 0.0 };
        let mut shared_us = 0.0;
        for cache in &caches.sites {
            let charged_anything = cache.charged_work_us.unwrap_or(0.0) > 0.0;
            if cache.function != site.function || !charged_anything || cache.pc <= site.pc as i64 {
                continue;
            }
            let Some(fallback_end) = site.fallback_end_pc else {
                self.missing = Some("needs a newer profile to identify shared fallback cache instructions");
                continue;
            };
            if cache.pc > fallback_end as i64 {
                continue;
            }
            let Some(cache_work) = cache_context_work(cache) else {
                self.missing = Some("needs per-context site records for shared fallback instructions");
                continue;
            };
            for mut part in cache_work.sites {
                if part.node != node {
                    continue;
                }
                shared_us += part.saving * 1000.0;
                part.fix = fix.to_string();
                self.work.sites.push(part);
            }
        }
        shared_us
    }

    /// Records the frames in which each context of the function was active, so the cold code's cost is paid only while
    /// some context still needs the fix (see [`WorkSet::total`])
    fn add_cold_code_contexts(&mut self, s: &Scoped, function: i64, fix: &str) {
        let mut frames_by_context: BTreeMap<usize, BTreeSet<i64>> = BTreeMap::new();
        if let Some(timeline) = &s.p.raw.timeline {
            // (a cell is a function, a node, then inclusive [first frame, last frame] ranges)
            for cells in &timeline.function_contexts {
                if cells.len() < 4 || cells[0] != function {
                    continue;
                }
                let frames = frames_by_context.entry(cells[1] as usize).or_default();
                for row in s.rows() {
                    let Some(frame) = timeline.value(row, "frame").map(|frame| frame as i64) else { continue };
                    let (ranges, _) = cells[2..].as_chunks::<2>();
                    if ranges.iter().any(|range| frame >= range[0] && frame <= range[1]) {
                        frames.insert(frame);
                    }
                }
            }
        }
        if frames_by_context.is_empty() {
            self.missing = Some("needs per-context prototype activity for the added cold code");
        } else {
            self.work.replacement_contexts.insert(fix.to_string(), frames_by_context);
        }
    }
}

/// A declined call is priced only when the profile states the replacement's cost for those same calls.
fn builtin_declines(s: &Scoped, builtins: &[Builtin], issues: &mut Vec<Issue>) {
    for builtin in builtins {
        // One source edit also changes copies the compiler unrolled or inlined on this line.
        let mut sites_by_line: BTreeMap<(i64, i64), Vec<&crate::model::BuiltinSite>> = BTreeMap::new();
        for site in &builtin.sites {
            sites_by_line.entry((site.function, site.line)).or_default().push(site);
        }
        for ((function, line), sites) in sites_by_line {
            let Some(func) = s.p.func_of_id(function) else { continue };
            let f = &s.p.funcs[func];
            if f.tool || (s.harness_out && f.harness) {
                continue;
            }
            if let Some(issue) = declined_calls_issue(s, builtin, function, line, func, &sites) {
                issues.push(issue);
            }
        }
    }
}

/// The issue for one line's declined calls of a builtin, None when there were none
fn declined_calls_issue(s: &Scoped, builtin: &Builtin, function: i64, line: i64, func: usize, sites: &[&crate::model::BuiltinSite]) -> Option<Issue> {
    let f = &s.p.funcs[func];
    let needs_setup = builtin_fix_adds_code(&builtin.name);
    let fix = format!("builtin:{}:{function}:{line}", builtin.name);
    let mut edit = DeclinedEdit::default();
    for site in sites {
        edit.add_site(s, &builtin.name, &fix, site);
    }
    if edit.calls <= 0.0 {
        return None;
    }
    if edit.cold_ms > 0.0 {
        edit.add_cold_code_contexts(s, function, &fix);
    }
    edit.work.replacements.insert(fix, edit.cold_ms);

    // The site rows' own totals say whether the calls were already no dearer than the fast path
    let site_totals = sites.iter().try_fold((0.0, 0.0), |(charged, fast), site| Some((charged + site.decline_charged_us?, fast + site.fast_equivalent_us?)));
    let already_no_dearer = builtin.name != "table.unpack" && site_totals.is_some_and(|(charged, fast)| charged <= fast);
    let site_saves_nothing = already_no_dearer || (edit.missing.is_none() && edit.charged_us <= edit.replacement_us);
    let reach = edit_reach(&builtin.name, &edit.reasons);
    // Only an edit that keeps its operands can say the complete fix saves nothing. For any other edit a site part
    // that saves nothing leaves the whole-code bound standing: the outside work may still be saved.
    let no_saving = site_saves_nothing && reach == EditReach::SiteOnly;
    if site_saves_nothing {
        edit.missing = Some(match reach {
            EditReach::SiteOnly => FIX_SAVES_NOTHING,
            EditReach::WholeCode => SITE_PART_SAVES_NOTHING,
        });
        if let Some((charged, fast)) = site_totals {
            edit.charged_us = charged;
            let added_us: f64 = sites.iter().map(|site| site.fix_instructions_us.unwrap_or(0.0) + site.fix_cold_us.unwrap_or(0.0)).sum();
            edit.replacement_us = fast + added_us;
        }
        edit.work = WorkSet::default();
    }

    let mut advice: Vec<&str> = Vec::new();
    let mut reasons_without_advice: Vec<&str> = Vec::new();
    for reason in &edit.reasons {
        match advice_for_decline(builtin, reason) {
            Some(text) => advice.push(text),
            None => reasons_without_advice.push(reason),
        }
    }
    let unsafe_env = edit.reasons.contains("unsafe_env");
    if unsafe_env {
        // An early unsafe_env rejection hides the checks after it, so fixing the environment alone proves nothing
        advice.push("This bound requires the combination: restore the safe environment AND supply the types, values, argument count and result count the fast path accepts. The early unsafe_env rejection hides those later checks; removing getfenv alone does not establish this saving");
        advice.push("Removing the environment calls also removes their own calls and changes other code's work, so the complete edit can save more than this site's subtotal");
    }
    // (a string.sub's gc_debt declines have no advice of their own, and do not stop the edit from being priced)
    let only_gc_debt_unadvised = builtin.name == "string.sub" && reasons_without_advice.iter().all(|reason| *reason == "gc_debt");
    if edit.missing.is_none() && !reasons_without_advice.is_empty() && !only_gc_debt_unadvised && !unsafe_env {
        edit.missing = Some("the profile has no equivalent advised fix for every declined call");
    }
    let adds_instructions = sites.iter().any(|site| site.fix_instructions_us.is_some_and(|us| us > 0.0));
    if needs_setup && adds_instructions && !edit.reasons.contains("args") {
        advice.push("Also pass the omitted end as #s so string.sub can take its fast path");
    }
    let adds_cold_code = sites.iter().any(|site| site.fix_cold_us.is_some_and(|us| us > 0.0));
    if builtin.name == "string.byte" && adds_cold_code && !edit.reasons.contains("args") {
        advice.push("Also pass the first index explicitly: string.byte(s, 1)");
    }
    let setup_is_unknown = needs_setup && sites.iter().any(|site| !site.fix_cost_exact);
    if setup_is_unknown {
        advice.push("The price is an upper bound: the fixed form's receiver loads, register moves, argument instructions and cold code are not recoverable from these counters; the net saving can be zero");
    }

    let price = edit.missing.is_none().then_some((edit.charged_us - edit.replacement_us) / 1000.0 / s.frame_count());
    // The bound does not depend on the site part: the complete edit can save outside work even when that part is missing
    // or nonpositive
    let upper_ms = match reach {
        EditReach::WholeCode => Some(s.code_work / s.frame_count()),
        EditReach::SiteOnly => None,
    };
    let bound_reason = if unsafe_env {
        "the environment calls themselves and other work changed by removing them"
    } else {
        "the caller's instructions and the evaluation of its arguments"
    };
    let location = if line > 0 { format!("{}:{line}", f.short) } else { f.label().replace("  ", " ") };
    let some_fix_cost_missing = sites.iter().any(|site| site.fix_instructions_us.is_none() || site.fix_cold_us.is_none());
    let lower_bound = if needs_setup && some_fix_cost_missing { "at least " } else { "" };
    let unpriced = edit.missing.map(str::to_owned);
    Some(s.priced(Issue {
        kind: "builtin declines",
        summary_key: format!("builtin:{}:{function}:{line}", builtin.name),
        at_most: setup_is_unknown || unsafe_env,
        upper_ms,
        bound_reason,
        title: format!("{} at {location}: {} declined calls", builtin.name, fmt::grouped(edit.calls)),
        evidence: vec![
            format!("charged {} minus replacement {lower_bound}{} over the reported phases", fmt::ms(edit.charged_us / 1000.0), fmt::ms(edit.replacement_us / 1000.0)),
            format!("function: {}", f.label().replace("  ", " ")),
        ],
        advice: advice.join("; "),
        work: edit.work,
        ms: price,
        no_saving,
        target: if line > 0 { Target::Line(func as u32, line) } else { Target::Func(func as u32) },
        unpriced,
        ..Default::default()
    }))
}

/// How far an advised source edit reaches beyond the declined calls the profile recorded, which decides its bound and
/// whether it may say "fixing it saves nothing"
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EditReach {
    /// The default. The edit can change code the recorded site part does not hold (an operand's evaluation, the caller's
    /// instructions, the call itself), so its bound is all the scoped code work (`upper_ms`). A site part that saves
    /// nothing says only that the recorded part saves nothing
    WholeCode,
    /// The edit only adds what the call left out and keeps every original operand, so the site part is all it changes
    /// and is its own bound. Only this kind may say that fixing it saves nothing on this clock
    SiteOnly,
}

/// An advised rewrite that keeps every original operand and adds nothing else
struct OperandPreservingRewrite {
    builtin: &'static str,
    /// The decline reasons that may come with `args` without asking for another source edit
    also_reasons: &'static [&'static str],
}

/// The allowlist of [`EditReach::SiteOnly`] rewrites. A rewrite not listed here, or with any reason besides `args` and
/// its own `also_reasons`, is [`EditReach::WholeCode`]
const OPERAND_PRESERVING_REWRITES: &[OperandPreservingRewrite] = &[
    // string.byte(s) becomes string.byte(s, 1): the string expression is still evaluated
    OperandPreservingRewrite { builtin: "string.byte", also_reasons: &[] },
    // string.sub(s, i) becomes string.sub(s, i, #s): the string and start expressions are still evaluated, and a
    // gc_debt decline asks for no edit of its own
    OperandPreservingRewrite { builtin: "string.sub", also_reasons: &["gc_debt"] },
];

/// Which bound an advised rewrite of a builtin's declined calls gets: the default, unless it is on the allowlist. It
/// is on it only when the calls declined for missing arguments (`args`), and for nothing else the allowlist does not
/// name, since any other reason asks for a different edit
fn edit_reach(builtin: &str, reasons: &BTreeSet<String>) -> EditReach {
    let Some(rewrite) = OPERAND_PRESERVING_REWRITES.iter().find(|rewrite| rewrite.builtin == builtin) else {
        return EditReach::WholeCode;
    };
    if !reasons.contains("args") {
        return EditReach::WholeCode;
    }
    for reason in reasons {
        let allowed = reason == "args" || rewrite.also_reasons.contains(&reason.as_str());
        if !allowed {
            return EditReach::WholeCode;
        }
    }
    EditReach::SiteOnly
}

/// What fixes one reason a builtin's calls were declined: the profile's advice for it, else ree's own
fn advice_for_decline<'a>(builtin: &'a Builtin, reason: &str) -> Option<&'a str> {
    match builtin.advice.get(reason) {
        Some(advice) => Some(advice.as_str()),
        None => builtin_advice(&builtin.name, reason),
    }
}

// Older profiles carry the complete numeric-call price without this advice entry.
fn builtin_advice(name: &str, reason: &str) -> Option<&'static str> {
    match (name, reason) {
        ("math.abs", "types") => Some("Pass the existing numeric value directly instead of its string form; this saving includes no added conversion"),
        _ => None,
    }
}

/// What helps a cache outcome, when the profile gives none (ree's report.rs OUTCOME_INFO says the same)
fn outcome_advice(outcome: &str) -> &'static str {
    match outcome {
        "repredicted" => "The tables here differ in shape (keys added in other orders, or other keys): build them alike",
        "absent" => "The key is missing: store a value such as false, or test for it once outside the loop",
        "inserted" => "A new key each time: create the table with its fields, in one constructor",
        "rehashed" => "Allocate the final key slots in the table constructor before these writes; keep the writes",
        "metatable_table" | "metatable_function" => "A metatable serves the lookup (__index): a plain field, or a local copy of the method, avoids the walk",
        "unsafe_env" => "The script's environment is unsafe (it calls getfenv or setfenv): drop those calls",
        _ => "The cache could not serve the lookup from its slot",
    }
}

/// What helps a site of a profile before the outcomes (its op and its counts)
fn legacy_advice(site: &crate::model::CacheSite) -> &'static str {
    let slow_mostly = site.slow > site.misses;
    match site.op.as_str() {
        op if slow_mostly && op.starts_with("FASTCALL") => "The script's environment is unsafe (it calls getfenv or setfenv), so no builtin takes its fast path: drop those calls",
        _ if slow_mostly => "A metatable serves the lookup (__index, __newindex), or the environment is unsafe: a plain field avoids it",
        op if op.starts_with("FASTCALL") => "The builtin's fast path turned these arguments down (a type, or a count, it does not take there: string.sub without its end, say), so each call went the full way: pass what the fast path takes",
        "NAMECALL" => "The method is not where the cache looked: objects of several shapes or classes meet here, or the method sits further up an __index chain",
        "GETGLOBAL" | "SETGLOBAL" => "A global the cache could not keep its slot for: read it into a local once",
        "GETIMPORT" => "The import was not resolved when the script loaded: a global the script sets, or one missing then",
        _ => "The tables here differ in shape (keys added in other orders, or other keys): build them alike",
    }
}

/// Whether a boundary site is in the harness's code (its script under the harness's folder)
fn harness_site(p: &Profile, site: &crate::model::BoundarySite) -> bool {
    crate::model::harness_module(crate::model::module_name(&site.script)) || boundary_func(p, site).is_some_and(|func| p.funcs[func as usize].harness)
}

/// Whether a boundary site is in ree's own code (never an issue)
fn tool_site(p: &Profile, site: &crate::model::BoundarySite) -> bool {
    boundary_func(p, site).is_some_and(|func| p.funcs[func as usize].tool)
}

/// The function a boundary site names (its script and line)
fn boundary_func(p: &Profile, site: &crate::model::BoundarySite) -> Option<u32> {
    let script = site.script.as_str();
    let line = site.line;
    let short = crate::model::short_module(crate::model::module_name(script));
    let in_the_script = |f: &crate::model::Func| f.kind == Kind::Script && (f.module == script || f.short == short);

    // The boundary records the function that was executing, as `name:line defined`. It stays right when the compiler
    // inlined a helper into it, which the helper's own source lines do not.
    if let Some((name, defined_line)) = site.function.rsplit_once(':')
        && let Ok(defined_line) = defined_line.parse::<i64>()
    {
        let is_the_executing_function = |f: &crate::model::Func| {
            let same_name = f.name == name || (name == "anonymous" && f.name.is_empty());
            in_the_script(f) && f.line == defined_line && same_name
        };
        if let Some(index) = p.funcs.iter().position(is_the_executing_function) {
            return Some(index as u32);
        }
    }

    // Otherwise the innermost function whose lines hold the site's line
    let holds_the_line = |f: &crate::model::Func| f.line <= line && line <= f.last_line.max(f.line);
    p.funcs
        .iter()
        .enumerate()
        .filter(|(_, f)| in_the_script(f) && holds_the_line(f))
        .max_by_key(|(_, f)| f.line)
        .map(|(index, _)| index as u32)
}

/// What the work clock charged a site's crossings in all, ms (its rate a second times the phase's seconds)
fn site_cost_ms(site: &crate::model::BoundarySite) -> f64 {
    if site.per_s > 0.0 { site.cost_ms_per_s * site.count / site.per_s } else { 0.0 }
}

/// The boundary profile's repeat reads and unchanged writes, each priced at what a change saves: what the work clock
/// charged the repeats (or the unchanged writes) themselves, their crossings and their script side (the keyed access and
/// the calls the VM charged while each ran), a site's first read in a frame keeping the frame's cold charge, times the
/// run's price scale, over the phase's frames. A boundary.json that does not split those out (an older ree's) leaves them
/// unpriced
fn repeat_reads(s: &Scoped, boundary: &crate::model::Boundary, issues: &mut Vec<Issue>) {
    let p = s.p;
    let frames = s.frame_count();
    // A removal's recorded subtotal leaves out instructions the edit also changes (a receiver load, say), and the profile
    // cannot say which. So the upper bound is all of the code's work in the reported phases
    let upper_bound_ms = s.code_work / frames;
    let floor = 0.0;
    let ns = |ns: f64| fmt::ms(ns * 1e-6);
    let read_price = |site: &crate::model::BoundarySite| site.repeat_cost_ms.map(|cost| cost * s.scale / frames);
    let mut reads: Vec<(&crate::model::BoundarySite, Option<f64>)> = boundary
        .repeat_reads
        .iter()
        .filter(|site| site.repeat_reads > 0.0 && !tool_site(p, site) && !(s.harness_out && harness_site(p, site)))
        .map(|site| (site, read_price(site)))
        .filter(|(site, ms)| ms.map_or(site.repeat_reads >= frames, |ms| ms >= floor))
        .collect();
    reads.sort_by(|a, b| b.1.unwrap_or(0.0).total_cmp(&a.1.unwrap_or(0.0)).then(b.0.repeat_reads.total_cmp(&a.0.repeat_reads)));
    for (site, ms) in reads {
        let per_frame = site.repeat_reads / frames;
        let func = boundary_func(p, site);
        let mut evidence = vec![format!(
            "{} repeat reads in {}, of {}: the same instance's {} read again by the same function",
            fmt::grouped(site.repeat_reads),
            counted(frames, "frame", "frames"),
            counted(site.count, "read", "reads"),
            site.member
        )];
        if let Some(cost) = site.repeat_cost_ms {
            evidence.push(format!(
                "a repeat cost {}, its crossing and the read itself (the keyed access and any call the VM charged); a site's reads average {} at the boundary, a frame's first taking the frame's cold charge, which a kept value keeps",
                ns(cost * 1e6 * s.scale / site.repeat_reads),
                ns(site_cost_ms(site) * 1e6 * s.scale / site.count.max(1.0))
            ));
        }
        // (a newer ree tells where the read again was, and counts a read again only when the value is the one read before)
        let advice = match (site.repeat_same_call, site.repeat_later_call) {
            (Some(same), Some(later)) => {
                let other = site.repeat_other_function.filter(|other| *other > 0.0).map(|other| format!("; another function read the same value {} times more (not counted here: share the read)", fmt::grouped(other))).unwrap_or_default();
                evidence.push(format!("{} in the same call, {} in later calls of the same function in the frame{other}", fmt::grouped(same), fmt::grouped(later)));
                if later > same { "Read it once in the caller and pass it in: later calls of the same function read it again" } else { "Keep it in a local: the same call reads it again" }
            }
            _ => "Keep one value per frame: pass it to later calls, or keep it in a local within one call",
        };
        issues.push(s.priced(Issue {
            kind: "repeat reads",
            upper_ms: ms.map(|_| upper_bound_ms),
            title: format!("{} read again in the frame: {per_frame:.0} a frame at {}:{}", site.member, crate::model::short_module(&site.script), site.line),
            evidence,
            advice: advice.into(),
            target: func.map_or(Target::None, |f| Target::Line(f, site.line)),
            ms,
            unpriced: ms.is_none().then(|| "this boundary.json does not split a site's repeat reads' cost from its first read's, which takes the frame's cold charge a kept value keeps (needs a newer ree)".into()),
            weight: site.repeat_reads,
            ..Default::default()
        }));
    }
    // (a newer ree lists every site with writes that changed nothing; a site may also be in either of the site tables, by
    // count or by cost)
    let mut seen: Vec<(&str, i64, &str)> = Vec::new();
    let mut writes: Vec<(&crate::model::BoundarySite, Option<f64>)> = Vec::new();
    for site in boundary.unchanged_writes.iter().chain(&boundary.by_site_count).chain(&boundary.by_site_cost) {
        let key = (site.script.as_str(), site.line, site.member.as_str());
        if site.unchanged_writes <= 0.0 || seen.contains(&key) || tool_site(p, site) || (s.harness_out && harness_site(p, site)) {
            continue;
        }
        seen.push(key);
        // (what the unchanged writes themselves cost; without that split, the site's cost is theirs only when every write
        // at it changed nothing)
        let cost = site.unchanged_cost_ms.or_else(|| (site.unchanged_writes >= site.count).then(|| site_cost_ms(site)));
        let ms = cost.map(|cost| cost * s.scale / frames);
        if ms.map_or(site.unchanged_writes >= frames, |ms| ms >= floor) {
            writes.push((site, ms));
        }
    }
    writes.sort_by(|a, b| b.1.unwrap_or(0.0).total_cmp(&a.1.unwrap_or(0.0)).then(b.0.unchanged_writes.total_cmp(&a.0.unchanged_writes)));
    for (site, ms) in writes {
        let func = boundary_func(p, site);
        let mut evidence = vec![format!("{} of {} in {} changed nothing", fmt::grouped(site.unchanged_writes), counted(site.count, "write", "writes"), counted(frames, "frame", "frames"))];
        if let Some(ms) = ms {
            evidence.push(format!("{} an unchanged write: {} a frame", ns(ms * frames * 1e6 / site.unchanged_writes), fmt::ms(ms)));
        }
        issues.push(s.priced(Issue {
            kind: "unchanged writes",
            upper_ms: ms.map(|_| upper_bound_ms),
            title: format!("{} set to its own value: {:.0} a frame at {}:{}", site.member, site.unchanged_writes / frames, crate::model::short_module(&site.script), site.line),
            evidence,
            advice: "Write only when the value changes".into(),
            target: func.map_or(Target::None, |f| Target::Line(f, site.line)),
            ms,
            unpriced: ms.is_none().then(|| "this boundary.json does not split the unchanged writes' cost from the site's other writes' (needs a newer ree)".into()),
            weight: site.unchanged_writes,
            ..Default::default()
        }));
    }
}

/// Functions whose own cycles went mostly to waiting for load data, from the hardware counters read around their sampled
/// calls: a function holding at least 5 % of the run's stall cycles, with at least 30 % of its own cycles stalled
fn memory_waits(p: &Profile, c: &crate::model::Counters, harness_out: bool, issues: &mut Vec<Issue>) {
    let index = |name: &str| c.names.iter().position(|known| known == name);
    let (Some(stalls), Some(cycles)) = (index("load-stall-cycles"), index("cycles")) else { return };
    let (occupancy, allocs) = (index("miss-buffer-occupancy"), index("miss-buffer-allocs"));
    let run_cycles = c.run.counts.get("cycles").copied().unwrap_or(0.0);
    let run_stalls = c.run.counts.get("load-stall-cycles").copied().unwrap_or(0.0);
    if run_cycles <= 0.0 || run_stalls <= 0.0 {
        return;
    }
    let mut rows: Vec<(usize, f64, f64, Vec<f64>)> = p
        .counter_funcs
        .iter()
        .filter(|(func, _)| p.funcs[**func].kind == Kind::Script && !(harness_out && p.funcs[**func].harness))
        .filter_map(|(&func, (own, _))| {
            let (stalled, spent) = (own.get(stalls).copied().unwrap_or(0.0), own.get(cycles).copied().unwrap_or(0.0));
            (stalled >= 0.05 * run_stalls && spent > 0.0 && stalled / spent >= 0.3).then(|| (func, stalled, spent, own.clone()))
        })
        .collect();
    rows.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
    let how = if c.by_calls() { "read around its sampled calls (its own, its callees' apart)" } else { "booked from the 1 ms samples after it" };
    for (func, stalled, spent, own) in rows.into_iter().take(4) {
        let of_run = stalled / run_cycles;
        let mut evidence = vec![format!(
            "{} of its own {} cycles stalled waiting for load data ({}), {} of the run thread's cycles; {how}",
            fmt::compact(stalled),
            fmt::compact(spent),
            fmt::share(stalled, spent),
            fmt::share(stalled, run_cycles)
        )];
        if let (Some(occupancy), Some(allocs)) = (occupancy, allocs) {
            let (inflight, allocated) = (own.get(occupancy).copied().unwrap_or(0.0), own.get(allocs).copied().unwrap_or(0.0));
            if allocated > 0.0 && spent > 0.0 {
                evidence.push(format!("its L1 data cache misses stayed outstanding {:.0} cycles on average, {:.2} outstanding on average over its cycles", inflight / allocated, inflight / spent));
            }
        }
        evidence.push("the counters measure ree's own process running the Luau: where memory costs, not a client's frame time (the work clock's)".into());
        let f = &p.funcs[func];
        issues.push(Issue {
            severity: if of_run >= 0.10 {
                Severity::Serious
            } else if of_run >= 0.03 {
                Severity::Warning
            } else {
                Severity::Info
            },
            kind: "memory waits",
            title: format!("Waits on memory: {} ({} of its cycles stalled on loads)", f.label(), fmt::share(stalled, spent)),
            evidence,
            advice: "Its loads wait for data from beyond the L1: fewer cache lines touched, or the same lines touched again while they are near, would shorten the wait".into(),
            target: Target::Func(func as u32),
            weight: stalled,
            ..Default::default()
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One builtin whose calls declined for these reasons, and the reach its advised rewrite gets
    struct ReachCase {
        builtin: &'static str,
        reasons: &'static [&'static str],
        reach: EditReach,
    }

    #[test]
    fn only_the_allowlisted_rewrites_keep_a_site_bound() {
        let cases = [
            ReachCase { builtin: "string.byte", reasons: &["args"], reach: EditReach::SiteOnly },
            ReachCase { builtin: "string.sub", reasons: &["args"], reach: EditReach::SiteOnly },
            ReachCase { builtin: "string.sub", reasons: &["args", "gc_debt"], reach: EditReach::SiteOnly },
            // another reason asks for another edit
            ReachCase { builtin: "string.byte", reasons: &["args", "results"], reach: EditReach::WholeCode },
            ReachCase { builtin: "string.sub", reasons: &["args", "values"], reach: EditReach::WholeCode },
            ReachCase { builtin: "string.byte", reasons: &["args", "gc_debt"], reach: EditReach::WholeCode },
            ReachCase { builtin: "string.sub", reasons: &["args", "unsafe_env"], reach: EditReach::WholeCode },
            // without `args` there is no omitted argument to add
            ReachCase { builtin: "string.sub", reasons: &["gc_debt"], reach: EditReach::WholeCode },
            ReachCase { builtin: "string.byte", reasons: &[], reach: EditReach::WholeCode },
            // a builtin that is not listed takes the default
            ReachCase { builtin: "table.insert", reasons: &["args"], reach: EditReach::WholeCode },
            ReachCase { builtin: "future.builtin", reasons: &["args"], reach: EditReach::WholeCode },
        ];
        for case in cases {
            let reasons: BTreeSet<String> = case.reasons.iter().map(|reason| reason.to_string()).collect();
            assert_eq!(edit_reach(case.builtin, &reasons), case.reach, "{} {:?}", case.builtin, case.reasons);
        }
    }
}
