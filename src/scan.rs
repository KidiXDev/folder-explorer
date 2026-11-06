use rayon::prelude::*;
use std::collections::HashMap;
use std::ffi::OsStr;
use std::fs;
use std::io;
use std::os::windows::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};
use std::sync::{Arc, Mutex};

const MAX_LOGGED_ERRORS: usize = 10_000;
const ATTR_DIRECTORY: u32 = 0x10;
const ATTR_REPARSE_POINT: u32 = 0x400;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Times {
    pub modified: u64,
    pub created: u64,
    pub accessed: u64,
}

impl Times {
    fn of(meta: &fs::Metadata) -> Self {
        Self {
            modified: meta.last_write_time(),
            created: meta.creation_time(),
            accessed: meta.last_access_time(),
        }
    }
}

#[derive(Clone)]
pub struct FileEntry {
    name_end: u32,
    pub attrs: u32,
    pub size: u64,
    pub times: Times,
}

impl FileEntry {
    pub fn is_folder_link(&self) -> bool {
        self.attrs & ATTR_DIRECTORY != 0
    }
}

pub struct DirNode {
    pub name: Box<str>,
    pub attrs: u32,
    pub times: Times,
    pub size: u64,
    pub file_count: u64,
    pub dir_count: u64,
    pub error_count: u64,
    pub complete: bool,
    pub children: Vec<Arc<DirNode>>,
    pub entries: Vec<FileEntry>,
    names: String,
}

impl DirNode {
    fn new(dir: &SubDir) -> Self {
        Self {
            name: dir.name.clone(),
            attrs: dir.attrs,
            times: dir.times,
            size: 0,
            file_count: 0,
            dir_count: 0,
            error_count: 0,
            complete: true,
            children: Vec::new(),
            entries: Vec::new(),
            names: String::new(),
        }
    }

    fn push_entry(&mut self, name: &str, meta: &fs::Metadata) -> u64 {
        self.names.push_str(name);
        let size = meta.file_size();
        self.entries.push(FileEntry {
            name_end: self.names.len() as u32,
            attrs: meta.file_attributes(),
            size,
            times: Times::of(meta),
        });
        size
    }

    fn add_file(&mut self, name: &str, meta: &fs::Metadata) {
        self.size += self.push_entry(name, meta);
        self.file_count += 1;
    }

    fn add_link(&mut self, name: &str, meta: &fs::Metadata) {
        self.push_entry(name, meta);
    }

    pub fn absorb(&mut self, children: Vec<Arc<DirNode>>) {
        for child in &children {
            self.size += child.size;
            self.file_count += child.file_count;
            self.dir_count += child.dir_count + 1;
            self.error_count += child.error_count;
            self.complete &= child.complete;
        }
        self.children = children;
    }

    pub fn files(&self) -> impl Iterator<Item = (&str, &FileEntry)> {
        let mut start = 0;
        self.entries.iter().map(move |entry| {
            let end = entry.name_end as usize;
            let name = &self.names[start..end];
            start = end;
            (name, entry)
        })
    }

    fn renamed(&self, name: &OsStr) -> Self {
        Self {
            name: name.to_string_lossy().into(),
            attrs: self.attrs,
            times: self.times,
            size: self.size,
            file_count: self.file_count,
            dir_count: self.dir_count,
            error_count: self.error_count,
            complete: self.complete,
            children: self.children.clone(),
            entries: self.entries.clone(),
            names: self.names.clone(),
        }
    }

    fn child(&self, name: &OsStr) -> Option<&Arc<DirNode>> {
        self.children.iter().find(|child| same_name(&child.name, name))
    }
}

fn same_name(known: &str, wanted: &OsStr) -> bool {
    wanted.to_str().is_some_and(|wanted| known.eq_ignore_ascii_case(wanted))
}

#[derive(Clone)]
pub struct SubDir {
    pub name: Box<str>,
    pub path: PathBuf,
    pub attrs: u32,
    pub times: Times,
}

impl SubDir {
    pub fn at(path: &Path) -> io::Result<Self> {
        let meta = fs::metadata(path)?;
        if !meta.is_dir() {
            return Err(io::Error::new(io::ErrorKind::NotADirectory, "Not a folder"));
        }
        let name = path.file_name().unwrap_or(path.as_os_str());
        Ok(Self {
            name: name.to_string_lossy().into(),
            path: path.to_path_buf(),
            attrs: meta.file_attributes(),
            times: Times::of(&meta),
        })
    }
}

