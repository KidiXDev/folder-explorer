use crate::chart;
use crate::export;
use crate::file_ops;
use crate::format::{Unit, format_elapsed, format_size, plural};
use crate::hash::hash_file;
use crate::icons::IconStore;
use crate::item::{ATTR_HIDDEN, Column, Item};
use crate::list::{ListModel, ListState, View};
use crate::platform::{self, ClipboardFiles, DiskUsage, Drive};
use crate::query::{self, DateField, DateRule, Query, SummaryEntry, SummaryKind};
use crate::scan::{Cache, DirNode, ErrorLog, Level, Live, ScanContext, ScanError, SubDir, read_level, scan_tree};
use crate::settings::Settings;
use crate::sidebar::Sidebar;
use crate::ui::{AppWindow, ColumnChoice, ColumnInfo, Crumb, DriveCard};
use rayon::prelude::*;
use slint::{
    CloseRequestResponse, ComponentHandle, Image, LogicalSize, ModelRc, PhysicalSize, SharedString, Timer, TimerMode, VecModel, Weak,
};
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io;
use std::os::windows::fs::MetadataExt;
use std::path::{Component, Path, PathBuf};
use std::rc::{self, Rc};
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

const PUMP_INTERVAL: Duration = Duration::from_millis(16);
const PROGRESS_INTERVAL: Duration = Duration::from_millis(100);
const SEARCH_DELAY: Duration = Duration::from_millis(180);
const NEARLY_FULL: f32 = 0.9;
const DEFAULT_WINDOW_SIZE: (f32, f32) = (1180.0, 720.0);
const DRIVE_ICON_SIZE: u32 = 48;
const BYTES_PER_MEGABYTE: f64 = 1_048_576.0;
const SUMMARY_KINDS: [SummaryKind; 5] = [
    SummaryKind::Folders,
    SummaryKind::Extension,
    SummaryKind::Age(DateField::Modified),
    SummaryKind::Age(DateField::Created),
    SummaryKind::Age(DateField::Accessed),
];
const DATE_FIELDS: [DateField; 3] = [DateField::Modified, DateField::Created, DateField::Accessed];
const DATE_RULES: [DateRule; 3] = [DateRule::After, DateRule::Before, DateRule::On];

type Shared = Rc<RefCell<App>>;

#[derive(Clone, PartialEq)]
enum Location {
    Computer,
    Folder(PathBuf),
}

enum Message {
    Drives(Vec<Drive>),
    Pins(Vec<(PathBuf, String)>),
    TreeChildren { parent: PathBuf, folders: Vec<(OsString, u32)> },
    Listed { scan: u64, items: Vec<Item>, lives: Arc<Vec<Live>> },
    Unreadable { scan: u64, reason: String },
    Disk { scan: u64, title: String, usage: Option<DiskUsage> },
    Sized { scan: u64, index: usize, node: Arc<DirNode> },
    Finished {
        scan: u64,
        generation: u64,
        path: PathBuf,
        node: Arc<DirNode>,
        errors: Vec<ScanError>,
        seconds: f64,
        from_cache: bool,
    },
    Found { search: u64, items: Vec<Item>, seconds: f64 },
    Summary { id: u64, entries: Vec<SummaryEntry> },
    Hashed { run: u64, index: usize, hashes: Option<[String; 4]> },
    Recycled { paths: Vec<PathBuf>, succeeded: bool },
    Pasted { paths: Vec<PathBuf>, target: PathBuf, is_cut: bool, succeeded: bool, sequence: u32 },
    RecycleBin(Option<u64>),
}

struct App {
    ui: Weak<AppWindow>,
    me: rc::Weak<RefCell<App>>,
    list: Rc<ListModel>,
    icons: Rc<IconStore>,
    drive_icons_missing: Cell<bool>,
    sidebar: Sidebar,
    settings: Settings,
    cache: Cache,
    drives: Vec<Drive>,
    location: Location,
    back: Vec<Location>,
    forward: Vec<Location>,
    scan_id: u64,
    scanning: bool,
    cancel: Arc<AtomicBool>,
    lives: Arc<Vec<Live>>,
    scan_started: Instant,
    last_progress: Instant,
    activity: String,
    errors: Vec<ScanError>,
    search_id: u64,
    active_query: Option<Query>,
    showing_results: bool,
    search_timer: Timer,
    summary_id: u64,
    summary_kind: usize,
    summary: Vec<SummaryEntry>,
    hash_run: u64,
    hash_cancel: Arc<AtomicBool>,
    hashes_left: usize,
    cut_paths: HashSet<PathBuf>,
    cut_sequence: u32,
    pending_select: Option<String>,
    pending_rename: bool,
    renaming: Option<PathBuf>,
    sender: Sender<Message>,
    receiver: Receiver<Message>,
    workers: Arc<()>,
    spawned: u64,
    pump: Timer,
}

fn scan_folder(
    scan: u64,
    generation: u64,
    path: PathBuf,
    known: HashMap<PathBuf, Arc<DirNode>>,
    cancel: Arc<AtomicBool>,
    sender: Sender<Message>,
) {
    let started = Instant::now();
    let listing = SubDir::at(&path).and_then(|root| {
        let errors = ErrorLog::default();
        read_level(&root, &errors).map(|level| (level, errors))
    });
    let (Level { mut node, subdirs }, errors) = match listing {
        Ok(listing) => listing,
        Err(error) => {
            let _ = sender.send(Message::Unreadable { scan, reason: describe_error(&path, &error) });
            return;
        }
    };

    let parent: Arc<Path> = path.as_path().into();
    let lives: Arc<Vec<Live>> = Arc::new(subdirs.iter().map(|_| Live::default()).collect());
    let mut items: Vec<Item> = subdirs.iter().map(|dir| Item::pending_folder(dir, &parent)).collect();
    items.extend(node.files().map(|(name, entry)| Item::file(name, entry, &parent)));
    let _ = sender.send(Message::Listed { scan, items, lives: lives.clone() });

    let root = root_of(&path);
    let title = root.to_string_lossy().trim_end_matches('\\').to_string();
    let _ = sender.send(Message::Disk { scan, title, usage: platform::disk_usage(&root) });

    let from_cache = subdirs.iter().all(|dir| known.contains_key(&dir.path));
    let context = ScanContext { cancel, known, errors };
    let children = subdirs
        .par_iter()
        .enumerate()
        .map(|(index, dir)| {
            let child = scan_tree(dir, &context, &lives[index]);
            let _ = sender.send(Message::Sized { scan, index, node: child.clone() });
            child
        })
        .collect();
    node.absorb(children);
    let errors = context.errors.into_inner();
    let seconds = started.elapsed().as_secs_f64();
    let _ = sender.send(Message::Finished { scan, generation, path, node: Arc::new(node), errors, seconds, from_cache });
}

fn describe_error(path: &Path, error: &io::Error) -> String {
    match error.kind() {
        io::ErrorKind::NotFound => format!("{} does not exist.", path.display()),
        io::ErrorKind::PermissionDenied => format!("You don't have permission to open {}.", path.display()),
        _ => format!("Can't open {}: {error}", path.display()),
    }
}

fn root_of(path: &Path) -> PathBuf {
    path.ancestors().last().unwrap_or(path).to_path_buf()
}

fn normalize(text: &str) -> PathBuf {
    let trimmed = text.trim().trim_matches('"');
    let rooted = if trimmed.len() == 2 && trimmed.ends_with(':') { format!("{trimmed}\\") } else { trimmed.to_string() };
    let absolute = std::path::absolute(&rooted).unwrap_or_else(|_| PathBuf::from(rooted));
    absolute.components().collect()
}

