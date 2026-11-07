//! The viewer's state and what each key does. The terminal and `render` drive the same App: render applies its
//! options and key script, then draws once.

use std::{cell::RefCell, collections::BTreeMap, rc::Rc};

use crate::{
    compare::Compare,
    issues::{self, Issue, Target},
    keys::Key,
    metric::Metric,
    model::{Profile, Source},
    sites::{AllocSite, alloc_sites},
    theme::{Theme, ThemeKind},
    tree::Tree,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum View {
    Overview,
    Flame,
    Calls,
    Source,
    Alloc,
    Heap,
    Timeline,
    Caches,
    Issues,
    Diff,
    Compare,
}

impl View {
    pub const ALL: [View; 11] = [View::Overview, View::Flame, View::Calls, View::Source, View::Alloc, View::Heap, View::Timeline, View::Caches, View::Issues, View::Diff, View::Compare];

    /// The key that opens it
    pub fn key(self) -> char {
        match self {
            View::Overview => '1',
            View::Flame => '2',
            View::Calls => '3',
            View::Source => '4',
            View::Alloc => '5',
            View::Heap => '6',
            View::Timeline => '7',
            View::Caches => '8',
            View::Issues => '9',
            View::Diff => 'd',
            View::Compare => 'c',
        }
    }

    /// The tab's name (and `--view`'s)
    pub fn name(self) -> &'static str {
        match self {
            View::Overview => "overview",
            View::Flame => "flame",
            View::Calls => "calls",
            View::Source => "source",
            View::Alloc => "alloc",
            View::Heap => "heap",
            View::Timeline => "timeline",
            View::Caches => "caches",
            View::Issues => "issues",
            View::Diff => "diff",
            View::Compare => "compare",
        }
    }

    pub fn title(self) -> &'static str {
        match self {
            View::Overview => "Overview",
            View::Flame => "Flame",
            View::Calls => "Calls",
            View::Source => "Source",
            View::Alloc => "Allocations",
            View::Heap => "Heap",
            View::Timeline => "Timeline",
            View::Caches => "Caches",
            View::Issues => "Issues",
            View::Diff => "Diff",
            View::Compare => "Compare",
        }
    }

    pub fn parse(name: &str) -> Option<View> {
        let name = name.trim().to_ascii_lowercase();
        let name = match name.as_str() {
            "allocations" | "allocs" | "memory" => "alloc",
            "icicle" | "tree" => "flame",
            "callers" | "callees" => "calls",
            "lines" => "source",
            "frames" => "timeline",
            "cache" => "caches",
            "issue" => "issues",
            "variants" => "compare",
            other => other,
        };
        View::ALL.iter().copied().find(|view| view.name() == name)
    }
}

/// A list's selection and scroll
#[derive(Clone, Copy, Debug, Default)]
pub struct List {
    pub selected: usize,
    pub offset: usize,
}

impl List {
    /// Moves the selection by a key; true when the key was a movement
    pub fn key(&mut self, key: Key, len: usize, page: usize) -> bool {
        if len == 0 {
            self.selected = 0;
            return matches!(key, Key::Up | Key::Down | Key::PageUp | Key::PageDown | Key::Home | Key::End);
        }
        let last = len - 1;
        match key {
            Key::Up => self.selected = self.selected.saturating_sub(1),
            Key::Down => self.selected = (self.selected + 1).min(last),
            Key::PageUp => self.selected = self.selected.saturating_sub(page.max(1)),
            Key::PageDown => self.selected = (self.selected + page.max(1)).min(last),
            Key::Home => self.selected = 0,
            Key::End => self.selected = last,
            _ => return false,
        }
        true
    }

    /// Keeps the selection in the list and on the screen (`height` rows shown)
    pub fn fit(&mut self, len: usize, height: usize) {
        if len == 0 {
            self.selected = 0;
            self.offset = 0;
            return;
        }
        self.selected = self.selected.min(len - 1);
        let height = height.max(1);
        if self.selected < self.offset {
            self.offset = self.selected;
        } else if self.selected >= self.offset + height {
            self.offset = self.selected + 1 - height;
        }
        self.offset = self.offset.min(len.saturating_sub(height));
    }
}

/// The function (and line) the views follow: picked in one, shown in the next
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Focus {
    pub func: u32,
    pub line: Option<i64>,
}