#[derive(Clone, Debug)]
pub struct ScanError {
    pub path: PathBuf,
    pub message: String,
}

#[derive(Default)]
pub struct ErrorLog(Mutex<Vec<ScanError>>);

impl ErrorLog {
    fn record(&self, path: &Path, error: &io::Error) {
        let mut errors = self.0.lock().unwrap();
        if errors.len() < MAX_LOGGED_ERRORS {
            let text = error.to_string();
            let message = text.split(" (os error").next().unwrap_or(&text).to_string();
            errors.push(ScanError { path: path.to_path_buf(), message });
        }
    }

    pub fn into_inner(self) -> Vec<ScanError> {
        self.0.into_inner().unwrap()
    }
}

pub struct Level {
    pub node: DirNode,
    pub subdirs: Vec<SubDir>,
}

fn is_link(entry: &fs::DirEntry, file_type: fs::FileType, attrs: u32) -> bool {
    file_type.is_symlink()
        || attrs & ATTR_REPARSE_POINT != 0
            && fs::symlink_metadata(entry.path()).is_ok_and(|meta| meta.file_type().is_symlink())
}

pub fn read_level(dir: &SubDir, errors: &ErrorLog) -> Result<Level, io::Error> {
    let mut node = DirNode::new(dir);
    let mut subdirs = Vec::new();
    for entry in fs::read_dir(&dir.path)? {
        match entry.and_then(|entry| {
            let file_type = entry.file_type()?;
            Ok((entry.metadata()?, file_type, entry))
        }) {
            Ok((meta, file_type, entry)) => {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                let attrs = meta.file_attributes();
                if is_link(&entry, file_type, attrs) {
                    node.add_link(&name, &meta);
                } else if file_type.is_dir() {
                    subdirs.push(SubDir {
                        name: name.as_ref().into(),
                        path: entry.path(),
                        attrs,
                        times: Times::of(&meta),
                    });
                } else {
                    node.add_file(&name, &meta);
                }
            }
            Err(error) => {
                node.error_count += 1;
                errors.record(&dir.path, &error);
            }
        }
    }
    Ok(Level { node, subdirs })
}

#[derive(Default)]
pub struct Live {
    pub size: AtomicU64,
    pub files: AtomicU64,
    pub dirs: AtomicU64,
}

impl Live {
    fn add(&self, size: u64, files: u64, dirs: u64) {
        self.size.fetch_add(size, Relaxed);
        self.files.fetch_add(files, Relaxed);
        self.dirs.fetch_add(dirs, Relaxed);
    }
}

pub struct ScanContext {
    pub cancel: Arc<AtomicBool>,
    pub known: HashMap<PathBuf, Arc<DirNode>>,
    pub errors: ErrorLog,
}

pub fn scan_tree(dir: &SubDir, context: &ScanContext, live: &Live) -> Arc<DirNode> {
    if let Some(known) = context.known.get(&dir.path) {
        live.add(known.size, known.file_count, known.dir_count);
        return known.clone();
    }
    let mut unread = DirNode::new(dir);
    if context.cancel.load(Relaxed) {
        unread.complete = false;
        return Arc::new(unread);
    }
    let Level { mut node, subdirs } = match read_level(dir, &context.errors) {
        Ok(level) => level,
        Err(error) => {
            context.errors.record(&dir.path, &error);
            unread.error_count = 1;
            return Arc::new(unread);
        }
    };
    live.add(node.size, node.file_count, subdirs.len() as u64);
    let children = subdirs.par_iter().map(|subdir| scan_tree(subdir, context, live)).collect();
    node.absorb(children);
    Arc::new(node)
}

#[derive(Default)]
pub struct Cache {
    roots: HashMap<PathBuf, Arc<DirNode>>,
}

impl Cache {
    pub fn store(&mut self, path: &Path, node: &Arc<DirNode>) {
        if node.complete {
            self.roots.insert(path.to_path_buf(), node.clone());
            return;
        }
        for child in &node.children {
            self.store(&path.join(&*child.name), child);
        }
    }

    pub fn get(&self, path: &Path) -> Option<Arc<DirNode>> {
        let (root_path, root) = path.ancestors().find_map(|at| self.roots.get_key_value(at))?;
        let below = path.strip_prefix(root_path).ok()?;
        below.iter().try_fold(root.clone(), |node, part| node.child(part).cloned())
    }