fn resolve_target(location: &Location, text: &str) -> PathBuf {
    let exact = match location {
        Location::Folder(path) => path.ancestors().find(|ancestor| ancestor.to_string_lossy() == text),
        Location::Computer => None,
    };
    exact.map_or_else(|| normalize(text), Path::to_path_buf)
}

fn crumbs_for(location: &Location) -> Vec<Crumb> {
    let mut crumbs = vec![Crumb { name: "This PC".into(), target: SharedString::new() }];
    if let Location::Folder(path) = location {
        let mut ancestors: Vec<&Path> = path.ancestors().collect();
        ancestors.reverse();
        crumbs.extend(ancestors.iter().map(|ancestor| {
            let name = match ancestor.components().next_back() {
                Some(Component::Normal(name)) => name.to_string_lossy().to_string(),
                _ => ancestor.to_string_lossy().trim_end_matches('\\').to_string(),
            };
            Crumb { name: name.into(), target: ancestor.to_string_lossy().as_ref().into() }
        }));
    }
    crumbs
}

fn list_subfolders(parent: &Path, show_hidden: bool) -> Vec<(OsString, u32)> {
    let Ok(entries) = fs::read_dir(parent) else { return Vec::new() };
    entries
        .flatten()
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .filter_map(|entry| Some((entry.file_name(), entry.metadata().ok()?.file_attributes())))
        .filter(|(_, attrs)| show_hidden || attrs & ATTR_HIDDEN == 0)
        .collect()
}

fn disk_label(title: &str, usage: DiskUsage, unit: Unit) -> String {
    let percent = (usage.used_ratio() * 100.0).round();
    format!("{title} {percent}% used · {} free", format_size(usage.free, unit))
}

fn drive_card(drive: &Drive, icon: Image, unit: Unit) -> DriveCard {
    let usage = drive.usage;
    let ratio = usage.map_or(0.0, DiskUsage::used_ratio);
    let detail = match usage {
        Some(usage) => format!("{} free of {}", format_size(usage.free, unit), format_size(usage.total, unit)),
        None => "Not available".into(),
    };
    DriveCard {
        title: drive.title.as_str().into(),
        detail: detail.into(),
        path: drive.root.to_string_lossy().as_ref().into(),
        icon,
        ratio,
        nearly_full: ratio > NEARLY_FULL,
    }
}

impl App {
    fn new(ui: &AppWindow, me: rc::Weak<RefCell<App>>, settings: Settings, icons: Rc<IconStore>) -> Self {
        let state = ListState::new(
            settings.columns.clone(),
            settings.unit,
            settings.sort,
            settings.descending,
            settings.show_hidden,
        );
        let (sender, receiver) = mpsc::channel();
        Self {
            ui: ui.as_weak(),
            me,
            list: Rc::new(ListModel::new(state, icons.clone())),
            sidebar: Sidebar::new(icons.clone(), settings.tree),
            icons,
            drive_icons_missing: Cell::new(false),
            settings,
            cache: Cache::default(),
            drives: Vec::new(),
            location: Location::Computer,
            back: Vec::new(),
            forward: Vec::new(),
            scan_id: 0,
            scanning: false,
            cancel: Arc::default(),
            lives: Arc::default(),
            scan_started: Instant::now(),
            last_progress: Instant::now(),
            activity: String::new(),
            errors: Vec::new(),
            search_id: 0,
            active_query: None,
            showing_results: false,
            search_timer: Timer::default(),
            summary_id: 0,
            summary_kind: 0,
            summary: Vec::new(),
            hash_run: 0,
            hash_cancel: Arc::default(),
            hashes_left: 0,
            cut_paths: HashSet::new(),
            cut_sequence: 0,
            pending_select: None,
            pending_rename: false,
            renaming: None,
            sender,
            receiver,
            workers: Arc::new(()),
            spawned: 0,
            pump: Timer::default(),
        }
    }

    fn ui(&self) -> AppWindow {
        self.ui.upgrade().expect("the window outlives the app state")
    }

    fn spawn(&mut self, work: impl FnOnce(Sender<Message>) + Send + 'static) {
        let (sender, worker) = (self.sender.clone(), self.workers.clone());
        thread::spawn(move || {
            let _worker = worker;
            work(sender);
        });
        self.spawned += 1;
        if !self.pump.running() {
            let me = self.me.clone();
            self.pump.start(TimerMode::Repeated, PUMP_INTERVAL, move || {
                if let Some(shared) = me.upgrade()
                    && let Ok(mut app) = shared.try_borrow_mut()
                {
                    app.drain();
                }
            });
        }
    }

    fn drain(&mut self) {
        let spawned = self.spawned;
        let idle = Arc::strong_count(&self.workers) == 1;
        while let Ok(message) = self.receiver.try_recv() {
            self.handle(message);
        }
        if self.scanning && self.last_progress.elapsed() >= PROGRESS_INTERVAL {
            self.last_progress = Instant::now();
            self.list.apply_live(&self.lives);
            self.show_scan_progress();
        }
        if idle && spawned == self.spawned {
            self.pump.stop();
        }
    }

    fn handle(&mut self, message: Message) {
        match message {
            Message::Drives(drives) => self.on_drives(drives),
            Message::Pins(pins) => self.on_pins(pins),
            Message::TreeChildren { parent, folders } => {
                let next = self.sidebar.set_children(&parent, folders);
                self.load_tree_children(next);
                self.scroll_sidebar();
            }
            Message::Listed { scan, items, lives } if scan == self.scan_id => self.on_listed(items, lives),
            Message::Unreadable { scan, reason } if scan == self.scan_id => self.on_unreadable(reason),
            Message::Disk { scan, title, usage } if scan == self.scan_id => self.on_disk(&title, usage),
            Message::Sized { scan, index, node } if scan == self.scan_id && !self.showing_results => {
                self.list.update_item(index, |item| item.apply_totals(&node));
            }
            Message::Finished { scan, generation, path, node, errors, seconds, from_cache } => {
                self.cache.store_scan(generation, &path, &node);
                if scan == self.scan_id {
                    self.on_finished(&node, errors, seconds, from_cache);
                }
            }
            Message::Found { search, items, seconds } if search == self.search_id => self.on_found(items, seconds),
            Message::Summary { id, entries } if id == self.summary_id => {
                self.summary = entries;
                self.show_summary();
            }
            Message::Hashed { run, index, hashes } if run == self.hash_run => self.on_hashed(index, hashes),
            Message::Recycled { paths, succeeded } => self.on_recycled(&paths, succeeded),
            Message::Pasted { paths, target, is_cut, succeeded, sequence } => {
                self.on_pasted(&paths, &target, is_cut, succeeded, sequence);
            }
            Message::RecycleBin(size) => self.show_recycle_bin(size),
            _ => {}
        }
    }

    // Navigation

    fn go(&mut self, location: Location) {
        if location == self.location && !self.showing_results {
            return;
        }
        let previous = std::mem::replace(&mut self.location, location);
        if previous != self.location {
            self.back.push(previous);
            self.forward.clear();
        }
        self.show_location();
        self.ui().invoke_focus_list();
    }

    fn go_back(&mut self) {
        if self.showing_results || self.active_query.is_some() {
            self.show_location();
        } else if let Some(previous) = self.back.pop() {
            self.forward.push(std::mem::replace(&mut self.location, previous));
            self.show_location();
            self.ui().invoke_focus_list();
        }
    }

    fn go_forward(&mut self) {
        if let Some(next) = self.forward.pop() {
            self.back.push(std::mem::replace(&mut self.location, next));
            self.show_location();
            self.ui().invoke_focus_list();
        }
    }

    fn go_up(&mut self) {
        if let Location::Folder(path) = &self.location {
            let parent = path.parent().map_or(Location::Computer, |parent| Location::Folder(parent.to_path_buf()));
            self.go(parent);
        }
    }