#[derive(Clone, Debug, Default)]
pub struct FlameState {
    /// Zoomed-in nodes, innermost last (empty: the whole run)
    pub zoom: Vec<u32>,
    pub selected: u32,
    /// The first depth shown
    pub top: u16,
    /// The tree the selection belongs to: (profile, metric, scripts only)
    pub built: Option<(usize, Metric, bool)>,
    pub by_name: bool,
    /// The width the icicle was last drawn at (the keys walk the nodes that width shows)
    pub width: u16,
}

#[derive(Clone, Debug, Default)]
pub struct CallsState {
    pub center: Option<u32>,
    pub history: Vec<u32>,
    pub list: List,
    pub by_calls: bool,
}

#[derive(Clone, Debug, Default)]
pub struct SourceState {
    pub module: Option<String>,
    pub line: i64,
    pub offset: usize,
    /// Lines by heat instead of in file order
    pub hot: bool,
    pub list: List,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AllocSort {
    #[default]
    Bytes,
    Count,
    SteadyBytes,
    Size,
}

#[derive(Clone, Debug, Default)]
pub struct AllocState {
    pub list: List,
    pub sort: AllocSort,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum HeapPane {
    #[default]
    Holders,
    Types,
    Stacks,
}

#[derive(Clone, Debug, Default)]
pub struct HeapState {
    pub snapshot: Option<usize>,
    pub pane: HeapPane,
    pub list: List,
}

#[derive(Clone, Debug, Default)]
pub struct TimelineState {
    /// The selected row of the timeline
    pub frame: Option<usize>,
    /// A column picked with [ and ] instead of the metric's
    pub column: Option<String>,
    /// Rows shown (a zoomed range)
    pub range: Option<(usize, usize)>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CacheSort {
    #[default]
    Misses,
    Rate,
    Runs,
}

#[derive(Clone, Debug, Default)]
pub struct CachesState {
    pub list: List,
    pub sort: CacheSort,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DiffTab {
    #[default]
    Functions,
    Lines,
    Sites,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DiffSort {
    #[default]
    Change,
    Increase,
    Decrease,
    Name,
}

#[derive(Clone, Debug, Default)]
pub struct DiffState {
    pub tab: DiffTab,
    pub list: List,
    pub sort: DiffSort,
}

#[derive(Clone, Debug, Default)]
pub struct CompareState {
    pub list: List,
    /// The input size whose run the detail shows (an index into the sizes; None: the largest)
    pub size: Option<usize>,
    /// The comparison's metric shown (its key in compare.json), picked with m
    pub metric: Option<String>,
}

pub struct App {
    pub profiles: Vec<Profile>,
    /// The profile the single-profile views show (the after of a pair)
    pub shown: usize,
    pub compare: Option<Compare>,
    pub view: View,
    pub metric: Metric,
    pub scripts_only: bool,
    pub theme: Theme,
    pub help: bool,
    pub searching: bool,
    pub query: String,
    pub status: String,
    /// Text the terminal copies (OSC 52) after the next draw
    pub clipboard: Option<String>,
    pub focus: Option<Focus>,
    pub quit: bool,
    trees: RefCell<BTreeMap<(usize, Metric, bool), Rc<Tree>>>,
    sites: RefCell<BTreeMap<usize, Rc<Vec<AllocSite>>>>,
    issues: RefCell<BTreeMap<usize, Rc<(Vec<Issue>, Vec<String>)>>>,
    sources: RefCell<BTreeMap<(usize, String), Rc<Option<Source>>>>,
    pub overview: List,
    pub flame: FlameState,
    pub calls: CallsState,
    pub source: SourceState,
    pub alloc: AllocState,
    pub heap: HeapState,
    pub timeline: TimelineState,
    pub caches: CachesState,
    pub issues_list: List,
    pub diff: DiffState,
    pub compare_view: CompareState,
    /// The body's height at the last draw (page keys move by it)
    pub page: usize,
}

impl App {
    pub fn new(profiles: Vec<Profile>, compare: Option<Compare>, theme: ThemeKind) -> App {
        let shown = profiles.len().saturating_sub(1);
        let metric = profiles.get(shown).and_then(|p| Metric::available(p).first().copied()).unwrap_or(Metric::Work);
        let view = if profiles.is_empty() && compare.is_some() { View::Compare } else { View::Overview };
        App {
            profiles,
            shown,
            compare,
            view,
            metric,
            scripts_only: false,
            theme: Theme::new(theme),
            help: false,
            searching: false,
            query: String::new(),
            status: String::new(),
            clipboard: None,
            focus: None,
            quit: false,
            trees: RefCell::new(BTreeMap::new()),
            sites: RefCell::new(BTreeMap::new()),
            issues: RefCell::new(BTreeMap::new()),
            sources: RefCell::new(BTreeMap::new()),
            overview: List::default(),
            flame: FlameState::default(),
            calls: CallsState::default(),
            source: SourceState::default(),
            alloc: AllocState::default(),
            heap: HeapState::default(),
            timeline: TimelineState::default(),
            caches: CachesState::default(),
            issues_list: List::default(),
            diff: DiffState::default(),
            compare_view: CompareState::default(),
            page: 20,
        }
    }

    pub fn profile(&self) -> &Profile {
        &self.profiles[self.shown]
    }

    pub fn has_profile(&self) -> bool {
        !self.profiles.is_empty()
    }

    /// The call tree of the shown profile for the metric (work's when the metric has no stacks)
    pub fn tree(&self) -> Rc<Tree> {
        self.tree_of(self.shown, self.metric, self.scripts_only)
    }

    pub fn tree_of(&self, profile: usize, metric: Metric, scripts_only: bool) -> Rc<Tree> {
        let p = &self.profiles[profile];
        let metric = metric.stacks(p);
        self.trees.borrow_mut().entry((profile, metric, scripts_only)).or_insert_with(|| Rc::new(Tree::build(p, metric, scripts_only))).clone()
    }

    pub fn sites(&self) -> Rc<Vec<AllocSite>> {
        self.sites_of(self.shown)
    }

    pub fn sites_of(&self, profile: usize) -> Rc<Vec<AllocSite>> {
        self.sites.borrow_mut().entry(profile).or_insert_with(|| Rc::new(alloc_sites(&self.profiles[profile]))).clone()
    }

    /// A module's source text (read once)
    pub fn source_text(&self, module: &str) -> Rc<Option<Source>> {
        let key = (self.shown, module.to_string());
        if let Some(found) = self.sources.borrow().get(&key) {
            return found.clone();
        }
        let value = Rc::new(self.profile().source(module));
        self.sources.borrow_mut().insert(key, value.clone());
        value
    }

    /// Replaces the profiles (a comparison's run opened from Compare): the views start over
    pub fn open_profiles(&mut self, profiles: Vec<Profile>) {
        self.profiles = profiles;
        self.shown = self.profiles.len().saturating_sub(1);
        self.trees.borrow_mut().clear();
        self.sites.borrow_mut().clear();
        self.issues.borrow_mut().clear();
        self.sources.borrow_mut().clear();
        self.focus = None;
        self.overview = List::default();
        self.flame = FlameState::default();
        self.calls = CallsState::default();
        self.source = SourceState::default();
        self.alloc = AllocState::default();
        self.heap = HeapState::default();
        self.timeline = TimelineState::default();
        self.caches = CachesState::default();
        self.issues_list = List::default();
        self.diff = DiffState::default();
        if !self.metrics().contains(&self.metric) {
            self.metric = self.metrics().first().copied().unwrap_or(Metric::Work);
        }
    }

    /// The shown profile's issues, and what its kinds could not check
    pub fn issues(&self) -> Rc<(Vec<Issue>, Vec<String>)> {
        if !self.has_profile() {
            let found = self.compare.as_ref().map(|c| c.issues()).unwrap_or_default();
            return Rc::new((found, Vec::new()));
        }
        let shown = self.shown;
        if let Some(found) = self.issues.borrow().get(&shown) {
            return found.clone();
        }
        let p = &self.profiles[shown];
        let work = Metric::Work.has_stacks(p).then(|| self.tree_of(shown, Metric::Work, false));
        let sites = self.sites_of(shown);
        let (mut found, unchecked) = issues::detect(p, work.as_deref(), &sites);
        if let Some(compare) = &self.compare {
            found.extend(compare.issues());
            found.sort_by(|a, b| a.severity.cmp(&b.severity).then(b.weight.total_cmp(&a.weight)).then(a.title.cmp(&b.title)));
        }
        let value = Rc::new((found, unchecked));
        self.issues.borrow_mut().insert(shown, value.clone());
        value
    }

    /// The views this App can show (Diff with two profiles, Compare with a comparison)
    pub fn available(&self, view: View) -> bool {
        match view {
            View::Diff => self.profiles.len() >= 2,
            View::Compare => self.compare.is_some(),
            _ => self.has_profile(),
        }
    }

    pub fn metrics(&self) -> Vec<Metric> {
        if self.has_profile() { Metric::available(self.profile()) } else { vec![Metric::Work] }
    }

    pub fn set_view(&mut self, view: View) {
        if !self.available(view) {
            self.status = match view {
                View::Diff => "Diff needs two profiles: ree profile view <before> <after>".into(),
                View::Compare => "Compare needs a comparison: a folder with compare.json (ree profile try)".into(),
                _ => "no profile loaded".into(),
            };
            return;
        }
        self.view = view;
        self.status.clear();
    }

    pub fn set_metric(&mut self, metric: Metric) {
        self.metric = metric;
        self.flame.built = None;
    }

    /// Selects a function by name or a place by `file:line` (`--select`); false when nothing matches
    pub fn select(&mut self, query: &str) -> bool {
        if !self.has_profile() {
            return false;
        }
        let p = &self.profiles[self.shown];
        let query = query.trim();
        // `Module:line`, `file.luau:line`, `path/to/file.luau:line`
        if let Some((place, line)) = query.rsplit_once(':') {
            if let Ok(line) = line.trim().parse::<i64>() {
                let short = crate::model::short_module(place.rsplit(['/', '\\']).next().unwrap_or(place));
                let found = p
                    .funcs
                    .iter()
                    .enumerate()
                    .filter(|(_, f)| !f.module.is_empty() && (f.module == place || f.short.eq_ignore_ascii_case(short)) && f.line <= line && line <= f.last_line.max(f.line))
                    .max_by_key(|(_, f)| f.line)
                    .map(|(index, _)| index as u32);
                if let Some(func) = found {
                    self.focus = Some(crate::app::Focus { func, line: Some(line) });
                    self.source.line = line;
                    self.source.module = Some(p.funcs[func as usize].module.clone());
                    return true;
                }
            }
        }
        let lower = query.to_ascii_lowercase();
        let tree = self.tree();
        let ranked = tree.ranked();
        let exact = ranked.iter().find(|(func, _)| p.funcs[*func as usize].name.eq_ignore_ascii_case(query) || p.funcs[*func as usize].key.eq_ignore_ascii_case(query));
        let partial = ranked.iter().find(|(func, _)| p.funcs[*func as usize].label().to_ascii_lowercase().contains(&lower));
        if let Some(&(func, _)) = exact.or(partial) {
            let line = p.funcs[func as usize].line;
            self.focus = Some(Focus { func, line: Some(line) });
            self.source.module = Some(p.funcs[func as usize].module.clone());
            self.source.line = line;
            return true;
        }
        false
    }

    /// Follows an issue's link
    pub fn follow(&mut self, target: &Target) {
        match *target {
            Target::None => {}
            Target::Func(func) => {
                self.focus = Some(Focus { func, line: None });
                self.calls.center = Some(func);
                self.set_view(View::Calls);
            }
            Target::Line(func, line) => {
                self.focus = Some(Focus { func, line: Some(line) });
                self.source.module = Some(self.profile().funcs[func as usize].module.clone());
                self.source.line = line;
                self.source.hot = false;
                self.set_view(View::Source);
            }
            Target::Frame(row) => {
                self.timeline.frame = Some(row);
                self.set_view(View::Timeline);
            }
            Target::Site(func, line) => {
                self.focus = Some(Focus { func, line: Some(line) });
                let sites = self.sites();
                let order = crate::ui::alloc::order(self, &sites);
                if let Some(at) = order.iter().position(|&i| sites[i].func == func && sites[i].line == line) {
                    self.alloc.list.selected = at;
                }
                self.set_view(View::Alloc);
            }
            Target::Cache(index) => {
                let order = crate::ui::caches::order(self);
                if let Some(at) = order.iter().position(|&i| i == index) {
                    self.caches.list.selected = at;
                }
                self.set_view(View::Caches);
            }
            Target::Snapshot(index) => {
                self.heap.snapshot = Some(index);
                self.set_view(View::Heap);
            }
            Target::Variant(ref name) => {
                if let Some(compare) = &self.compare {
                    if let Some(at) = compare.variants.iter().position(|v| &v.name == name) {
                        self.compare_view.list.selected = at;
                    }
                }
                self.set_view(View::Compare);
            }
        }
    }

    /// Switches the shown profile of a pair, keeping the focus on the same function
    pub fn swap(&mut self) {
        if self.profiles.len() < 2 {
            self.status = "one profile: nothing to switch to".into();
            return;
        }
        let key = self.focus.map(|focus| self.profile().funcs[focus.func as usize].key.clone());
        self.shown = (self.shown + 1) % self.profiles.len();
        self.focus = key.and_then(|key| {
            let p = self.profile();
            p.funcs.iter().position(|f| f.key == key).map(|func| Focus { func: func as u32, line: None })
        });
        self.flame = FlameState::default();
        self.calls = CallsState::default();
        self.heap = HeapState::default();
        self.timeline = TimelineState::default();
        if !self.metrics().contains(&self.metric) {
            self.metric = self.metrics().first().copied().unwrap_or(Metric::Work);
        }
        self.status = format!("showing {}", self.side_name(self.shown));
    }

    /// `before` / `after` for a pair, else the profile's name
    pub fn side_name(&self, index: usize) -> String {
        if self.profiles.len() == 2 {
            if let Some(label) = self.profiles.get(index).and_then(|p| p.label.clone()) {
                return label;
            }
            if index == 0 { "before".into() } else { "after".into() }
        } else {
            self.profiles.get(index).map(Profile::name).unwrap_or_default()
        }
    }

    /// One key
    pub fn key(&mut self, key: Key) {
        if key == Key::Interrupt {
            self.quit = true;
            return;
        }
        if self.searching {
            match key {
                Key::Enter => {
                    self.searching = false;
                    self.find(true, true);
                }
                Key::Esc => {
                    self.searching = false;
                    self.query.clear();
                }
                Key::Backspace => {
                    self.query.pop();
                }
                Key::Char(c) => self.query.push(c),
                _ => {}
            }
            return;
        }
        if self.help {
            if matches!(key, Key::Char('?') | Key::Esc | Key::Char('q') | Key::Backspace | Key::Enter) {
                self.help = false;
            }
            return;
        }
        let key = key.navigation();
        match key {
            Key::Char('q') => self.quit = true,
            Key::Char('?') => self.help = true,
            Key::Char(c @ '1'..='9') => self.set_view(View::ALL[(c as u8 - b'1') as usize]),
            Key::Char('d') => self.set_view(View::Diff),
            Key::Char('c') | Key::Char('0') => self.set_view(View::Compare),
            Key::Char('m') | Key::Char('M') if self.view == View::Compare => crate::ui::compare::cycle_metric(self, key == Key::Char('m')),
            Key::Char('m') | Key::Char('M') => {
                let metrics = self.metrics();
                if let Some(at) = metrics.iter().position(|m| *m == self.metric) {
                    let next = if key == Key::Char('m') { (at + 1) % metrics.len() } else { (at + metrics.len() - 1) % metrics.len() };
                    self.set_metric(metrics[next]);
                } else if let Some(first) = metrics.first() {
                    self.set_metric(*first);
                }
                if self.has_profile() {
                    self.status = format!("metric: {}", self.metric.label(self.profile()));
                }
            }
            Key::Char('f') => {
                self.scripts_only = !self.scripts_only;
                self.flame.built = None;
                self.status = if self.scripts_only { "scripts only: C functions and the engine's own Luau fold into their callers".into() } else { "every function".into() };
            }
            Key::Char('t') => {
                let kind = if self.theme.kind == ThemeKind::Dark { ThemeKind::Light } else { ThemeKind::Dark };
                self.theme = Theme::new(kind);
            }
            Key::Char('x') => self.swap(),
            Key::Char('/') => {
                self.searching = true;
                self.query.clear();
            }
            Key::Char('n') => self.find(true, false),
            Key::Char('N') => self.find(false, false),
            Key::Char('y') => {
                if let Some(text) = crate::ui::copy_text(self) {
                    self.status = format!("copied: {text}");
                    self.clipboard = Some(text);
                }
            }
            _ => crate::ui::key(self, key),
        }
    }

    /// Moves to the next (or previous) match of the search in the current view
    pub fn find(&mut self, forward: bool, from_start: bool) {
        if self.query.is_empty() {
            return;
        }
        let found = crate::ui::find(self, forward, from_start);
        self.status = if found { format!("/{}", self.query) } else { format!("/{}: no match in {}", self.query, self.view.title()) };
    }
}
