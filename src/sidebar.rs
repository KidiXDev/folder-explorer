use crate::icons::{IconStore, THIS_PC};
use crate::item::compare_text;
use crate::platform::{Drive, QUICK_ACCESS};
use crate::settings::TreeMemory;
use crate::ui::TreeRow;
use slint::{Image, Model, ModelRc, SharedString, VecModel};
use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::rc::Rc;

const KIND_COMPUTER: i32 = 0;
const KIND_DRIVE: i32 = 1;
const KIND_FOLDER: i32 = 2;
const KIND_PIN: i32 = 3;
const KIND_QUICK_ACCESS: i32 = 4;
const ICON_SIZE: u32 = 16;

struct Node {
    path: Option<PathBuf>,
    name: String,
    depth: i32,
    kind: i32,
    attrs: u32,
    expanded: bool,
    loaded: bool,
}

pub struct Sidebar {
    nodes: Vec<Node>,
    root: usize,
    pins: Vec<(PathBuf, String)>,
    drives: Vec<(PathBuf, String)>,
    memory: TreeMemory,
    model: Rc<VecModel<TreeRow>>,
    selected: Option<PathBuf>,
    computer_selected: bool,
    target: Option<PathBuf>,
    scroll_pending: bool,
    icons: Rc<IconStore>,
    missing_icons: Cell<bool>,
}

impl Sidebar {
    pub fn new(icons: Rc<IconStore>, memory: TreeMemory) -> Self {
        let computer = Node {
            path: None,
            name: "This PC".into(),
            depth: 0,
            kind: KIND_COMPUTER,
            attrs: 0,
            expanded: !memory.computer_closed,
            loaded: true,
        };
        let sidebar = Self {
            nodes: vec![computer],
            root: 0,
            pins: Vec::new(),
            drives: Vec::new(),
            memory,
            model: Rc::new(VecModel::default()),
            selected: None,
            computer_selected: true,
            target: None,
            scroll_pending: false,
            icons,
            missing_icons: Cell::new(false),
        };
        sidebar.sync();
        sidebar
    }

    pub fn model(&self) -> ModelRc<TreeRow> {
        ModelRc::from(self.model.clone())
    }

    pub fn memory(&self) -> TreeMemory {
        self.memory
    }

    fn sync(&self) {
        self.missing_icons.set(false);
        let rows: Vec<TreeRow> = (0..self.nodes.len()).map(|index| self.row(index)).collect();
        if self.model.row_count() == rows.len() {
            for (index, row) in rows.into_iter().enumerate() {
                self.model.set_row_data(index, row);
            }
        } else {
            self.model.set_vec(rows);
        }
    }

    pub fn refresh_icons(&self) {
        if self.missing_icons.get() {
            self.sync();
        }
    }

    fn icon(&self, node: &Node) -> Image {
        let icon = match &node.path {
            None if node.kind == KIND_QUICK_ACCESS => self.icons.location_icon(QUICK_ACCESS, ICON_SIZE),
            None => self.icons.location_icon(THIS_PC, ICON_SIZE),
            Some(path) if matches!(node.kind, KIND_DRIVE | KIND_PIN) => {
                self.icons.location_icon(&path.to_string_lossy(), ICON_SIZE)
            }
            Some(path) => self.icons.folder_icon(path, node.attrs, ICON_SIZE),
        };
        if icon.is_none() {
            self.missing_icons.set(true);
        }
        icon.unwrap_or_default()
    }

    fn selected_row(&self) -> Option<usize> {
        if self.computer_selected {
            return Some(self.root);
        }
        let selected = self.selected.as_ref()?;
        if let Some(pin) = (0..self.root).find(|&row| self.nodes[row].path.as_ref() == Some(selected)) {
            return Some(pin);
        }
        if self.memory.computer_closed {
            return Some(self.root);
        }
        (self.root..self.nodes.len())
            .filter(|&row| self.nodes[row].path.as_ref().is_some_and(|path| selected.starts_with(path)))
            .max_by_key(|&row| self.nodes[row].depth)
    }