    fn open_target(&mut self, target: &str) {
        let location =
            if target.is_empty() { Location::Computer } else { Location::Folder(resolve_target(&self.location, target)) };
        self.go(location);
    }

    fn enter_typed_path(&mut self, text: &str) {
        let text = text.trim();
        if text.eq_ignore_ascii_case("this pc") {
            self.go(Location::Computer);
        } else if !text.is_empty() {
            self.go(Location::Folder(resolve_target(&self.location, text)));
        }
    }

    fn show_location(&mut self) {
        self.showing_results = false;
        self.summary.clear();
        self.icons.cancel_queued();
        self.list.set_items(Vec::new());
        if let Location::Folder(path) = &self.location {
            self.show_view(self.settings.view_of(path));
        }
        let ui = self.ui();
        ui.set_search_text(SharedString::new());
        ui.invoke_scroll_to_top();
        self.enter_location();
    }

    fn enter_location(&mut self) {
        self.stop_work();
        self.active_query = None;
        self.errors.clear();
        let ui = self.ui();
        ui.set_in_search_results(self.showing_results);
        ui.set_can_go_back(!self.back.is_empty());
        ui.set_can_go_forward(!self.forward.is_empty());
        ui.set_can_go_up(self.location != Location::Computer);
        ui.set_crumbs(ModelRc::new(VecModel::from(crumbs_for(&self.location))));
        ui.set_status_activity(SharedString::new());
        self.sync_columns();
        self.show_errors();
        self.show_summary();

        let location = self.location.clone();
        let path = match &location {
            Location::Computer => None,
            Location::Folder(path) => Some(path.as_path()),
        };
        let next = self.sidebar.reveal(path);
        self.load_tree_children(next);
        self.scroll_sidebar();
        ui.set_current_path(path.map_or("This PC".into(), |path| path.to_string_lossy().as_ref().into()));
        ui.set_showing_drives(path.is_none());
        ui.set_can_search(path.is_some());
        ui.set_can_edit_folder(path.is_some() && !self.showing_results);
        self.show_selection();

        match location {
            Location::Computer => {
                ui.set_disk_text(SharedString::new());
                ui.set_status_items(plural(self.drives.len() as u64, "drive").into());
                self.load_drives();
            }
            Location::Folder(path) => {
                ui.set_list_message(SharedString::new());
                self.start_scan(path);
            }
        }
    }

    fn reload(&mut self) {
        let query = self.active_query.clone();
        self.showing_results &= query.is_some();
        self.enter_location();
        if let Some(query) = query {
            self.run_query(query);
        }
    }

    fn refresh(&mut self) {
        if let Location::Folder(path) = self.location.clone() {
            self.forget(&path);
        }
        self.load_pins();
        self.reload();
    }

    fn forget(&mut self, path: &Path) {
        let retired = self.cache.invalidate(path);
        thread::spawn(move || drop(retired));
    }

    // Scanning

    fn start_scan(&mut self, path: PathBuf) {
        self.scan_id += 1;
        self.scanning = true;
        self.scan_started = Instant::now();
        self.cancel = Arc::default();
        self.lives = Arc::default();
        let (known, generation) = (self.cache.known_below(&path), self.cache.generation());
        let (scan, cancel) = (self.scan_id, self.cancel.clone());
        self.spawn(move |sender| scan_folder(scan, generation, path, known, cancel, sender));
        self.sync_busy();
    }

    fn stop_work(&mut self) {
        self.cancel_scan();
        self.search_id += 1;
        self.summary_id += 1;
        self.search_timer.stop();
        self.scanning = false;
        self.scan_id += 1;
        self.sync_busy();
    }

    fn cancel_scan(&mut self) {
        self.cancel.store(true, Relaxed);
        self.cancel_hashing();
        self.sync_busy();
    }

    fn sync_busy(&self) {
        self.ui().set_busy(self.scanning || self.hashes_left > 0);
    }

    fn on_listed(&mut self, items: Vec<Item>, lives: Arc<Vec<Live>>) {
        if self.showing_results {
            return;
        }
        self.list.set_items(items);
        self.lives = lives;
        self.ui().set_list_message("This folder is empty.".into());
        self.restore_cut_marks();
        self.show_selection();
        self.start_hashing();
        self.apply_pending_selection();
    }

    fn apply_pending_selection(&mut self) {
        let Some(name) = self.pending_select.take() else { return };
        let rename = std::mem::take(&mut self.pending_rename);
        if let Some(row) = self.list.change_selection(|state| state.select_name(&name)) {
            self.ui().invoke_reveal_row(row as i32);
            self.show_selection();
            if rename {
                self.begin_rename();
            }
        }
    }

    fn on_unreadable(&mut self, reason: String) {
        self.scanning = false;
        self.active_query = None;
        let ui = self.ui();
        ui.set_list_message(reason.into());
        ui.set_status_activity(SharedString::new());
        self.sync_busy();
    }

    fn on_disk(&self, title: &str, usage: Option<DiskUsage>) {
        let ui = self.ui();
        match usage {
            Some(usage) => {
                ui.set_disk_text(disk_label(title, usage, self.settings.unit).into());
                ui.set_disk_ratio(usage.used_ratio());
                ui.set_disk_nearly_full(usage.used_ratio() > NEARLY_FULL);
            }
            None => ui.set_disk_text(SharedString::new()),
        }
    }

    fn on_finished(&mut self, node: &DirNode, errors: Vec<ScanError>, seconds: f64, from_cache: bool) {
        self.scanning = false;
        self.errors = errors;
        self.lives = Arc::default();
        self.list.update(ListState::rebuild);
        self.show_errors();
        self.show_selection();
        let files = plural(node.file_count, "file");
        let elapsed = format_elapsed(seconds);
        self.activity = match (node.complete, from_cache) {
            (false, _) => format!("Stopped after {files} in {elapsed}"),
            (true, true) => files,
            (true, false) => format!("Scanned {files} in {elapsed}"),
        };
        self.ui().set_status_activity(self.activity.as_str().into());
        self.sync_busy();
        if node.complete {
            self.refresh_summary();
            self.execute_query();
        } else {
            self.active_query = None;
        }
    }

    fn show_scan_progress(&self) {
        let (files, size) = self.lives.iter().fold((0, 0), |(files, size), live| {
            (files + live.files.load(Relaxed), size + live.size.load(Relaxed))
        });
        let elapsed = format_elapsed(self.scan_started.elapsed().as_secs_f64());
        let waiting = if self.active_query.is_some() { " · search runs when this finishes" } else { "" };
        let text = format!(
            "Scanning {} · {} · {elapsed}{waiting}",
            plural(files, "file"),
            format_size(size, self.settings.unit)
        );
        self.ui().set_status_activity(text.into());
        self.show_totals();
    }

    // Search

    fn read_query(&self) -> Query {
        let ui = self.ui();
        let mut query = Query::new(&ui.get_search_text());
        if ui.get_size_enabled()
            && let Ok(megabytes) = ui.get_size_text().trim().parse::<f64>()
        {
            let bytes = (megabytes * BYTES_PER_MEGABYTE) as u64;
            query.size = Some(if ui.get_size_rule() == 0 { bytes + 1..u64::MAX } else { 0..bytes });
        }
        if ui.get_date_enabled()
            && let Some(day) = platform::local_day_start(&ui.get_date_text())
        {
            let field = DATE_FIELDS[ui.get_date_field() as usize];
            let rule = DATE_RULES[ui.get_date_rule() as usize];
            query.date = Some((field, query::date_range(day, rule)));
        }
        query
    }

    fn schedule_search(&mut self) {
        let me = self.me.clone();
        self.search_timer.start(TimerMode::SingleShot, SEARCH_DELAY, move || {
            if let Some(shared) = me.upgrade()
                && let Ok(mut app) = shared.try_borrow_mut()
            {
                app.apply_search();
            }
        });
    }

