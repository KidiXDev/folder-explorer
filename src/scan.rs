use rayon::prelude::*;
use std::collections::HashMap;
use std::ffi::OsStr;
use std::fs;
use std::io;
use std::os::windows::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};

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
    pub name: Box<OsStr>,
    pub attrs: u32,
    pub times: Times,
    pub size: u64,
    pub file_count: u64,
    pub dir_count: u64,
    pub error_count: u64,
    pub complete: bool,
    pub children: Vec<Arc<DirNode>>,
    pub entries: Vec<FileEntry>,
    names: Vec<u8>,
    errors: Vec<Box<str>>,
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
            names: Vec::new(),
            errors: Vec::new(),
        }
    }

    fn push_entry(&mut self, name: &OsStr, meta: &fs::Metadata) -> u64 {
        self.names.extend_from_slice(name.as_encoded_bytes());
        let size = meta.file_size();
        self.entries.push(FileEntry {
            name_end: self.names.len() as u32,
            attrs: meta.file_attributes(),
            size,
            times: Times::of(meta),
        });
        size
    }

    fn add_file(&mut self, name: &OsStr, meta: &fs::Metadata) {
        self.size += self.push_entry(name, meta);
        self.file_count += 1;
    }

    fn add_link(&mut self, name: &OsStr, meta: &fs::Metadata) {
        self.push_entry(name, meta);
    }

    fn record_error(&mut self, error: &io::Error) {
        let text = error.to_string();
        self.errors.push(text.split(" (os error").next().unwrap_or(&text).into());
        self.error_count += 1;
    }

    pub fn collect_errors(&self, path: &Path, into: &mut Vec<ScanError>) {
        let room = MAX_LOGGED_ERRORS.saturating_sub(into.len());
        into.extend(
            self.errors.iter().take(room).map(|message| ScanError { path: path.to_path_buf(), message: message.to_string() }),
        );
        for child in self.children.iter().filter(|child| child.error_count > 0) {
            child.collect_errors(&path.join(&*child.name), into);
        }
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

    pub fn files(&self) -> impl Iterator<Item = (&OsStr, &FileEntry)> {
        let mut start = 0;
        self.entries.iter().map(move |entry| {
            let end = entry.name_end as usize;
            let name = unsafe { OsStr::from_encoded_bytes_unchecked(&self.names[start..end]) };
            start = end;
            (name, entry)
        })
    }

    fn renamed(&self, name: &OsStr) -> Self {
        Self {
            name: name.into(),
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
            errors: self.errors.clone(),
        }
    }

    fn child(&self, name: &OsStr) -> Option<&Arc<DirNode>> {
        self.children.iter().find(|child| &*child.name == name)
    }
}