    fn row(&self, index: usize) -> TreeRow {
        let node = &self.nodes[index];
        let selected = self.selected_row() == Some(index);
        let has_children = match node.kind {
            KIND_COMPUTER => !self.drives.is_empty(),
            KIND_QUICK_ACCESS => true,
            _ => self.nodes.get(index + 1).is_some_and(|next| next.depth > node.depth),
        };
        TreeRow {
            name: SharedString::from(&node.name),
            depth: node.depth,
            expanded: node.expanded,
            expandable: !node.loaded || has_children,
            selected,
            kind: node.kind,
            icon: self.icon(node),
        }
    }

    pub fn set_pins(&mut self, pins: Vec<(PathBuf, String)>) {
        if self.pins != pins {
            self.pins = pins;
            self.show_pins();
            self.scroll_pending = true;
        }
    }

    fn show_pins(&mut self) {
        let header = (!self.pins.is_empty()).then(|| Node {
            path: None,
            name: "Quick access".into(),
            depth: 0,
            kind: KIND_QUICK_ACCESS,
            attrs: 0,
            expanded: !self.memory.quick_access_closed,
            loaded: true,
        });
        let shown: &[(PathBuf, String)] = if self.memory.quick_access_closed { &[] } else { &self.pins };
        let pins = shown.iter().map(|(path, name)| Node {
            path: Some(path.clone()),
            name: name.clone(),
            depth: 1,
            kind: KIND_PIN,
            attrs: 0,
            expanded: false,
            loaded: true,
        });
        let rows: Vec<Node> = header.into_iter().chain(pins).collect();
        let root = rows.len();
        self.nodes.splice(..self.root, rows);
        self.root = root;
        self.sync();
    }

    pub fn set_drives(&mut self, drives: &[Drive]) -> Option<PathBuf> {
        let drives: Vec<(PathBuf, String)> =
            drives.iter().map(|drive| (drive.root.clone(), drive.title.clone())).collect();
        if drives == self.drives {
            return None;
        }
        self.drives = drives;
        self.nodes.truncate(self.root + 1);
        if !self.memory.computer_closed {
            self.nodes[self.root].expanded = true;
            self.nodes.extend(self.drives.iter().map(|(root, title)| Node {
                path: Some(root.clone()),
                name: title.clone(),
                depth: 1,
                kind: KIND_DRIVE,
                attrs: 0,
                expanded: false,
                loaded: false,
            }));
        } else {
            self.nodes[self.root].expanded = false;
        }
        let request = self.advance();
        self.sync();
        request
    }

    pub fn take_scroll_row(&mut self) -> Option<usize> {
        if !std::mem::take(&mut self.scroll_pending) {
            return None;
        }
        self.nodes.iter().position(|node| node.path.is_some() && node.path == self.selected)
    }

    pub fn path_at(&self, row: usize) -> Option<Option<PathBuf>> {
        self.nodes.get(row).filter(|node| node.kind != KIND_QUICK_ACCESS).map(|node| node.path.clone())
    }

    pub fn toggle(&mut self, row: usize) -> Option<PathBuf> {
        let node = self.nodes.get(row)?;
        if node.kind == KIND_QUICK_ACCESS {
            self.memory.quick_access_closed = !self.memory.quick_access_closed;
            self.show_pins();
            return None;
        }
        self.target = None;
        self.memory.auto_expand_off = node.expanded;
        if node.kind == KIND_COMPUTER {
            if node.expanded {
                self.memory.computer_closed = true;
                self.collapse(row);
            } else {
                self.memory.computer_closed = false;
                self.expand_computer();
            }
            return None;
        }
        let path = node.path.clone()?;
        if node.expanded {
            self.collapse(row);
            return None;
        }
        Some(path)
    }

    fn expand_computer(&mut self) {
        let root = self.root;
        self.nodes[root].expanded = true;
        self.nodes[root].loaded = true;
        let children: Vec<Node> = self
            .drives
            .iter()
            .map(|(root, title)| Node {
                path: Some(root.clone()),
                name: title.clone(),
                depth: 1,
                kind: KIND_DRIVE,
                attrs: 0,
                expanded: false,
                loaded: false,
            })
            .collect();
        let count = children.len();
        let synced = self.model.row_count() == self.nodes.len();
        self.nodes.splice(root + 1..root + 1, children);
        if synced {
            self.model.set_row_data(root, self.row(root));
            for index in root + 1..=root + count {
                self.model.insert(index, self.row(index));
            }
        } else {
            self.sync();
        }
    }