    fn apply_search(&mut self) {
        let query = self.read_query();
        if !query.is_empty() {
            self.run_query(query);
        } else if self.showing_results {
            self.show_location();
        } else {
            self.active_query = None;
            self.search_id += 1;
        }
    }

    fn run_query(&mut self, query: Query) {
        self.active_query = Some(query);
        self.search_id += 1;
        self.execute_query();
    }

    fn execute_query(&mut self) {
        let (Some(query), Location::Folder(path)) = (self.active_query.clone(), self.location.clone()) else { return };
        if self.scanning {
            return;
        }
        let Some(node) = self.cache.get(&path) else {
            self.start_scan(path);
            return;
        };
        let search = self.search_id;
        self.spawn(move |sender| {
            let started = Instant::now();
            let items = query::search(&node, &path, &query);
            let seconds = started.elapsed().as_secs_f64();
            let _ = sender.send(Message::Found { search, items, seconds });
        });
    }

    fn on_found(&mut self, items: Vec<Item>, seconds: f64) {
        self.showing_results = true;
        let found = plural(items.len() as u64, "result");
        self.list.set_items(items);
        self.sync_columns();
        let ui = self.ui();
        ui.set_in_search_results(true);
        ui.set_can_edit_folder(false);
        ui.set_list_message("Nothing matches this search.".into());
        self.activity = format!("{found} in {:.0} ms", seconds * 1000.0);
        ui.set_status_activity(self.activity.as_str().into());
        ui.invoke_scroll_to_top();
        self.restore_cut_marks();
        self.show_selection();
        self.start_hashing();
        self.apply_pending_selection();
    }

    // Summary pane

    fn refresh_summary(&mut self) {
        self.summary_id += 1;
        if !self.ui().get_summary_open() {
            return;
        }
        let node = match &self.location {
            Location::Folder(path) => self.cache.get(path),
            Location::Computer => None,
        };
        let Some(node) = node else {
            self.summary.clear();
            self.show_summary();
            return;
        };
        let (id, kind) = (self.summary_id, SUMMARY_KINDS[self.summary_kind]);
        self.spawn(move |sender| {
            let _ = sender.send(Message::Summary { id, entries: query::summarize(&node, kind) });
        });
    }

    fn show_summary(&self) {
        let unit = self.settings.unit;
        let (rows, total) = chart::build_rows(&self.summary, unit);
        let treemap = chart::build_treemap(&self.summary, unit);
        let ui = self.ui();
        let donut = ui.get_summary_viz_mode() == 0;
        ui.set_summary_hovered_index(-1);
        ui.set_summary_total_size(format_size(total, unit).into());
        ui.set_summary_chart_image(chart::render_wheel(&self.summary, donut, None));
        ui.set_summary_treemap(ModelRc::new(VecModel::from(treemap)));
        ui.set_summary_rows(ModelRc::new(VecModel::from(rows)));
        ui.set_summary_kind(self.summary_kind as i32);
        let hint = if self.location == Location::Computer {
            "Open a folder to see what fills it."
        } else {
            "Available when the scan finishes."
        };
        ui.set_summary_hint(hint.into());
    }

    fn refresh_summary_wheel(&self, hovered: Option<usize>) {
        let ui = self.ui();
        let mode = ui.get_summary_viz_mode();
        if mode <= 1 {
            ui.set_summary_chart_image(chart::render_wheel(&self.summary, mode == 0, hovered));
        }
    }

    fn hover_summary_row(&self, index: i32) {
        let ui = self.ui();
        let clamped = if index >= 0 && (index as usize) < self.summary.len() { index } else { -1 };
        if ui.get_summary_hovered_index() != clamped {
            ui.set_summary_hovered_index(clamped);
            self.refresh_summary_wheel((clamped >= 0).then_some(clamped as usize));
        }
    }

    fn hover_summary_wheel(&self, nx: f32, ny: f32) {
        let donut = self.ui().get_summary_viz_mode() == 0;
        let hit = chart::hit_test_wheel(&self.summary, donut, nx, ny).map_or(-1, |idx| idx as i32);
        self.hover_summary_row(hit);
    }

    fn choose_summary(&mut self, kind: usize) {
        self.summary_kind = kind.min(SUMMARY_KINDS.len() - 1);
        self.summary.clear();
        self.show_summary();
        self.refresh_summary();
    }

    fn activate_summary(&mut self, row: usize) {
        let Some(entry) = self.summary.get(row).cloned() else { return };
        if let (Some(folder), Location::Folder(parent)) = (entry.folder, &self.location) {
            self.go(Location::Folder(parent.join(folder)));
        } else if !entry.filter.is_empty() {
            self.ui().set_search_text(SharedString::new());
            self.run_query(entry.filter);
        }
    }

    // Checksums

    fn cancel_hashing(&mut self) {
        self.hash_cancel.store(true, Relaxed);
        self.hash_run += 1;
        if std::mem::take(&mut self.hashes_left) > 0 {
            self.on_hashing_ended();
        }
    }

    fn on_hashing_ended(&self) {
        if self.list.state().sort.hash_slot().is_some() {
            self.list.update(ListState::rebuild);
        }
        if !self.scanning {
            self.ui().set_status_activity(self.activity.as_str().into());
        }
    }

    fn start_hashing(&mut self) {
        self.cancel_hashing();
        self.hash_cancel = Arc::default();
        let mut wanted = [false; 4];
        let targets: Vec<(usize, PathBuf)> = {
            let state = self.list.state();
            for slot in state.columns.iter().filter_map(|column| column.hash_slot()) {
                wanted[slot] = true;
            }
            let missing = |item: &Item| {
                wanted.contains(&true)
                    && !item.is_dir
                    && item.hashes.as_ref().is_none_or(|hashes| (0..4).any(|slot| wanted[slot] && hashes[slot].is_empty()))
            };
            state.items.iter().enumerate().filter(|(_, item)| missing(item)).map(|(index, item)| (index, item.path())).collect()
        };
        self.hashes_left = targets.len();
        if !targets.is_empty() {
            let (run, cancel) = (self.hash_run, self.hash_cancel.clone());
            self.spawn(move |sender| {
                targets.par_iter().for_each(|(index, path)| {
                    let hashes = hash_file(path, wanted, &cancel).ok();
                    let _ = sender.send(Message::Hashed { run, index: *index, hashes });
                });
            });
        }
        self.sync_busy();
    }

    fn on_hashed(&mut self, index: usize, hashes: Option<[String; 4]>) {
        self.hashes_left = self.hashes_left.saturating_sub(1);
        self.list.update_item(index, |item| {
            let merged = item.hashes.get_or_insert_with(Default::default);
            match hashes {
                Some(hashes) => {
                    for (slot, hash) in hashes.into_iter().enumerate().filter(|(_, hash)| !hash.is_empty()) {
                        merged[slot] = hash;
                    }
                }
                None => merged.iter_mut().filter(|hash| hash.is_empty()).for_each(|hash| *hash = "Unreadable".into()),
            }
        });
        if self.hashes_left == 0 {
            self.on_hashing_ended();
        } else if !self.scanning {
            let activity = format!("Calculating checksums · {} left", plural(self.hashes_left as u64, "file"));
            self.ui().set_status_activity(activity.into());
        }
        self.sync_busy();
    }

    // Columns and display

    fn shown_columns(&self) -> Vec<Column> {
        let mut columns = self.settings.columns.clone();
        if self.showing_results && !columns.contains(&Column::Location) {
            columns.insert(2, Column::Location);
        }
        columns
    }

