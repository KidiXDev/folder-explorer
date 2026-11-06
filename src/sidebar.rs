use crate::icons::{IconStore, THIS_PC};
use crate::item::compare_text;
use crate::platform::Drive;
use crate::ui::TreeRow;
use slint::{Image, ModelRc, SharedString, VecModel};
use std::cell::Cell;
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
        self.model.set_vec(rows);
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

    fn row(&self, index: usize) -> TreeRow {
        let node = &self.nodes[index];
        let selected = match &node.path {
            Some(path) => self.selected.as_ref() == Some(path),
            None => self.computer_selected,
        };
        let has_children = self.nodes.get(index + 1).is_some_and(|next| next.depth > node.depth);
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
        self.nodes.retain(|node| node.kind == KIND_COMPUTER);
        self.nodes.extend(drives.iter().map(|drive| Node {
            path: Some(drive.root.clone()),
            name: drive.title.clone(),
            depth: 1,
            kind: KIND_DRIVE,
            attrs: 0,
            expanded: false,
            loaded: false,
        }));
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
            return None;
        }
        if node.expanded {
            self.collapse(row);
            self.sync();
            return None;
        }
        node.path.clone()
    }

    fn collapse(&mut self, row: usize) {
        let depth = self.nodes[row].depth;
        let end = self.nodes[row + 1..]
            .iter()
            .position(|node| node.depth <= depth)
            .map_or(self.nodes.len(), |offset| row + 1 + offset);
        self.nodes.drain(row + 1..end);
        self.nodes[row].expanded = false;
        self.nodes[row].loaded = false;
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
        folders.sort_by(|(a, _), (b, _)| compare_text(a, b));
        let depth = self.nodes[row].depth + 1;
        let children = folders.into_iter().map(|(name, attrs)| Node {
            path: Some(parent.join(&name)),
            name,
            depth,
            kind: KIND_FOLDER,
            attrs,
            expanded: false,
            loaded: false,
        });
        self.nodes.splice(row + 1..row + 1, children);
        self.nodes[row].expanded = true;
        self.nodes[row].loaded = true;
        let request = self.advance();
        self.sync();
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
        let row = (0..self.nodes.len())
            .filter(|row| self.nodes[*row].path.as_ref().is_some_and(|path| target.starts_with(path)))
            .max_by_key(|row| self.nodes[*row].depth)?;
        let node = &self.nodes[row];
        if node.path.as_deref() == Some(target.as_path()) || node.loaded && node.expanded {
            self.target = None;
            self.scroll_pending = true;
            return None;
        }
        node.path.clone()
    }
}