    pub fn known_below(&self, path: &Path) -> HashMap<PathBuf, Arc<DirNode>> {
        let mut known: HashMap<_, _> = self
            .roots
            .iter()
            .filter(|(root, _)| root.starts_with(path) && root.as_path() != path)
            .map(|(root, node)| (root.clone(), node.clone()))
            .collect();
        if let Some(node) = self.get(path) {
            for child in &node.children {
                known.insert(path.join(&*child.name), child.clone());
            }
        }
        known
    }

    pub fn invalidate(&mut self, path: &Path) -> Vec<Arc<DirNode>> {
        let mut retired = Vec::new();
        self.roots.retain(|root, node| {
            let keep = !root.starts_with(path);
            if !keep {
                retired.push(node.clone());
            }
            keep
        });
        for ancestor in path.ancestors().skip(1) {
            if let Some(root) = self.roots.remove(ancestor) {
                self.keep_siblings_of(path, ancestor, root.clone());
                retired.push(root);
            }
        }
        retired
    }

    pub fn rename(&mut self, from: &Path, to: &Path) -> Vec<Arc<DirNode>> {
        let moved = self.get(from);
        let retired = self.invalidate(from);
        if let (Some(node), Some(name)) = (moved, to.file_name()) {
            self.store(to, &Arc::new(node.renamed(name)));
        }
        retired
    }

    fn keep_siblings_of(&mut self, path: &Path, root_path: &Path, root: Arc<DirNode>) {
        let mut node = root;
        let mut at = root_path.to_path_buf();
        let below = path.strip_prefix(root_path).expect("root is an ancestor");
        for part in below {
            let mut next = None;
            for child in &node.children {
                if same_name(&child.name, part) {
                    next = Some(child.clone());
                } else {
                    self.roots.insert(at.join(&*child.name), child.clone());
                }
            }
            at.push(part);
            match next {
                Some(child) => node = child,
                None => return,
            }
        }
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use std::process::Command;
    use std::sync::atomic::AtomicUsize;

    pub struct TempTree(pub PathBuf);

    impl TempTree {
        pub fn new() -> Self {
            static NEXT: AtomicUsize = AtomicUsize::new(0);
            let unique = format!("folder-explorer-{}-{}", std::process::id(), NEXT.fetch_add(1, Relaxed));
            let root = std::env::temp_dir().join(unique);
            fs::create_dir_all(&root).unwrap();
            Self(root)
        }

        pub fn file(&self, relative: &str, size: usize) -> &Self {
            let path = self.0.join(relative);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, vec![b'x'; size]).unwrap();
            self
        }
    }

    impl Drop for TempTree {
        fn drop(&mut self) {
            let _ = Command::new("cmd").args(["/c", "rmdir", "/s", "/q"]).arg(&self.0).output();
        }
    }

    fn context() -> ScanContext {
        ScanContext { cancel: Arc::default(), known: HashMap::new(), errors: ErrorLog::default() }
    }

    pub fn scan(path: &Path) -> Arc<DirNode> {
        scan_tree(&SubDir::at(path).unwrap(), &context(), &Live::default())
    }

    fn sample() -> TempTree {
        let tree = TempTree::new();
        tree.file("a.txt", 10).file("sub/b.txt", 20).file("sub/deep/c.log", 30).file("other/d.bin", 40);
        tree
    }

    #[test]
    fn totals_cover_the_whole_tree() {
        let tree = sample();
        let node = scan(&tree.0);
        assert_eq!((node.size, node.file_count, node.dir_count), (100, 4, 3));
        assert!(node.complete);
    }

    #[test]
    fn file_names_survive_the_packed_storage() {
        let tree = TempTree::new();
        tree.file("one.txt", 1).file("two words.txt", 2).file("日本語.txt", 3);
        let node = scan(&tree.0);
        let mut names: Vec<_> = node.files().map(|(name, entry)| (name.to_string(), entry.size)).collect();
        names.sort();
        assert_eq!(
            names,
            [("one.txt".into(), 1), ("two words.txt".into(), 2), ("日本語.txt".into(), 3)]
        );
    }

    #[test]
    fn junctions_are_listed_but_not_followed() {
        let tree = sample();
        let target = tree.0.join("sub");
        let junction = tree.0.join("link");
        let made = Command::new("cmd").args(["/c", "mklink", "/J"]).arg(&junction).arg(&target).output().unwrap();
        assert!(made.status.success());
        let node = scan(&tree.0);
        assert_eq!((node.size, node.file_count, node.dir_count), (100, 4, 3));
        assert!(node.files().any(|(name, _)| name == "link"));
    }