    fn sync_columns(&self) {
        let columns = self.shown_columns();
        let (sort, descending) = {
            let state = self.list.state();
            (state.sort, state.descending)
        };
        let info: Vec<ColumnInfo> = columns
            .iter()
            .map(|column| ColumnInfo {
                title: column.title().into(),
                width: self.settings.width_of(*column),
                right_aligned: column.right_aligned(),
                sort_direction: match (*column == sort, descending) {
                    (false, _) => 0,
                    (true, false) => 1,
                    (true, true) => -1,
                },
            })
            .collect();
        let ui = self.ui();
        ui.set_columns_width(info.iter().map(|column| column.width).sum());
        ui.set_columns(ModelRc::new(VecModel::from(info)));
        let choices: Vec<ColumnChoice> = Column::ALL
            .iter()
            .filter(|column| !column.is_fixed())
            .map(|column| ColumnChoice {
                title: column.title().into(),
                shown: self.settings.columns.contains(column),
            })
            .collect();
        ui.set_column_choices(ModelRc::new(VecModel::from(choices)));
        let sort_choices: Vec<ColumnChoice> = Column::DEFAULT
            .iter()
            .map(|column| ColumnChoice { title: column.title().into(), shown: *column == sort })
            .collect();
        ui.set_sort_choices(ModelRc::new(VecModel::from(sort_choices)));
        if self.list.state().columns != columns {
            self.list.update(|state| state.columns = columns);
        }
    }

    fn toggle_column(&mut self, choice: usize) {
        let Some(column) = Column::ALL.iter().filter(|column| !column.is_fixed()).nth(choice).copied() else { return };
        let columns = &mut self.settings.columns;
        if columns.contains(&column) {
            columns.retain(|shown| *shown != column);
        } else {
            columns.push(column);
            columns.sort_by_key(|shown| Column::ALL.iter().position(|known| known == shown));
        }
        self.sync_columns();
        self.start_hashing();
    }

    fn sort_by(&mut self, index: usize) {
        let Some(column) = self.list.state().columns.get(index).copied() else { return };
        self.sort_column(column);
    }

    fn sort_column(&mut self, column: Column) {
        self.list.update(|state| {
            state.descending = if state.sort == column { !state.descending } else { column.starts_descending() };
            state.sort = column;
            state.rebuild();
        });
        let state = self.list.state();
        self.settings.sort = state.sort;
        self.settings.descending = state.descending;
        drop(state);
        self.sync_columns();
    }

    fn resize_column(&mut self, index: usize, width: f32) {
        let Some(column) = self.list.state().columns.get(index).copied() else { return };
        self.settings.set_width(column, width);
        self.sync_columns();
    }

    fn choose_unit(&mut self, index: usize) {
        let unit = Unit::ALL[index];
        self.settings.unit = unit;
        self.ui().set_unit_index(index as i32);
        self.list.update(|state| state.unit = unit);
        self.show_selection();
        self.show_summary();
        self.show_drives();
    }

    fn choose_view(&mut self, index: usize) {
        let view = View::ALL[index];
        if let Location::Folder(path) = &self.location {
            self.settings.set_view_of(path, view);
        }
        self.show_view(view);
    }

    fn show_view(&self, view: View) {
        self.ui().set_view_index(view.index() as i32);
        self.icons.cancel_queued();
        self.list.update(|state| state.view = view);
    }

    fn refresh_icons(&self) {
        self.sidebar.refresh_icons();
        if self.drive_icons_missing.get() {
            self.show_drives();
        }
    }

    fn toggle_hidden(&mut self) {
        let show_hidden = self.ui().get_show_hidden();
        self.settings.show_hidden = show_hidden;
        self.list.update(|state| {
            state.show_hidden = show_hidden;
            state.rebuild();
        });
        self.show_selection();
    }

    fn show_totals(&self) {
        let state = self.list.state();
        let items = plural(state.row_count() as u64, "item");
        let text = format!("{items} · {}", format_size(state.total_size(), state.unit));
        self.ui().set_status_items(text.into());
    }

    fn show_selection(&self) {
        if self.location != Location::Computer {
            self.show_totals();
        }
        let state = self.list.state();
        let count = state.selected_count();
        let size: u64 = state.selected_items().map(|item| item.size).sum();
        let text = if count == 0 {
            String::new()
        } else {
            format!("{count} selected · {}", format_size(size, state.unit))
        };
        let ui = self.ui();
        ui.set_status_selection(text.into());
        ui.set_selected_count(count as i32);
        ui.set_single_selection(count == 1);
    }

    fn show_errors(&self) {
        let lines: Vec<SharedString> = self
            .errors
            .iter()
            .map(|error| format!("{} — {}", error.path.display(), error.message).into())
            .collect();
        let ui = self.ui();
        ui.set_status_errors(match lines.len() {
            0 => SharedString::new(),
            count => plural(count as u64, "unreadable item").into(),
        });
        ui.set_errors(ModelRc::new(VecModel::from(lines)));
    }

    // Drives and sidebar

    fn load_drives(&mut self) {
        self.spawn(|sender| {
            let _ = sender.send(Message::Drives(platform::drives()));
        });
        self.spawn(|sender| {
            let _ = sender.send(Message::RecycleBin(platform::recycle_bin_size()));
        });
    }

    fn on_drives(&mut self, drives: Vec<Drive>) {
        self.drives = drives;
        let next = self.sidebar.set_drives(&self.drives);
        self.load_tree_children(next);
        self.scroll_sidebar();
        self.show_drives();
    }

    fn load_pins(&mut self) {
        self.spawn(|sender| {
            let _ = sender.send(Message::Pins(platform::quick_access_pins()));
        });
    }

    fn on_pins(&mut self, pins: Vec<(PathBuf, String)>) {
        self.sidebar.set_pins(pins);
        self.scroll_sidebar();
    }

    fn scroll_sidebar(&mut self) {
        if let Some(row) = self.sidebar.take_scroll_row() {
            self.ui().invoke_reveal_tree_row(row as i32);
        }
    }

    fn show_drives(&self) {
        let mut missing_icons = false;
        let mut devices = Vec::new();
        let mut network = Vec::new();
        for drive in &self.drives {
            let icon = self.icons.location_icon(&drive.root.to_string_lossy(), DRIVE_ICON_SIZE);
            missing_icons |= icon.is_none();
            let card = drive_card(drive, icon.unwrap_or_default(), self.settings.unit);
            if drive.network {
                network.push(card);
            } else {
                devices.push(card);
            }
        }
        self.drive_icons_missing.set(missing_icons);
        let ui = self.ui();
        if self.location == Location::Computer {
            ui.set_status_items(plural(self.drives.len() as u64, "drive").into());
        }
        ui.set_drives(ModelRc::new(VecModel::from(devices)));
        ui.set_network_drives(ModelRc::new(VecModel::from(network)));
    }

    fn load_tree_children(&mut self, parent: Option<PathBuf>) {
        let Some(parent) = parent else { return };
        let show_hidden = self.settings.show_hidden;
        self.spawn(move |sender| {
            let folders = list_subfolders(&parent, show_hidden);
            let _ = sender.send(Message::TreeChildren { parent, folders });
        });
    }

    fn refresh_tree(&mut self, folder: Option<&Path>) {
        if let Some(folder) = folder.filter(|folder| self.sidebar.is_expanded(folder)) {
            self.load_tree_children(Some(folder.to_path_buf()));
        }
    }

    fn tree_opened(&mut self, row: usize) {
        match self.sidebar.path_at(row) {
            Some(path) => self.go(path.map_or(Location::Computer, Location::Folder)),
            None => self.tree_toggled(row),
        }
    }

    fn tree_toggled(&mut self, row: usize) {
        let parent = self.sidebar.toggle(row);
        self.load_tree_children(parent);
    }

    // Selection

    fn press_row(&mut self, row: usize, toggle: bool, extend: bool) {
        self.list.change_selection(|state| state.press(row, toggle, extend));
        self.show_selection();
    }

