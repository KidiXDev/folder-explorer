use crate::format::{Unit, format_size, group_digits};
use crate::platform;
use crate::scan::{DirNode, FileEntry, SubDir, Times};
use std::cmp::Ordering;
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub const ATTR_READONLY: u32 = 0x1;
pub const ATTR_HIDDEN: u32 = 0x2;
pub const ATTR_SYSTEM: u32 = 0x4;
pub const ATTR_DIRECTORY: u32 = 0x10;
pub const ATTR_ARCHIVE: u32 = 0x20;
pub const ATTR_REPARSE_POINT: u32 = 0x400;
pub const ATTR_COMPRESSED: u32 = 0x800;
pub const ATTR_ENCRYPTED: u32 = 0x4000;

pub const HASH_NAMES: [&str; 4] = ["MD5", "SHA-1", "SHA-256", "SHA-512"];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ItemState {
    Ready,
    Pending,
    Partial,
    Cancelled,
}

#[derive(Clone, Debug)]
pub struct Item {
    pub name: Box<str>,
    pub parent: Arc<str>,
    pub is_dir: bool,
    pub attrs: u32,
    pub times: Times,
    pub size: u64,
    pub file_count: u64,
    pub dir_count: u64,
    pub state: ItemState,
    pub hashes: Option<Box<[String; 4]>>,
}

impl Item {
    pub fn pending_folder(dir: &SubDir, parent: &Arc<str>) -> Self {
        Self {
            name: dir.name.clone(),
            parent: parent.clone(),
            is_dir: true,
            attrs: dir.attrs,
            times: dir.times,
            size: 0,
            file_count: 0,
            dir_count: 0,
            state: ItemState::Pending,
            hashes: None,
        }
    }

    pub fn file(name: &str, entry: &FileEntry, parent: &Arc<str>) -> Self {
        Self {
            name: name.into(),
            parent: parent.clone(),
            is_dir: entry.attrs & ATTR_DIRECTORY != 0,
            attrs: entry.attrs,
            times: entry.times,
            size: entry.size,
            file_count: 0,
            dir_count: 0,
            state: ItemState::Ready,
            hashes: None,
        }
    }

    pub fn folder(node: &DirNode, parent: &Arc<str>) -> Self {
        let mut item = Self {
            name: node.name.clone(),
            parent: parent.clone(),
            is_dir: true,
            attrs: node.attrs,
            times: node.times,
            size: 0,
            file_count: 0,
            dir_count: 0,
            state: ItemState::Pending,
            hashes: None,
        };
        item.apply_totals(node);
        item
    }

    pub fn apply_totals(&mut self, node: &DirNode) {
        self.size = node.size;
        self.file_count = node.file_count;
        self.dir_count = node.dir_count;
        self.state = match (node.complete, node.error_count) {
            (false, _) => ItemState::Cancelled,
            (true, 0) => ItemState::Ready,
            (true, _) => ItemState::Partial,
        };
    }

    pub fn path(&self) -> PathBuf {
        Path::new(&*self.parent).join(&*self.name)
    }

    pub fn is_hidden(&self) -> bool {
        self.attrs & ATTR_HIDDEN != 0
    }

    fn extension(&self) -> &str {
        match self.name.rsplit_once('.') {
            Some((stem, extension)) if !self.is_dir && !stem.is_empty() => extension,
            _ => "",
        }
    }

    pub fn type_label(&self) -> String {
        match (self.is_dir, self.extension()) {
            (true, _) => "Folder".into(),
            (false, "") => "File".into(),
            (false, extension) => format!("{} File", extension.to_ascii_uppercase()),
        }
    }

