use crate::icons::{IconStore, THIS_PC};
use crate::item::compare_text;
use crate::platform::Drive;
use crate::ui::TreeRow;
use slint::{Image, Model, ModelRc, SharedString, VecModel};
use std::cell::Cell;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::rc::Rc;

const KIND_COMPUTER: i32 = 0;
const KIND_DRIVE: i32 = 1;
const KIND_FOLDER: i32 = 2;
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
    drives: Vec<(PathBuf, String)>,
    closed: HashSet<PathBuf>,
    computer_closed: bool,
    model: Rc<VecModel<TreeRow>>,
    selected: Option<PathBuf>,
    computer_selected: bool,
    target: Option<PathBuf>,
    scroll_pending: bool,
    icons: Rc<IconStore>,
    missing_icons: Cell<bool>,
}

impl Sidebar {
    pub fn new(icons: Rc<IconStore>) -> Self {
        let computer = Node {
            path: None,
            name: "This PC".into(),
            depth: 0,
            kind: KIND_COMPUTER,
            attrs: 0,
            expanded: true,
            loaded: true,
        };
        let sidebar = Self {
            nodes: vec![computer],
            drives: Vec::new(),
            closed: HashSet::new(),
            computer_closed: false,
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
            None => self.icons.location_icon(THIS_PC, ICON_SIZE),
            Some(path) if node.kind == KIND_DRIVE => self.icons.location_icon(&path.to_string_lossy(), ICON_SIZE),
            Some(path) => self.icons.folder_icon(path, node.attrs, ICON_SIZE),
        };
        if icon.is_none() {
            self.missing_icons.set(true);
        }
        icon.unwrap_or_default()
    }

    fn selected_row(&self) -> Option<usize> {
        if self.computer_selected {
            return Some(0);
        }
        let selected = self.selected.as_ref()?;
        if self.computer_closed {
            return Some(0);
        }
        (0..self.nodes.len())
            .filter(|&row| self.nodes[row].path.as_ref().is_some_and(|path| selected.starts_with(path)))
            .max_by_key(|&row| self.nodes[row].depth)
    }

    fn row(&self, index: usize) -> TreeRow {
        let node = &self.nodes[index];
        let selected = self.selected_row() == Some(index);
        let has_children = if node.kind == KIND_COMPUTER {
            !self.drives.is_empty()
        } else {
            self.nodes.get(index + 1).is_some_and(|next| next.depth > node.depth)
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

    pub fn set_drives(&mut self, drives: &[Drive]) -> Option<PathBuf> {
        self.drives = drives.iter().map(|drive| (drive.root.clone(), drive.title.clone())).collect();
        self.nodes.retain(|node| node.kind == KIND_COMPUTER);
        if !self.computer_closed {
            self.nodes[0].expanded = true;
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
            self.nodes[0].expanded = false;
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
        self.nodes.get(row).map(|node| node.path.clone())
    }

    pub fn toggle(&mut self, row: usize) -> Option<PathBuf> {
        let node = self.nodes.get(row)?;
        if node.kind == KIND_COMPUTER {
            self.target = None;
            if node.expanded {
                self.computer_closed = true;
                self.collapse(row);
            } else {
                self.computer_closed = false;
                self.expand_computer();
            }
            return None;
        }
        let path = node.path.clone()?;
        self.target = None;
        if node.expanded {
            self.closed.insert(path);
            self.collapse(row);
            return None;
        }
        self.closed.remove(&path);
        Some(path)
    }

    fn expand_computer(&mut self) {
        self.nodes[0].expanded = true;
        self.nodes[0].loaded = true;
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
        self.nodes.splice(1..1, children);
        if synced {
            self.model.set_row_data(0, self.row(0));
            for index in 1..=count {
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
        for index in row..end {
            if self.nodes[index].expanded
                && let Some(path) = &self.nodes[index].path
            {
                self.closed.insert(path.clone());
            }
        }
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
        let Some(row) = self.nodes.iter().position(|node| node.path.as_deref() == Some(parent)) else {
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
        if self.computer_closed {
            self.target = None;
            return None;
        }
        let target = self.target.clone()?;
        let row = (0..self.nodes.len())
            .filter(|row| self.nodes[*row].path.as_ref().is_some_and(|path| target.starts_with(path)))
            .max_by_key(|row| self.nodes[*row].depth)?;
        let node = &self.nodes[row];
        let closed_by_user = node.path.as_ref().is_some_and(|path| self.closed.contains(path));
        if node.path.as_deref() == Some(target.as_path()) || (node.loaded && node.expanded) || closed_by_user {
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

    #[test]
    fn closed_branch_is_not_forced_open_when_navigating_inside_it() {
        let mut sidebar = Sidebar::new(IconStore::new());
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
        let mut sidebar = Sidebar::new(IconStore::new());
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