    #[test]
    fn cancelled_scans_are_marked_incomplete() {
        let tree = sample();
        let context = ScanContext { cancel: Arc::new(AtomicBool::new(true)), ..context() };
        let node = scan_tree(&SubDir::at(&tree.0).unwrap(), &context, &Live::default());
        assert!(!node.complete);
    }

    #[test]
    fn known_subtrees_are_reused_without_reading_them() {
        let tree = sample();
        let sub = tree.0.join("sub");
        let mut cache = Cache::default();
        cache.store(&sub, &scan(&sub));
        fs::remove_dir_all(&sub).unwrap();
        let context = ScanContext { known: cache.known_below(&tree.0), ..context() };
        let node = scan_tree(&SubDir::at(&tree.0).unwrap(), &context, &Live::default());
        assert_eq!(node.size, 50);
    }

    #[test]
    fn cache_finds_folders_inside_a_stored_tree() {
        let tree = sample();
        let mut cache = Cache::default();
        cache.store(&tree.0, &scan(&tree.0));
        let deep = cache.get(&tree.0.join("sub").join("deep")).unwrap();
        assert_eq!(deep.size, 30);
        assert!(cache.get(&tree.0.join("missing")).is_none());
    }

    #[test]
    fn invalidating_keeps_siblings_but_drops_the_path_and_its_ancestors() {
        let tree = sample();
        let mut cache = Cache::default();
        cache.store(&tree.0, &scan(&tree.0));
        cache.invalidate(&tree.0.join("sub").join("deep"));
        assert!(cache.get(&tree.0).is_none());
        assert!(cache.get(&tree.0.join("sub")).is_none());
        assert!(cache.get(&tree.0.join("sub").join("deep")).is_none());
        assert_eq!(cache.get(&tree.0.join("other")).unwrap().size, 40);
    }

    #[test]
    fn invalidating_reaches_every_stale_ancestor() {
        let tree = sample();
        let sub = tree.0.join("sub");
        let mut cache = Cache::default();
        cache.store(&tree.0, &scan(&tree.0));
        cache.store(&sub, &scan(&sub));
        cache.invalidate(&sub.join("deep"));
        assert!(cache.get(&tree.0).is_none());
        assert!(cache.get(&sub).is_none());
    }

    #[test]
    fn renaming_keeps_the_scanned_subtree_under_its_new_name() {
        let tree = sample();
        let mut cache = Cache::default();
        cache.store(&tree.0, &scan(&tree.0));
        let (from, to) = (tree.0.join("sub"), tree.0.join("renamed"));
        cache.rename(&from, &to);
        assert!(cache.get(&from).is_none());
        assert!(cache.get(&tree.0).is_none());
        let moved = cache.get(&to).unwrap();
        assert_eq!((&*moved.name, moved.size, moved.file_count), ("renamed", 50, 2));
        assert_eq!(cache.get(&to.join("deep")).unwrap().size, 30);
        assert_eq!(moved.files().next().unwrap().0, "b.txt");
        assert_eq!(cache.get(&tree.0.join("other")).unwrap().size, 40);
    }

    #[test]
    fn renaming_an_unscanned_path_only_drops_stale_ancestors() {
        let tree = sample();
        let mut cache = Cache::default();
        cache.store(&tree.0, &scan(&tree.0));
        cache.rename(&tree.0.join("a.txt"), &tree.0.join("b.txt"));
        assert!(cache.get(&tree.0).is_none());
        assert_eq!(cache.get(&tree.0.join("sub")).unwrap().size, 50);
    }

    #[test]
    fn incomplete_trees_only_store_their_finished_parts() {
        let tree = sample();
        let finished = scan(&tree.0.join("other"));
        let root = SubDir::at(&tree.0).unwrap();
        let mut partial = DirNode::new(&root);
        let unfinished = Arc::new(DirNode { complete: false, ..DirNode::new(&SubDir::at(&tree.0.join("sub")).unwrap()) });
        partial.absorb(vec![finished, unfinished]);
        let mut cache = Cache::default();
        cache.store(&tree.0, &Arc::new(partial));
        assert!(cache.get(&tree.0).is_none());
        assert!(cache.get(&tree.0.join("sub")).is_none());
        assert_eq!(cache.get(&tree.0.join("other")).unwrap().size, 40);
    }
}