    fn context_row(&mut self, row: usize) {
        if !self.list.state().is_row_selected(row) {
            self.press_row(row, false, false);
        }
    }

    fn clear_selection(&mut self) {
        self.list.change_selection(ListState::clear_selection);
        self.show_selection();
    }

    fn move_cursor(&mut self, delta: i32, extend: bool) {
        if let Some(row) = self.list.change_selection(|state| state.move_cursor(delta, extend)) {
            self.ui().invoke_reveal_row(row as i32);
            self.show_selection();
        }
    }

    fn selected_paths(&self) -> Vec<PathBuf> {
        self.list.state().selected_items().map(Item::path).collect()
    }

    fn first_selected(&self) -> Option<PathBuf> {
        self.list.state().selected_items().next().map(Item::path)
    }

    fn list_action(&mut self, action: &str) {
        match action {
            "cut" => self.cut_selection(),
            "copy" => self.copy_selection(),
            "rename" => self.begin_rename(),
            "copy-path" => {
                let paths: Vec<OsString> = self.selected_paths().into_iter().map(PathBuf::into_os_string).collect();
                platform::copy_text(paths.join(OsStr::new("\r\n")));
            }
            "select-all" => {
                self.list.change_selection(ListState::select_all);
                self.show_selection();
            }
            "select-inverse" => {
                self.list.change_selection(ListState::invert_selection);
                self.show_selection();
            }
            "recalculate" => {
                for path in self.selected_paths() {
                    self.forget(&path);
                }
                self.reload();
            }
            "open-location" => {
                if let Some(parent) = self.first_selected().as_deref().and_then(Path::parent) {
                    self.go(Location::Folder(parent.to_path_buf()));
                }
            }
            "refresh" => self.refresh(),
            _ => {}
        }
    }

    // File operations

    fn editable_folder(&self) -> Option<PathBuf> {
        match &self.location {
            Location::Folder(path) if !self.showing_results => Some(path.clone()),
            _ => None,
        }
    }

    fn refresh_can_paste(&self) {
        self.ui().set_can_paste(self.editable_folder().is_some() && platform::clipboard_has_files());
    }

    fn cut_selection(&mut self) {
        let paths = self.selected_paths();
        if !paths.is_empty() && platform::cut_files(&paths) {
            self.cut_sequence = platform::clipboard_sequence();
            self.cut_paths = paths.into_iter().collect();
            self.show_cut_marks();
        }
    }

    fn copy_selection(&mut self) {
        let paths = self.selected_paths();
        if !paths.is_empty() && platform::copy_files(&paths) {
            self.cut_paths.clear();
            self.show_cut_marks();
        }
    }

    fn show_cut_marks(&self) {
        self.list.update(|state| state.mark_cut(&self.cut_paths));
    }

    fn restore_cut_marks(&mut self) {
        if platform::clipboard_sequence() != self.cut_sequence {
            self.cut_paths.clear();
        }
        if !self.cut_paths.is_empty() {
            self.show_cut_marks();
        }
    }

    fn begin_rename(&mut self) {
        let target = self
            .list
            .state()
            .only_selected()
            .map(|(row, item)| (row, item.name.to_string(), item.is_dir, item.path()));
        if let Some((row, name, is_dir, path)) = target {
            self.renaming = Some(path);
            let stem = file_ops::editable_stem_length(&name, is_dir);
            self.ui().invoke_begin_rename(row as i32, name.into(), stem as i32);
        }
    }

    fn cancel_rename(&mut self) {
        self.renaming = None;
        self.ui().invoke_focus_list();
    }

    fn on_renamed(&mut self, old: &Path, new: &Path) {
        let retired = self.cache.rename(old, new);
        thread::spawn(move || drop(retired));
        self.pending_select = new.file_name().map(|name| name.to_string_lossy().to_string());
        self.refresh_tree(new.parent());
        self.reload();
    }

    fn on_created(&mut self, parent: &Path, name: String) {
        self.forget(&parent.join(&name));
        self.pending_select = Some(name);
        self.pending_rename = true;
        self.refresh_tree(Some(parent));
        self.reload();
    }

    fn paste(&mut self, files: ClipboardFiles, owner: usize) {
        let Some(target) = self.editable_folder() else { return };
        let ClipboardFiles { mut paths, is_cut, sequence } = files;
        if is_cut {
            paths.retain(|path| path.parent() != Some(target.as_path()));
        }
        if paths.is_empty() {
            return;
        }
        self.spawn(move |sender| {
            let succeeded = match is_cut {
                true => platform::move_into(&paths, &target, owner),
                false => platform::copy_into(&paths, &target, owner),
            };
            let _ = sender.send(Message::Pasted { paths, target, is_cut, succeeded, sequence });
            let _ = sender.send(Message::RecycleBin(platform::recycle_bin_size()));
        });
    }

    fn on_pasted(&mut self, paths: &[PathBuf], target: &Path, is_cut: bool, succeeded: bool, sequence: u32) {
        for path in paths {
            if let Some(name) = path.file_name() {
                self.forget(&target.join(name));
            }
            if is_cut {
                self.forget(path);
                self.refresh_tree(path.parent());
            }
        }
        if is_cut && succeeded {
            platform::clear_clipboard_if_unchanged(sequence);
            self.cut_paths.clear();
        }
        self.refresh_tree(Some(target));
        self.reload();
        if !succeeded {
            self.ui().set_status_activity("Some items were not pasted.".into());
        }
    }

    fn recycle(&mut self, paths: Vec<PathBuf>, owner: usize) {
        self.cancel_scan();
        self.spawn(move |sender| {
            let succeeded = platform::recycle(&paths, owner);
            let _ = sender.send(Message::Recycled { paths, succeeded });
            let _ = sender.send(Message::RecycleBin(platform::recycle_bin_size()));
        });
    }

    fn on_recycled(&mut self, paths: &[PathBuf], succeeded: bool) {
        for path in paths {
            self.forget(path);
        }
        self.refresh_tree(paths.first().and_then(|path| path.parent()));
        self.reload();
        if !succeeded {
            self.ui().set_status_activity("Some items could not be moved to the Recycle Bin.".into());
        }
    }

    fn show_recycle_bin(&self, size: Option<u64>) {
        let title = match size {
            Some(size) if size > 0 => format!("Empty Recycle Bin ({})", format_size(size, self.settings.unit)),
            _ => "Empty Recycle Bin".into(),
        };
        self.ui().set_recycle_bin_title(title.into());
    }

    fn save_settings(&mut self) {
        let ui = self.ui();
        self.settings.placement = platform::window_placement(ui.window()).or(self.settings.placement);
        self.settings.sidebar_width = ui.get_sidebar_width();
        self.settings.summary_open = ui.get_summary_open();
        self.settings.tree = self.sidebar.memory();
        self.settings.save();
    }
}

// Actions that open modal or shell windows run without holding the app borrow

fn open_selection(shared: &Shared) {
    let (folder, files) = {
        let app = shared.borrow();
        let state = app.list.state();
        let folder = state.selected_items().find(|item| item.is_dir).map(Item::path);
        let files: Vec<PathBuf> = state.selected_items().filter(|item| !item.is_dir).map(Item::path).collect();
        (folder, files)
    };
    for file in &files {
        platform::open(file);
    }
    if let Some(path) = folder {
        shared.borrow_mut().go(Location::Folder(path));
    }
}

fn activate_row(shared: &Shared, row: usize) {
    let item = shared.borrow().list.state().item(row).map(|item| (item.is_dir, item.path()));
    match item {
        Some((true, path)) => shared.borrow_mut().go(Location::Folder(path)),
        Some((false, path)) => {
            platform::open(&path);
        }
        None => {}
    }
}