#[derive(Clone)]
pub struct SubDir {
    pub name: Box<OsStr>,
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
            name: name.into(),
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

pub struct Level {
    pub node: DirNode,
    pub subdirs: Vec<SubDir>,
}

fn is_link(entry: &fs::DirEntry, file_type: fs::FileType, attrs: u32) -> bool {
    file_type.is_symlink()
        || attrs & ATTR_REPARSE_POINT != 0
            && fs::symlink_metadata(entry.path()).is_ok_and(|meta| meta.file_type().is_symlink())
}

pub fn read_level(dir: &SubDir) -> Result<Level, io::Error> {
    let mut node = DirNode::new(dir);
    let mut subdirs = Vec::new();
    for entry in fs::read_dir(&dir.path)? {
        match entry.and_then(|entry| {
            let file_type = entry.file_type()?;
            Ok((entry.metadata()?, file_type, entry))
        }) {
            Ok((meta, file_type, entry)) => {
                let name = entry.file_name();
                let attrs = meta.file_attributes();
                if is_link(&entry, file_type, attrs) {
                    node.add_link(&name, &meta);
                } else if file_type.is_dir() {
                    subdirs.push(SubDir {
                        name: name.into_boxed_os_str(),
                        path: entry.path(),
                        attrs,
                        times: Times::of(&meta),
                    });
                } else {
                    node.add_file(&name, &meta);
                }
            }
            Err(error) => node.record_error(&error),
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
    let Level { mut node, subdirs } = match read_level(dir) {
        Ok(level) => level,
        Err(error) => {
            unread.record_error(&error);
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
    generation: u64,
}

impl Cache {
    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn store_scan(&mut self, generation: u64, path: &Path, node: &Arc<DirNode>) {
        if generation == self.generation {
            self.store(path, node);
        }
    }

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
        self.generation += 1;
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
                if &*child.name == part {
                    next = Some(child.clone());
                } else if !child.name.eq_ignore_ascii_case(part) {
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
    use std::ffi::OsString;
    use std::os::windows::ffi::OsStringExt;
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
        ScanContext { cancel: Arc::default(), known: HashMap::new() }
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
        let mut names: Vec<_> = node.files().map(|(name, entry)| (name.to_string_lossy().into_owned(), entry.size)).collect();
        names.sort();
        assert_eq!(
            names,
            [("one.txt".into(), 1), ("two words.txt".into(), 2), ("日本語.txt".into(), 3)]
        );
    }

    #[test]
    fn names_that_are_not_valid_unicode_stay_distinct() {
        let tree = TempTree::new();
        let names = [OsString::from_wide(&[0x61, 0xD800]), OsString::from_wide(&[0x61, 0xD801])];
        for (index, name) in names.iter().enumerate() {
            fs::write(tree.0.join(name), vec![0; index + 1]).unwrap();
        }
        let node = scan(&tree.0);
        let mut found: Vec<_> = node.files().map(|(name, entry)| (name.to_os_string(), entry.size)).collect();
        found.sort();
        assert_eq!(found, [(names[0].clone(), 1), (names[1].clone(), 2)]);
    }

    #[test]
    fn neighbouring_names_do_not_merge_their_surrogates() {
        let tree = TempTree::new();
        let names = [OsString::from_wide(&[0x61, 0xD83D]), OsString::from_wide(&[0xDE00, 0x62])];
        for name in &names {
            fs::write(tree.0.join(name), [0]).unwrap();
        }
        let node = scan(&tree.0);
        let mut found: Vec<_> = node.files().map(|(name, _)| name.to_os_string()).collect();
        found.sort();
        assert_eq!(found, names);
    }

    #[test]
    fn folders_with_invalid_names_are_found_in_the_cache() {
        let tree = TempTree::new();
        let folder = tree.0.join(OsString::from_wide(&[0x61, 0xD800]));
        fs::create_dir(&folder).unwrap();
        fs::write(folder.join("inside.txt"), [0; 3]).unwrap();
        let mut cache = Cache::default();
        cache.store(&tree.0, &scan(&tree.0));
        assert_eq!(cache.get(&folder).unwrap().size, 3);
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
        assert_eq!((&*moved.name, moved.size, moved.file_count), (OsStr::new("renamed"), 50, 2));
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

    fn subdir(name: &str) -> SubDir {
        SubDir { name: OsStr::new(name).into(), path: PathBuf::from(name), attrs: 0, times: Times::default() }
    }

    fn sized_folder(name: &str, size: u64) -> Arc<DirNode> {
        Arc::new(DirNode { size, ..DirNode::new(&subdir(name)) })
    }

    fn cached_folders(root: &Path, children: Vec<Arc<DirNode>>) -> Cache {
        let mut parent = DirNode::new(&subdir("root"));
        parent.absorb(children);
        let mut cache = Cache::default();
        cache.store(root, &Arc::new(parent));
        cache
    }

    #[test]
    fn siblings_differing_only_by_case_stay_apart() {
        let root = PathBuf::from(r"C:\root");
        let cache = cached_folders(&root, vec![sized_folder("foo", 1), sized_folder("Foo", 2)]);
        assert_eq!(cache.get(&root.join("foo")).unwrap().size, 1);
        assert_eq!(cache.get(&root.join("Foo")).unwrap().size, 2);
        assert!(cache.get(&root.join("FOO")).is_none());
    }

    #[test]
    fn invalidating_a_differently_cased_path_keeps_nothing_it_may_name() {
        let root = PathBuf::from(r"C:\root");
        let mut cache = cached_folders(&root, vec![sized_folder("foo", 1), sized_folder("other", 2)]);
        cache.invalidate(&root.join("FOO").join("inner"));
        assert!(cache.get(&root.join("foo")).is_none());
        assert_eq!(cache.get(&root.join("other")).unwrap().size, 2);
    }

    #[test]
    fn scans_started_before_an_invalidation_are_not_stored() {
        let tree = sample();
        let mut cache = Cache::default();
        let started = cache.generation();
        let node = scan(&tree.0);
        cache.invalidate(&tree.0);
        cache.store_scan(started, &tree.0, &node);
        assert!(cache.get(&tree.0).is_none());
    }

    #[test]
    fn scans_with_no_invalidation_since_they_started_are_stored() {
        let tree = sample();
        let mut cache = Cache::default();
        cache.invalidate(&tree.0);
        cache.store_scan(cache.generation(), &tree.0, &scan(&tree.0));
        assert_eq!(cache.get(&tree.0).unwrap().size, 100);
    }

    #[test]
    fn reused_subtrees_keep_their_error_messages() {
        let tree = sample();
        let sub = tree.0.join("sub");
        let dir = SubDir::at(&sub).unwrap();
        fs::remove_dir_all(&sub).unwrap();
        let failed = scan_tree(&dir, &context(), &Live::default());
        fs::create_dir(&sub).unwrap();
        let mut cache = Cache::default();
        cache.store(&sub, &failed);
        let context = ScanContext { known: cache.known_below(&tree.0), ..context() };
        let node = scan_tree(&SubDir::at(&tree.0).unwrap(), &context, &Live::default());
        let mut errors = Vec::new();
        node.collect_errors(&tree.0, &mut errors);
        assert_eq!(node.error_count, 1);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].path, sub);
    }

    #[test]
    fn logged_errors_stop_at_the_limit() {
        let mut node = DirNode::new(&subdir("root"));
        let error = io::Error::from(io::ErrorKind::NotFound);
        (0..MAX_LOGGED_ERRORS + 5).for_each(|_| node.record_error(&error));
        let mut errors = Vec::new();
        node.collect_errors(Path::new(r"C:\root"), &mut errors);
        assert_eq!(errors.len(), MAX_LOGGED_ERRORS);
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