    fn collapse(&mut self, row: usize) {
        let before_len = self.nodes.len();
        let depth = self.nodes[row].depth;
        let end = self.nodes[row + 1..]
            .iter()
            .position(|node| node.depth <= depth)
            .map_or(self.nodes.len(), |offset| row + 1 + offset);
        self.nodes.drain(row + 1..end);
        self.nodes[row].expanded = false;
        if self.nodes[row].kind != KIND_COMPUTER {
            self.nodes[row].loaded = false;
        }
        if self.model.row_count() == before_len {
            for _ in row + 1..end {
                self.model.remove(row + 1);
            }
            self.model.set_row_data(row, self.row(row));
        } else {
            self.sync();
        }
    }

    pub fn is_expanded(&self, path: &Path) -> bool {
        self.nodes.iter().any(|node| node.expanded && node.path.as_deref() == Some(path))
    }

    pub fn set_children(&mut self, parent: &Path, mut folders: Vec<(String, u32)>) -> Option<PathBuf> {
        let Some(row) = (self.root..self.nodes.len()).find(|&row| self.nodes[row].path.as_deref() == Some(parent)) else {
            return self.advance();
        };
        if self.nodes[row].expanded {
            self.collapse(row);
        }
        let before_len = self.nodes.len();
        folders.sort_by(|(a, _), (b, _)| compare_text(a, b));
        let depth = self.nodes[row].depth + 1;
        let children: Vec<Node> = folders
            .into_iter()
            .map(|(name, attrs)| Node {
                path: Some(parent.join(&name)),
                name,
                depth,
                kind: KIND_FOLDER,
                attrs,
                expanded: false,
                loaded: false,
            })
            .collect();
        let count = children.len();
        self.nodes.splice(row + 1..row + 1, children);
        self.nodes[row].expanded = true;
        self.nodes[row].loaded = true;
        let request = self.advance();
        if self.model.row_count() == before_len {
            self.model.set_row_data(row, self.row(row));
            for index in row + 1..row + 1 + count {
                self.model.insert(index, self.row(index));
            }
        } else {
            self.sync();
        }
        request
    }

    pub fn reveal(&mut self, path: Option<&Path>) -> Option<PathBuf> {
        self.computer_selected = path.is_none();
        self.selected = path.map(Path::to_path_buf);
        self.target = path.map(Path::to_path_buf);
        let request = self.advance();
        self.sync();
        request
    }