fn delete_selection(shared: &Shared) {
    let owner = platform::active_window();
    let paths = shared.borrow().selected_paths();
    let question = match paths.as_slice() {
        [] => return,
        [path] => format!("Move \"{}\" to the Recycle Bin?", path.display()),
        many => format!("Move {} to the Recycle Bin?", plural(many.len() as u64, "item")),
    };
    if platform::confirm("Move to Recycle Bin", &question) {
        shared.borrow_mut().recycle(paths, owner);
    }
}

fn describe_rename_error(error: &io::Error) -> String {
    match error.kind() {
        io::ErrorKind::AlreadyExists => "Something with that name already exists here.".into(),
        io::ErrorKind::PermissionDenied => "It may be open in another program, or you don't have permission.".into(),
        _ => error.to_string(),
    }
}

fn commit_rename(shared: &Shared, text: &str) {
    let old = shared.borrow_mut().renaming.take();
    let ui = shared.borrow().ui();
    ui.invoke_focus_list();
    let Some(old) = old else { return };
    if old.file_name().is_some_and(|current| current.to_string_lossy() == text) {
        return;
    }
    if let Err(reason) = file_ops::validate_name(text) {
        platform::alert("Can't rename", &reason);
        return;
    }
    let new = old.with_file_name(text);
    match platform::rename(&old, &new) {
        Ok(()) => shared.borrow_mut().on_renamed(&old, &new),
        Err(error) => platform::alert("Can't rename", &describe_rename_error(&error)),
    }
}

fn new_folder(shared: &Shared) {
    let parent = shared.borrow().editable_folder();
    let Some(parent) = parent else { return };
    match file_ops::create_new_folder(&parent) {
        Ok(name) => shared.borrow_mut().on_created(&parent, name),
        Err(error) => platform::alert("Can't create a folder", &error.to_string()),
    }
}

fn paste(shared: &Shared) {
    let owner = platform::active_window();
    if let Some(files) = platform::clipboard_files() {
        shared.borrow_mut().paste(files, owner);
    }
}

fn export_list(shared: &Shared) {
    let filters: Vec<(&str, &str)> = export::DELIMITERS.iter().map(|(label, extension, _)| (*label, *extension)).collect();
    let Some((path, filter)) = platform::save_dialog("Export list", "folder-list.csv", &filters) else { return };
    let delimiter = export::DELIMITERS.get(filter).map_or(',', |(_, _, delimiter)| *delimiter);
    let result = {
        let app = shared.borrow();
        let state = app.list.state();
        export::export_file(&path, &state.columns, state.visible_items(), delimiter)
    };
    if let Err(error) = result {
        platform::alert("Export failed", &error.to_string());
    }
}

fn run_action(shared: &Shared, action: &str) {
    match action {
        "open" => open_selection(shared),
        "delete" => delete_selection(shared),
        "export" => export_list(shared),
        "new-folder" => new_folder(shared),
        "paste" => paste(shared),
        "properties" => {
            let path = shared.borrow().first_selected();
            if let Some(path) = path {
                platform::show_properties(&path);
            }
        }
        "reveal" => {
            let path = shared.borrow().first_selected();
            if let Some(path) = path {
                platform::reveal_in_explorer(&path);
            }
        }
        "empty-bin" => {
            platform::empty_recycle_bin();
            shared.borrow_mut().load_drives();
        }
        other => shared.borrow_mut().list_action(other),
    }
}

fn wire(ui: &AppWindow, shared: &Shared) {
    macro_rules! on {
        ($setter:ident, |$app:ident $(, $argument:ident)*| $body:expr) => {{
            let shared = shared.clone();
            ui.$setter(move |$($argument),*| {
                let $app = &mut *shared.borrow_mut();
                $body;
            });
        }};
    }

    on!(on_go_back, |app| app.go_back());
    on!(on_go_forward, |app| app.go_forward());
    on!(on_go_up, |app| app.go_up());
    on!(on_open_location, |app, target| app.open_target(&target));
    on!(on_path_entered, |app, text| app.enter_typed_path(&text));
    on!(on_tree_opened, |app, row| app.tree_opened(row as usize));
    on!(on_tree_toggled, |app, row| app.tree_toggled(row as usize));
    on!(on_sort_requested, |app, index| app.sort_by(index as usize));
    on!(on_column_resized, |app, index, width| app.resize_column(index as usize, width));
    on!(on_row_pressed, |app, row, toggle, extend| app.press_row(row as usize, toggle, extend));
    on!(on_row_context, |app, row| app.context_row(row as usize));
    on!(on_background_pressed, |app| app.clear_selection());
    on!(on_cursor_moved, |app, delta, extend| app.move_cursor(delta, extend));
    on!(on_search_changed, |app| app.schedule_search());
    on!(on_summary_kind_chosen, |app, kind| app.choose_summary(kind as usize));
    on!(on_summary_viz_mode_chosen, |app, _mode| {
        let hovered = app.ui().get_summary_hovered_index();
        app.refresh_summary_wheel((hovered >= 0).then_some(hovered as usize));
    });
    on!(on_summary_wheel_hovered, |app, nx, ny| app.hover_summary_wheel(nx, ny));
    on!(on_summary_row_hovered, |app, index| app.hover_summary_row(index));
    on!(on_summary_activated, |app, row| app.activate_summary(row as usize));
    on!(on_summary_toggled, |app| app.refresh_summary());
    on!(on_unit_chosen, |app, index| app.choose_unit(index as usize));
    on!(on_column_toggled, |app, choice| app.toggle_column(choice as usize));
    on!(on_hidden_toggled, |app| app.toggle_hidden());
    on!(on_view_chosen, |app, index| app.choose_view(index as usize));
    on!(on_sort_chosen, |app, index| app.sort_column(Column::DEFAULT[index as usize]));
    on!(on_menu_opening, |app| app.refresh_can_paste());
    on!(on_rename_cancelled, |app| app.cancel_rename());

    {
        let shared = shared.clone();
        ui.on_refresh_or_stop(move || {
            let mut app = shared.borrow_mut();
            if app.scanning || app.hashes_left > 0 {
                app.cancel_scan();
            } else {
                app.refresh();
            }
        });
    }
    {
        let shared = shared.clone();
        ui.on_cancel_pressed(move || {
            let mut app = shared.borrow_mut();
            if app.scanning || app.hashes_left > 0 {
                app.cancel_scan();
            } else if app.showing_results {
                app.show_location();
            }
        });
    }
    {
        let shared = shared.clone();
        ui.on_row_activated(move |row| activate_row(&shared, row as usize));
    }
    {
        let shared = shared.clone();
        ui.on_rename_committed(move |text| commit_rename(&shared, &text));
    }
    {
        let shared = shared.clone();
        ui.on_action(move |action| run_action(&shared, &action));
    }
    {
        let shared = shared.clone();
        ui.window().on_close_requested(move || {
            shared.borrow_mut().save_settings();
            CloseRequestResponse::HideWindow
        });
    }
}