    fn status_label(&self) -> &'static str {
        match self.state {
            ItemState::Ready => "OK",
            ItemState::Pending => "Scanning",
            ItemState::Partial => "Partial",
            ItemState::Cancelled => "Cancelled",
        }
    }

    fn size_label(&self, unit: Unit) -> String {
        match self.state {
            ItemState::Pending if self.size == 0 => "…".into(),
            ItemState::Pending | ItemState::Partial | ItemState::Cancelled => {
                format!("≥ {}", format_size(self.size, unit))
            }
            ItemState::Ready => format_size(self.size, unit),
        }
    }

    fn count_label(&self, count: u64) -> String {
        match (self.is_dir, self.state) {
            (false, _) => String::new(),
            (true, ItemState::Pending) if self.size == 0 => "…".into(),
            (true, _) => group_digits(count),
        }
    }

    fn attributes_label(&self) -> String {
        [
            (ATTR_READONLY, 'R'),
            (ATTR_HIDDEN, 'H'),
            (ATTR_SYSTEM, 'S'),
            (ATTR_ARCHIVE, 'A'),
            (ATTR_COMPRESSED, 'C'),
            (ATTR_ENCRYPTED, 'E'),
            (ATTR_REPARSE_POINT, 'L'),
        ]
        .iter()
        .filter(|(flag, _)| self.attrs & flag != 0)
        .map(|(_, letter)| letter)
        .collect()
    }

    fn hash_label(&self, slot: usize) -> String {
        match &self.hashes {
            Some(hashes) => hashes[slot].clone(),
            None if self.is_dir => String::new(),
            None => "…".into(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Column {
    Name,
    Size,
    Files,
    Folders,
    Modified,
    Created,
    Accessed,
    Type,
    Attributes,
    Status,
    Location,
    Md5,
    Sha1,
    Sha256,
    Sha512,
}

impl Column {
    pub const ALL: [Column; 15] = [
        Column::Name,
        Column::Size,
        Column::Files,
        Column::Folders,
        Column::Modified,
        Column::Created,
        Column::Accessed,
        Column::Type,
        Column::Attributes,
        Column::Status,
        Column::Location,
        Column::Md5,
        Column::Sha1,
        Column::Sha256,
        Column::Sha512,
    ];

    pub const DEFAULT: [Column; 6] =
        [Column::Name, Column::Size, Column::Files, Column::Folders, Column::Modified, Column::Type];

    pub fn title(self) -> &'static str {
        match self {
            Column::Name => "Name",
            Column::Size => "Size",
            Column::Files => "Files",
            Column::Folders => "Folders",
            Column::Modified => "Date modified",
            Column::Created => "Date created",
            Column::Accessed => "Date accessed",
            Column::Type => "Type",
            Column::Attributes => "Attributes",
            Column::Status => "Status",
            Column::Location => "Location",
            Column::Md5 => HASH_NAMES[0],
            Column::Sha1 => HASH_NAMES[1],
            Column::Sha256 => HASH_NAMES[2],
            Column::Sha512 => HASH_NAMES[3],
        }
    }

    pub fn default_width(self) -> f32 {
        match self {
            Column::Name => 320.0,
            Column::Size => 120.0,
            Column::Files | Column::Folders => 80.0,
            Column::Modified | Column::Created | Column::Accessed => 140.0,
            Column::Type => 110.0,
            Column::Attributes | Column::Status => 80.0,
            Column::Location => 320.0,
            Column::Md5 => 250.0,
            Column::Sha1 => 300.0,
            Column::Sha256 => 450.0,
            Column::Sha512 => 800.0,
        }
    }

    pub fn right_aligned(self) -> bool {
        matches!(self, Column::Size | Column::Files | Column::Folders)
    }

    pub fn is_fixed(self) -> bool {
        matches!(self, Column::Name | Column::Size)
    }

    pub fn hash_slot(self) -> Option<usize> {
        match self {
            Column::Md5 => Some(0),
            Column::Sha1 => Some(1),
            Column::Sha256 => Some(2),
            Column::Sha512 => Some(3),
            _ => None,
        }
    }

    pub fn starts_descending(self) -> bool {
        matches!(
            self,
            Column::Size | Column::Files | Column::Folders | Column::Modified | Column::Created | Column::Accessed
        )
    }

    pub fn cell(self, item: &Item, unit: Unit) -> String {
        match self {
            Column::Name => item.name.to_string(),
            Column::Size => item.size_label(unit),
            Column::Files => item.count_label(item.file_count),
            Column::Folders => item.count_label(item.dir_count),
            Column::Modified => platform::format_time(item.times.modified),
            Column::Created => platform::format_time(item.times.created),
            Column::Accessed => platform::format_time(item.times.accessed),
            Column::Type => item.type_label(),
            Column::Attributes => item.attributes_label(),
            Column::Status => item.status_label().into(),
            Column::Location => item.parent.to_string(),
            Column::Md5 | Column::Sha1 | Column::Sha256 | Column::Sha512 => {
                item.hash_label(self.hash_slot().unwrap_or(0))
            }
        }
    }

    pub fn export_cell(self, item: &Item) -> String {
        match self {
            Column::Size => item.size.to_string(),
            Column::Files if item.is_dir => item.file_count.to_string(),
            Column::Folders if item.is_dir => item.dir_count.to_string(),
            _ => self.cell(item, Unit::Auto),
        }
    }

    pub fn export_title(self) -> &'static str {
        match self {
            Column::Size => "Size (bytes)",
            _ => self.title(),
        }
    }

    pub fn compare(self, a: &Item, b: &Item) -> Ordering {
        match self {
            Column::Name => compare_text(&a.name, &b.name),
            Column::Size => a.size.cmp(&b.size),
            Column::Files => a.file_count.cmp(&b.file_count),
            Column::Folders => a.dir_count.cmp(&b.dir_count),
            Column::Modified => a.times.modified.cmp(&b.times.modified),
            Column::Created => a.times.created.cmp(&b.times.created),
            Column::Accessed => a.times.accessed.cmp(&b.times.accessed),
            Column::Type => compare_text(a.extension(), b.extension()),
            Column::Attributes => a.attrs.cmp(&b.attrs),
            Column::Status => a.status_label().cmp(b.status_label()),
            Column::Location => compare_text(&a.parent, &b.parent),
            Column::Md5 | Column::Sha1 | Column::Sha256 | Column::Sha512 => {
                let slot = self.hash_slot().unwrap_or(0);
                a.hash_label(slot).cmp(&b.hash_label(slot))
            }
        }
    }

    pub fn groups_folders_first(self) -> bool {
        matches!(self, Column::Name | Column::Type)
    }
}

pub fn compare_text(a: &str, b: &str) -> Ordering {
    if a.is_ascii() && b.is_ascii() {
        a.bytes().map(|byte| byte.to_ascii_lowercase()).cmp(b.bytes().map(|byte| byte.to_ascii_lowercase()))
    } else {
        a.to_lowercase().cmp(&b.to_lowercase())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(name: &str) -> Item {
        Item {
            name: name.into(),
            parent: Arc::from(r"C:\data"),
            is_dir: false,
            attrs: 0,
            times: Times::default(),
            size: 2048,
            file_count: 0,
            dir_count: 0,
            state: ItemState::Ready,
            hashes: None,
        }
    }

    #[test]
    fn text_compares_ignoring_case() {
        assert_eq!(compare_text("apple", "Banana"), Ordering::Less);
        assert_eq!(compare_text("ÉCOLE", "école"), Ordering::Equal);
    }

    #[test]
    fn type_comes_from_the_extension() {
        assert_eq!(file("report.pdf").type_label(), "PDF File");
        assert_eq!(file("README").type_label(), "File");
        assert_eq!(file(".gitignore").type_label(), "File");
    }

    #[test]
    fn unfinished_sizes_read_as_lower_bounds() {
        let mut item = file("folder");
        item.is_dir = true;
        item.state = ItemState::Pending;
        item.size = 0;
        assert_eq!(Column::Size.cell(&item, Unit::Auto), "…");
        item.size = 2048;
        assert_eq!(Column::Size.cell(&item, Unit::Auto), "≥ 2.0 KB");
        item.state = ItemState::Partial;
        assert_eq!(Column::Size.cell(&item, Unit::Auto), "≥ 2.0 KB");
        item.state = ItemState::Ready;
        assert_eq!(Column::Size.cell(&item, Unit::Auto), "2.0 KB");
    }

    #[test]
    fn exports_use_raw_numbers() {
        assert_eq!(Column::Size.export_cell(&file("a")), "2048");
        assert_eq!(Column::Size.export_title(), "Size (bytes)");
    }

    #[test]
    fn attributes_read_as_letters() {
        let mut item = file("a");
        item.attrs = ATTR_READONLY | ATTR_HIDDEN | ATTR_ARCHIVE;
        assert_eq!(item.attributes_label(), "RHA");
    }
}