    fn advance(&mut self) -> Option<PathBuf> {
        let target = self.target.clone()?;
        let row = (self.root..self.nodes.len())
            .filter(|row| self.nodes[*row].path.as_ref().is_some_and(|path| target.starts_with(path)))
            .max_by_key(|row| self.nodes[*row].depth)?;
        let node = &self.nodes[row];
        if self.memory.auto_expand_off || node.path.as_deref() == Some(target.as_path()) || (node.loaded && node.expanded) {
            self.target = None;
            self.scroll_pending = true;
            return None;
        }
        node.path.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn drive(root: &str, title: &str) -> Drive {
        Drive {
            root: PathBuf::from(root),
            title: title.into(),
            usage: None,
        }
    }

    fn sidebar_with_pin(path: &str) -> Sidebar {
        let mut sidebar = Sidebar::new(IconStore::new(), TreeMemory::default());
        sidebar.set_drives(&[drive(r"C:\", "Acer (C:)")]);
        sidebar.set_pins(vec![(PathBuf::from(path), "Pinned".into())]);
        sidebar
    }

    fn two_drives() -> [Drive; 2] {
        [drive(r"C:\", "Acer (C:)"), drive(r"D:\", "Data (D:)")]
    }

    fn sidebar_with_two_drives(memory: TreeMemory) -> Sidebar {
        let mut sidebar = Sidebar::new(IconStore::new(), memory);
        sidebar.set_drives(&two_drives());
        sidebar
    }

    const AUTO_EXPAND_OFF: TreeMemory = TreeMemory { auto_expand_off: true, computer_closed: false, quick_access_closed: false };

    #[test]
    fn a_manual_collapse_stops_auto_expanding_on_every_drive() {
        let mut sidebar = sidebar_with_two_drives(TreeMemory::default());
        assert_eq!(sidebar.reveal(Some(Path::new(r"C:\Users"))), Some(PathBuf::from(r"C:\")));
        sidebar.set_children(Path::new(r"C:\"), vec![("Users".into(), 0)]);
        assert_eq!(sidebar.toggle(1), None);
        assert_eq!(sidebar.reveal(Some(Path::new(r"D:\Games"))), None);
        assert!(sidebar.memory().auto_expand_off);
    }

    #[test]
    fn a_manual_expand_turns_auto_expanding_back_on() {
        let mut sidebar = sidebar_with_two_drives(AUTO_EXPAND_OFF);
        assert_eq!(sidebar.toggle(2), Some(PathBuf::from(r"D:\")));
        assert_eq!(sidebar.reveal(Some(Path::new(r"C:\Users"))), Some(PathBuf::from(r"C:\")));
        assert!(!sidebar.memory().auto_expand_off);
    }

    #[test]
    fn a_remembered_collapse_still_applies_after_a_restart() {
        let mut sidebar = sidebar_with_two_drives(AUTO_EXPAND_OFF);
        assert_eq!(sidebar.reveal(Some(Path::new(r"C:\Users"))), None);
        assert_eq!(sidebar.model().row_count(), 3);
    }

    #[test]
    fn remembered_sections_start_collapsed() {
        let memory = TreeMemory { auto_expand_off: true, computer_closed: true, quick_access_closed: true };
        let mut sidebar = sidebar_with_two_drives(memory);
        sidebar.set_pins(vec![(PathBuf::from(r"C:\Users"), "Users".into())]);
        assert_eq!(sidebar.model().row_count(), 2);
    }

    #[test]
    fn collapsing_quick_access_leaves_auto_expanding_on() {
        let mut sidebar = sidebar_with_pin(r"C:\Users\Me");
        assert_eq!(sidebar.toggle(0), None);
        assert!(sidebar.memory().quick_access_closed);
        assert!(!sidebar.memory().auto_expand_off);
    }

    #[test]
    fn returning_to_this_pc_keeps_the_tree_expanded() {
        let mut sidebar = sidebar_with_two_drives(TreeMemory::default());
        sidebar.reveal(Some(Path::new(r"C:\Users")));
        sidebar.set_children(Path::new(r"C:\"), vec![("Users".into(), 0)]);
        sidebar.reveal(None);
        assert_eq!(sidebar.set_drives(&two_drives()), None);
        assert!(sidebar.is_expanded(Path::new(r"C:\")));
        assert_eq!(sidebar.model().row_count(), 4);
    }

    #[test]
    fn pins_sit_above_this_pc_and_only_they_open_a_folder() {
        let sidebar = sidebar_with_pin(r"C:\Users\Me");
        assert_eq!(sidebar.model().row_count(), 4);
        assert_eq!(sidebar.path_at(0), None);
        assert_eq!(sidebar.path_at(1), Some(Some(PathBuf::from(r"C:\Users\Me"))));
        assert_eq!(sidebar.path_at(2), Some(None));
        assert_eq!(sidebar.path_at(3), Some(Some(PathBuf::from(r"C:\"))));
    }

    #[test]
    fn no_pins_means_no_quick_access_section() {
        let mut sidebar = sidebar_with_pin(r"C:\Users\Me");
        sidebar.set_pins(Vec::new());
        assert_eq!(sidebar.model().row_count(), 2);
        assert_eq!(sidebar.path_at(0), Some(None));
    }

    #[test]
    fn a_pin_does_not_stop_the_tree_expanding_to_the_same_folder() {
        let mut sidebar = sidebar_with_pin(r"C:\Users\Me");
        assert_eq!(sidebar.reveal(Some(Path::new(r"C:\Users\Me"))), Some(PathBuf::from(r"C:\")));
        assert_eq!(
            sidebar.set_children(Path::new(r"C:\"), vec![("Users".into(), 0)]),
            Some(PathBuf::from(r"C:\Users"))
        );
        assert_eq!(sidebar.set_children(Path::new(r"C:\Users"), vec![("Me".into(), 0)]), None);
        assert!(sidebar.is_expanded(Path::new(r"C:\Users")));
    }

    #[test]
    fn children_of_a_pinned_folder_go_under_the_tree_row_not_the_pin() {
        let mut sidebar = sidebar_with_pin(r"C:\Users");
        sidebar.reveal(Some(Path::new(r"C:\Users\Me")));
        sidebar.set_children(Path::new(r"C:\"), vec![("Users".into(), 0)]);
        sidebar.set_children(Path::new(r"C:\Users"), vec![("Me".into(), 0)]);
        assert!(!sidebar.nodes[1].expanded);
        assert_eq!(sidebar.nodes[2].kind, KIND_COMPUTER);
        assert_eq!(sidebar.nodes.last().unwrap().depth, 3);
    }

    #[test]
    fn a_pinned_folder_highlights_its_pin_and_a_subfolder_highlights_the_tree() {
        let mut sidebar = sidebar_with_pin(r"C:\Users\Me");
        sidebar.reveal(Some(Path::new(r"C:\Users\Me")));
        assert_eq!(sidebar.selected_row(), Some(1));
        sidebar.reveal(Some(Path::new(r"C:\Users\Me\Documents")));
        assert_eq!(sidebar.selected_row(), Some(3));
    }

    #[test]
    fn quick_access_collapses_and_the_tree_keeps_working() {
        let mut sidebar = sidebar_with_pin(r"C:\Users\Me");
        assert_eq!(sidebar.toggle(0), None);
        assert_eq!(sidebar.model().row_count(), 3);
        assert_eq!(sidebar.path_at(1), Some(None));

        assert_eq!(sidebar.toggle(1), None);
        assert_eq!(sidebar.model().row_count(), 2);
        assert_eq!(sidebar.toggle(1), None);
        assert_eq!(sidebar.model().row_count(), 3);

        assert_eq!(sidebar.toggle(0), None);
        assert_eq!(sidebar.model().row_count(), 4);
        assert_eq!(sidebar.path_at(1), Some(Some(PathBuf::from(r"C:\Users\Me"))));
    }

    #[test]
    fn closed_branch_is_not_forced_open_when_navigating_inside_it() {
        let mut sidebar = Sidebar::new(IconStore::new(), TreeMemory::default());
        sidebar.set_drives(&[drive(r"C:\", "Acer (C:)")]);

        // Auto-expand when opening C:\Users\Me initially
        assert_eq!(sidebar.reveal(Some(Path::new(r"C:\Users\Me"))), Some(PathBuf::from(r"C:\")));
        assert_eq!(
            sidebar.set_children(Path::new(r"C:\"), vec![("Users".into(), 0)]),
            Some(PathBuf::from(r"C:\Users"))
        );
        assert_eq!(sidebar.set_children(Path::new(r"C:\Users"), vec![("Me".into(), 0)]), None);
        assert!(sidebar.is_expanded(Path::new(r"C:\")));
        assert!(sidebar.is_expanded(Path::new(r"C:\Users")));

        // User collapses C:\ in the sidebar
        assert_eq!(sidebar.toggle(1), None);
        assert!(!sidebar.is_expanded(Path::new(r"C:\")));

        // Opening another folder inside C:\ does NOT force C:\ to expand again
        assert_eq!(sidebar.reveal(Some(Path::new(r"C:\Users\Me\Documents"))), None);
        assert!(!sidebar.is_expanded(Path::new(r"C:\")));

        // Explicitly toggling C:\ open with the chevron re-enables expansion for C:\
        assert_eq!(sidebar.toggle(1), Some(PathBuf::from(r"C:\")));
    }

    #[test]
    fn computer_root_can_be_collapsed_and_stays_closed_on_navigation() {
        let mut sidebar = Sidebar::new(IconStore::new(), TreeMemory::default());
        sidebar.set_drives(&[drive(r"C:\", "Acer (C:)")]);
        assert_eq!(sidebar.model().row_count(), 2);

        // Collapse "This PC"
        assert_eq!(sidebar.toggle(0), None);
        assert_eq!(sidebar.model().row_count(), 1);

        // Navigating to a folder does not force "This PC" open
        assert_eq!(sidebar.reveal(Some(Path::new(r"C:\Users"))), None);
        assert_eq!(sidebar.model().row_count(), 1);

        // Expanding "This PC" restores the drives
        assert_eq!(sidebar.toggle(0), None);
        assert_eq!(sidebar.model().row_count(), 2);
    }
}