pub fn run(start: Option<PathBuf>) -> Result<(), slint::PlatformError> {
    platform::allow_dark_menus();
    let ui = AppWindow::new()?;
    let settings = Settings::load();
    let placement = settings.placement;
    match placement {
        Some(placement) => {
            let (width, height) = placement.size();
            ui.window().set_size(PhysicalSize::new(width, height));
        }
        None => ui.window().set_size(LogicalSize::new(DEFAULT_WINDOW_SIZE.0, DEFAULT_WINDOW_SIZE.1)),
    }
    ui.set_unit_index(settings.unit.index() as i32);
    ui.set_show_hidden(settings.show_hidden);
    ui.set_summary_open(settings.summary_open);
    ui.set_sidebar_width(settings.sidebar_width);

    ui.show()?;

    let icons = IconStore::new();
    icons.set_scale(ui.window().scale_factor());
    let shared: Shared = Rc::new_cyclic(|me| RefCell::new(App::new(&ui, me.clone(), settings, icons.clone())));
    wire(&ui, &shared);
    let (list, me) = (shared.borrow().list.clone(), Rc::downgrade(&shared));
    icons.set_listener(move || {
        list.refresh_icons();
        if let Some(shared) = me.upgrade()
            && let Ok(app) = shared.try_borrow()
        {
            app.refresh_icons();
        }
    });
    {
        let mut app = shared.borrow_mut();
        ui.set_rows(ModelRc::from(app.list.clone() as Rc<dyn slint::Model<Data = _>>));
        ui.set_tree_rows(app.sidebar.model());
        app.load_drives();
        app.load_pins();
        match start {
            Some(path) => app.location = Location::Folder(normalize(&path.to_string_lossy())),
            None => app.location = Location::Computer,
        }
        app.show_location();
    }
    let _ = ui.as_weak().upgrade_in_event_loop(move |ui| {
        let nav_ui = ui.as_weak();
        platform::restore_frame_appearance(ui.window(), move |forward| {
            let nav_ui = nav_ui.clone();
            let _ = slint::invoke_from_event_loop(move || {
                if let Some(ui) = nav_ui.upgrade() {
                    if forward {
                        ui.invoke_go_forward();
                    } else {
                        ui.invoke_go_back();
                    }
                }
            });
        });
        if let Some(placement) = placement {
            platform::apply_window_placement(ui.window(), placement);
        }
    });
    slint::run_event_loop()?;
    ui.hide()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::item::HASH_NAMES;
    use crate::scan::tests::{TempTree, scan};
    use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType};
    use slint::platform::{Platform, WindowAdapter, set_platform};
    use std::os::windows::ffi::OsStringExt;

    struct Headless;

    impl Platform for Headless {
        fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
            Ok(MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer))
        }
    }

    fn headless_app() -> (AppWindow, Shared) {
        let _ = set_platform(Box::new(Headless));
        let ui = AppWindow::new().unwrap();
        let shared: Shared =
            Rc::new_cyclic(|me| RefCell::new(App::new(&ui, me.clone(), Settings::default(), IconStore::new())));
        (ui, shared)
    }

    fn items_in(tree: &TempTree) -> Vec<Item> {
        let parent: Arc<Path> = tree.0.as_path().into();
        scan(&tree.0).files().map(|(name, entry)| Item::file(name, entry, &parent)).collect()
    }

    fn choice_of(column: Column) -> usize {
        Column::ALL.iter().filter(|known| !known.is_fixed()).position(|known| *known == column).unwrap()
    }

    fn summary_row() -> SummaryEntry {
        SummaryEntry { label: "old".into(), files: 1, size: 1, filter: Query::default(), folder: None }
    }

    fn md5(text: &str) -> Option<[String; 4]> {
        Some([text.into(), String::new(), String::new(), String::new()])
    }

    #[test]
    fn a_summary_requested_before_navigating_is_dropped() {
        let (_ui, shared) = headless_app();
        let mut app = shared.borrow_mut();
        let stale = app.summary_id;
        app.show_location();
        app.handle(Message::Summary { id: stale, entries: vec![summary_row()] });
        assert!(app.summary.is_empty());
        let current = app.summary_id;
        app.handle(Message::Summary { id: current, entries: vec![summary_row()] });
        assert_eq!(app.summary.len(), 1);
    }

    #[test]
    fn clearing_the_search_drops_results_still_on_their_way() {
        let (_ui, shared) = headless_app();
        let mut app = shared.borrow_mut();
        app.active_query = Some(Query::new("report"));
        let stale = app.search_id;
        app.apply_search();
        app.handle(Message::Found { search: stale, items: Vec::new(), seconds: 0.0 });
        assert!(!app.showing_results);
    }

    #[test]
    fn hiding_the_last_checksum_column_abandons_the_run() {
        let tree = TempTree::new();
        tree.file("a.txt", 1);
        let (_ui, shared) = headless_app();
        let mut app = shared.borrow_mut();
        app.list.set_items(items_in(&tree));
        app.toggle_column(choice_of(Column::Md5));
        let stale = app.hash_run;
        assert_eq!(app.hashes_left, 1);
        app.toggle_column(choice_of(Column::Md5));
        assert_eq!(app.hashes_left, 0);
        app.handle(Message::Hashed { run: stale, index: 0, hashes: md5("old") });
        assert!(app.list.state().items[0].hashes.is_none());
    }

    #[test]
    fn cancelling_checksums_restores_the_status_text() {
        let (ui, shared) = headless_app();
        let mut app = shared.borrow_mut();
        app.activity = "Scanned 3 files".into();
        ui.set_status_activity("Calculating checksums · 2 files left".into());
        app.hashes_left = 2;
        app.cancel_hashing();
        assert_eq!(ui.get_status_activity(), "Scanned 3 files");
    }

    #[test]
    fn a_checksum_sort_settles_when_the_last_hash_arrives() {
        let tree = TempTree::new();
        tree.file("a.txt", 1).file("b.txt", 1);
        let (_ui, shared) = headless_app();
        let mut app = shared.borrow_mut();
        app.list.set_items(items_in(&tree));
        app.list.update(|state| {
            state.sort = Column::Md5;
            state.rebuild();
        });
        let run = app.hash_run;
        app.hashes_left = 2;
        app.handle(Message::Hashed { run, index: 0, hashes: md5("ff") });
        app.handle(Message::Hashed { run, index: 1, hashes: md5("00") });
        let names: Vec<String> = app.list.state().visible_items().map(|item| item.name.to_string()).collect();
        assert_eq!(names, ["b.txt", "a.txt"]);
    }

    #[test]
    fn breadcrumb_text_maps_back_to_the_exact_folder() {
        let parent = Path::new(r"C:\data").join(OsString::from_wide(&[0x61, 0xD800]));
        let location = Location::Folder(parent.join("inner"));
        assert_eq!(resolve_target(&location, &parent.to_string_lossy()), parent);
        assert_eq!(resolve_target(&location, r"D:\other"), PathBuf::from(r"D:\other"));
    }

    #[test]
    fn hash_names_cover_every_checksum_column() {
        let titles: Vec<_> = Column::ALL.iter().filter(|column| column.hash_slot().is_some()).map(|column| column.title()).collect();
        assert_eq!(titles, HASH_NAMES);
    }

    #[test]
    fn typed_paths_are_normalized() {
        assert_eq!(normalize(r"C:\Windows\"), PathBuf::from(r"C:\Windows"));
        assert_eq!(normalize("C:"), PathBuf::from(r"C:\"));
        assert_eq!(normalize("\"C:\\Program Files\""), PathBuf::from(r"C:\Program Files"));
        assert_eq!(normalize("C:/Windows/System32"), PathBuf::from(r"C:\Windows\System32"));
    }

    #[test]
    fn crumbs_walk_from_this_pc_to_the_folder() {
        let crumbs = crumbs_for(&Location::Folder(PathBuf::from(r"C:\Users\me")));
        let names: Vec<_> = crumbs.iter().map(|crumb| crumb.name.as_str()).collect();
        let targets: Vec<_> = crumbs.iter().map(|crumb| crumb.target.as_str()).collect();
        assert_eq!(names, ["This PC", "C:", "Users", "me"]);
        assert_eq!(targets, ["", r"C:\", r"C:\Users", r"C:\Users\me"]);
    }

    #[test]
    fn roots_come_from_the_path() {
        assert_eq!(root_of(Path::new(r"D:\a\b")), PathBuf::from(r"D:\"));
    }

    #[test]
    fn missing_folders_read_plainly() {
        let error = io::Error::from(io::ErrorKind::NotFound);
        assert_eq!(describe_error(Path::new(r"C:\gone"), &error), r"C:\gone does not exist.");
    }
}
