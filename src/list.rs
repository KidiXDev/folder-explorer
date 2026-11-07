use crate::format::Unit;
use crate::icons::IconStore;
use crate::item::{Column, Item, ItemState};
use crate::scan::Live;
use crate::ui::FileRow;
use rayon::prelude::*;
use slint::{Image, Model, ModelNotify, ModelRc, ModelTracker, SharedString, VecModel};
use std::cell::{Ref, RefCell};
use std::cmp::Ordering;
use std::collections::{BTreeSet, HashSet};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::Ordering::Relaxed;

const RESET_THRESHOLD: usize = 512;
const NOT_SHOWN: u32 = u32::MAX;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum View {
    ExtraLarge,
    Large,
    Medium,
    Small,
    List,
    #[default]
    Details,
    Tiles,
}

impl View {
    pub const ALL: [View; 7] =
        [View::ExtraLarge, View::Large, View::Medium, View::Small, View::List, View::Details, View::Tiles];

    pub fn index(self) -> usize {
        Self::ALL.iter().position(|view| *view == self).unwrap_or(0)
    }

    fn icon_size(self) -> u32 {
        match self {
            View::ExtraLarge => 256,
            View::Large => 96,
            View::Medium | View::Tiles => 48,
            View::Small | View::List | View::Details => 16,
        }
    }

    fn shows_thumbnails(self) -> bool {
        self.icon_size() >= 48
    }
}

pub struct ListState {
    pub items: Vec<Item>,
    pub columns: Vec<Column>,
    pub unit: Unit,
    pub sort: Column,
    pub descending: bool,
    pub show_hidden: bool,
    pub view: View,
    order: Vec<u32>,
    rows: Vec<u32>,
    selected: Vec<bool>,
    cut: Vec<bool>,
    anchor: usize,
    cursor: usize,
    total_size: u64,
    largest_size: u64,
}

impl ListState {
    pub fn new(columns: Vec<Column>, unit: Unit, sort: Column, descending: bool, show_hidden: bool) -> Self {
        Self {
            items: Vec::new(),
            columns,
            unit,
            sort,
            descending,
            show_hidden,
            view: View::default(),
            order: Vec::new(),
            rows: Vec::new(),
            selected: Vec::new(),
            cut: Vec::new(),
            anchor: 0,
            cursor: 0,
            total_size: 0,
            largest_size: 0,
        }
    }

    fn set_items(&mut self, items: Vec<Item>) {
        self.selected = vec![false; items.len()];
        self.cut = vec![false; items.len()];
        self.items = items;
        self.order.clear();
        self.anchor = 0;
        self.cursor = 0;
        self.rebuild();
    }

    fn compare(&self, a: u32, b: u32) -> Ordering {
        let (left, right) = (&self.items[a as usize], &self.items[b as usize]);
        let by_column = self.sort.compare(left, right);
        let by_column = if self.descending { by_column.reverse() } else { by_column };
        let folders_first =
            if self.sort.groups_folders_first() { right.is_dir.cmp(&left.is_dir) } else { Ordering::Equal };
        folders_first.then(by_column).then_with(|| Column::Name.compare(left, right)).then(a.cmp(&b))
    }

    fn row_after_rebuild(&self, item: Option<u32>, old_row: usize) -> usize {
        match item.map(|index| self.rows[index as usize]) {
            Some(row) if row != NOT_SHOWN => row as usize,
            _ => old_row.min(self.order.len().saturating_sub(1)),
        }
    }

    pub fn rebuild(&mut self) {
        let anchored = self.order.get(self.anchor).copied();
        let cursored = self.order.get(self.cursor).copied();
        let shows = |item: &Item| self.show_hidden || !item.is_hidden();
        let mut order: Vec<u32> =
            self.items.iter().enumerate().filter(|(_, item)| shows(item)).map(|(index, _)| index as u32).collect();
        order.par_sort_unstable_by(|a, b| self.compare(*a, *b));
        self.rows = vec![NOT_SHOWN; self.items.len()];
        for (row, index) in order.iter().enumerate() {
            self.rows[*index as usize] = row as u32;
        }
        self.order = order;
        self.anchor = self.row_after_rebuild(anchored, self.anchor);
        self.cursor = self.row_after_rebuild(cursored, self.cursor);
        self.recount();
    }

    pub fn recount(&mut self) {
        let sizes = || self.order.iter().map(|index| self.items[*index as usize].size);
        self.total_size = sizes().sum();
        self.largest_size = sizes().max().unwrap_or(0);
    }

    pub fn row_count(&self) -> usize {
        self.order.len()
    }

    pub fn total_size(&self) -> u64 {
        self.total_size
    }

    fn item_at(&self, row: usize) -> Option<(usize, &Item)> {
        let index = *self.order.get(row)? as usize;
        Some((index, &self.items[index]))
    }

    pub fn item(&self, row: usize) -> Option<&Item> {
        self.item_at(row).map(|(_, item)| item)
    }

    pub fn visible_items(&self) -> impl Iterator<Item = &Item> {
        self.order.iter().map(|index| &self.items[*index as usize])
    }

    pub fn selected_items(&self) -> impl Iterator<Item = &Item> {
        self.visible_items_indexed().filter(|(index, _)| self.selected[*index]).map(|(_, item)| item)
    }

    fn visible_items_indexed(&self) -> impl Iterator<Item = (usize, &Item)> {
        self.order.iter().map(|index| (*index as usize, &self.items[*index as usize]))
    }

    pub fn only_selected(&self) -> Option<(usize, &Item)> {
        let mut selected = self.order.iter().enumerate().filter(|(_, index)| self.selected[**index as usize]);
        let (row, index) = selected.next()?;
        selected.next().is_none().then(|| (row, &self.items[*index as usize]))
    }

    pub fn select_name(&mut self, name: &str) -> Option<usize> {
        let row = self.visible_items().position(|item| &*item.name == name)?;
        self.press(row, false, false);
        Some(row)
    }

    pub fn mark_cut(&mut self, paths: &HashSet<PathBuf>) {
        self.cut = self.items.iter().map(|item| !paths.is_empty() && paths.contains(&item.path())).collect();
    }

    pub fn selected_count(&self) -> usize {
        self.selected_items().count()
    }

    pub fn is_row_selected(&self, row: usize) -> bool {
        self.item_at(row).is_some_and(|(index, _)| self.selected[index])
    }

    pub fn clear_selection(&mut self) {
        self.selected.fill(false);
    }

    pub fn press(&mut self, row: usize, toggle: bool, extend: bool) {
        if row >= self.order.len() {
            return;
        }
        match (toggle, extend) {
            (_, true) => self.select_range(row),
            (true, false) => {
                let index = self.order[row] as usize;
                self.selected[index] = !self.selected[index];
                self.anchor = row;
            }
            (false, false) => {
                self.clear_selection();
                self.selected[self.order[row] as usize] = true;
                self.anchor = row;
            }
        }
        self.cursor = row;
    }

    fn select_range(&mut self, row: usize) {
        self.clear_selection();
        let (first, last) = (self.anchor.min(row), self.anchor.max(row));
        for index in &self.order[first..=last] {
            self.selected[*index as usize] = true;
        }
    }

    pub fn select_all(&mut self) {
        for index in &self.order {
            self.selected[*index as usize] = true;
        }
    }

    pub fn invert_selection(&mut self) {
        for index in &self.order {
            let selected = &mut self.selected[*index as usize];
            *selected = !*selected;
        }
    }

    pub fn move_cursor(&mut self, delta: i32, extend: bool) -> Option<usize> {
        let last = self.order.len().checked_sub(1)?;
        let row = (self.cursor as i64 + delta as i64).clamp(0, last as i64) as usize;
        self.press(row, false, extend);
        Some(row)
    }

    fn file_row(&self, row: usize) -> Option<FileRow> {
        let (index, item) = self.item_at(row)?;
        let cells: Vec<SharedString> =
            self.columns.iter().map(|column| column.cell(item, self.unit).into()).collect();
        let ratio = if self.largest_size == 0 { 0.0 } else { item.size as f32 / self.largest_size as f32 };
        let subtitle = if self.view == View::Tiles { item.type_label().into() } else { SharedString::new() };
        Some(FileRow {
            cells: ModelRc::new(VecModel::from(cells)),
            icon: Image::default(),
            subtitle,
            folder: item.is_dir,
            selected: self.selected[index],
            ratio,
            busy: item.state == ItemState::Pending,
            warn: matches!(item.state, ItemState::Partial | ItemState::Cancelled),
            dim: item.is_hidden() || self.cut[index],
        })
    }
}

pub struct ListModel {
    state: RefCell<ListState>,
    notify: ModelNotify,
    icons: Rc<IconStore>,
    waiting_for_icons: RefCell<BTreeSet<usize>>,
}

impl ListModel {
    pub fn new(state: ListState, icons: Rc<IconStore>) -> Self {
        Self {
            state: RefCell::new(state),
            notify: ModelNotify::default(),
            icons,
            waiting_for_icons: RefCell::default(),
        }
    }

    pub fn refresh_icons(&self) {
        let rows = std::mem::take(&mut *self.waiting_for_icons.borrow_mut());
        for row in rows {
            self.notify.row_changed(row);
        }
    }

    pub fn state(&self) -> Ref<'_, ListState> {
        self.state.borrow()
    }

    pub fn set_items(&self, items: Vec<Item>) {
        self.state.borrow_mut().set_items(items);
        self.notify.reset();
    }

    pub fn update(&self, change: impl FnOnce(&mut ListState)) {
        change(&mut self.state.borrow_mut());
        self.notify.reset();
    }

    pub fn update_item(&self, index: usize, change: impl FnOnce(&mut Item)) {
        let row = {
            let mut state = self.state.borrow_mut();
            let Some(item) = state.items.get_mut(index) else { return };
            change(item);
            state.rows[index]
        };
        if row != NOT_SHOWN {
            self.notify.row_changed(row as usize);
        }
    }

    pub fn apply_live(&self, lives: &[Live]) {
        for (index, live) in lives.iter().enumerate() {
            let size = live.size.load(Relaxed);
            let stale = self.state.borrow().items.get(index).is_some_and(|item| {
                item.state == ItemState::Pending && item.size != size
            });
            if stale {
                self.update_item(index, |item| {
                    item.size = size;
                    item.file_count = live.files.load(Relaxed);
                    item.dir_count = live.dirs.load(Relaxed);
                });
            }
        }
        self.state.borrow_mut().recount();
    }

    pub fn change_selection<R>(&self, change: impl FnOnce(&mut ListState) -> R) -> R {
        let (result, changed) = {
            let mut state = self.state.borrow_mut();
            let before = state.selected.clone();
            let result = change(&mut state);
            let changed: Vec<usize> = state
                .order
                .iter()
                .enumerate()
                .filter(|(_, index)| before[**index as usize] != state.selected[**index as usize])
                .map(|(row, _)| row)
                .collect();
            (result, changed)
        };
        if changed.len() > RESET_THRESHOLD {
            self.notify.reset();
        } else {
            for row in changed {
                self.notify.row_changed(row);
            }
        }
        result
    }
}

impl Model for ListModel {
    type Data = FileRow;

    fn row_count(&self) -> usize {
        self.state.borrow().row_count()
    }

    fn row_data(&self, row: usize) -> Option<FileRow> {
        let state = self.state.borrow();
        let mut file_row = state.file_row(row)?;
        let item = state.item(row)?;
        let (icon, ready) = self.icons.item_icon(item, state.view.icon_size(), state.view.shows_thumbnails());
        file_row.icon = icon;
        if !ready {
            self.waiting_for_icons.borrow_mut().insert(row);
        }
        Some(file_row)
    }

    fn model_tracker(&self) -> &dyn ModelTracker {
        &self.notify
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::item::ATTR_HIDDEN;
    use crate::scan::Times;
    use std::path::Path;
    use std::sync::Arc;

    fn item(name: &str, is_dir: bool, size: u64, attrs: u32) -> Item {
        Item {
            name: name.into(),
            native_name: None,
            parent: Arc::from(Path::new(r"C:\data")),
            is_dir,
            attrs,
            times: Times::default(),
            size,
            file_count: 0,
            dir_count: 0,
            state: ItemState::Ready,
            hashes: None,
        }
    }

    fn state(sort: Column, descending: bool, show_hidden: bool) -> ListState {
        let mut state = ListState::new(vec![Column::Name, Column::Size], Unit::Auto, sort, descending, show_hidden);
        state.set_items(vec![
            item("banana.txt", false, 300, 0),
            item("zoo", true, 100, 0),
            item("Apple.txt", false, 200, 0),
            item("archive", true, 900, 0),
            item("secret.txt", false, 50, ATTR_HIDDEN),
        ]);
        state
    }

    fn names(state: &ListState) -> Vec<&str> {
        state.visible_items().map(|item| &*item.name).collect()
    }

    #[test]
    fn name_sort_keeps_folders_first_in_both_directions() {
        assert_eq!(names(&state(Column::Name, false, false)), ["archive", "zoo", "Apple.txt", "banana.txt"]);
        assert_eq!(names(&state(Column::Name, true, false)), ["zoo", "archive", "banana.txt", "Apple.txt"]);
    }

    #[test]
    fn size_sort_mixes_files_and_folders() {
        assert_eq!(names(&state(Column::Size, true, false)), ["archive", "banana.txt", "Apple.txt", "zoo"]);
    }

    #[test]
    fn hidden_items_appear_only_when_asked_for() {
        assert_eq!(state(Column::Name, false, false).row_count(), 4);
        assert_eq!(state(Column::Name, false, true).row_count(), 5);
    }

    #[test]
    fn totals_only_count_visible_rows() {
        assert_eq!(state(Column::Name, false, false).total_size(), 1500);
        assert_eq!(state(Column::Name, false, true).total_size(), 1550);
    }

    #[test]
    fn a_plain_press_replaces_the_selection() {
        let mut list = state(Column::Name, false, false);
        list.press(0, false, false);
        list.press(2, false, false);
        assert_eq!(list.selected_items().map(|item| &*item.name).collect::<Vec<_>>(), ["Apple.txt"]);
    }

    #[test]
    fn control_toggles_single_rows() {
        let mut list = state(Column::Name, false, false);
        list.press(0, true, false);
        list.press(2, true, false);
        assert_eq!(list.selected_count(), 2);
        list.press(0, true, false);
        assert_eq!(list.selected_count(), 1);
    }

    #[test]
    fn shift_selects_from_the_anchor_in_either_direction() {
        let mut list = state(Column::Name, false, false);
        list.press(1, false, false);
        list.press(3, false, true);
        assert_eq!(list.selected_count(), 3);
        list.press(0, false, true);
        assert_eq!(list.selected_count(), 2);
    }

    #[test]
    fn hiding_the_anchor_row_keeps_shift_selection_inside_the_list() {
        let mut list = state(Column::Name, false, true);
        list.press(4, false, false);
        list.show_hidden = false;
        list.rebuild();
        list.press(0, false, true);
        assert_eq!(list.selected_count(), 4);
    }

    #[test]
    fn the_cursor_stays_usable_after_its_row_is_hidden() {
        let mut list = state(Column::Name, false, true);
        list.press(4, false, false);
        list.show_hidden = false;
        list.rebuild();
        assert_eq!(list.move_cursor(-1, false), Some(2));
    }

    #[test]
    fn the_anchor_follows_its_item_through_a_resort() {
        let mut list = state(Column::Name, false, false);
        list.press(1, false, false);
        list.sort = Column::Size;
        list.descending = true;
        list.rebuild();
        assert_eq!(names(&list), ["archive", "banana.txt", "Apple.txt", "zoo"]);
        list.press(1, false, true);
        let selected: Vec<_> = list.selected_items().map(|item| &*item.name).collect();
        assert_eq!(selected, ["banana.txt", "Apple.txt", "zoo"]);
    }

    #[test]
    fn replacing_the_items_resets_the_anchor() {
        let mut list = state(Column::Name, false, false);
        list.press(3, false, false);
        list.set_items(vec![item("only.txt", false, 1, 0)]);
        list.press(0, false, true);
        assert_eq!(list.selected_count(), 1);
    }

    #[test]
    fn select_all_skips_hidden_rows_and_inverse_flips() {
        let mut list = state(Column::Name, false, false);
        list.select_all();
        assert_eq!(list.selected_count(), 4);
        list.press(0, false, false);
        list.invert_selection();
        assert_eq!(list.selected_count(), 3);
    }

    #[test]
    fn the_cursor_stops_at_the_ends() {
        let mut list = state(Column::Name, false, false);
        assert_eq!(list.move_cursor(-5, false), Some(0));
        assert_eq!(list.move_cursor(99, false), Some(3));
        assert_eq!(list.selected_count(), 1);
        assert!(list.is_row_selected(3));
    }

    #[test]
    fn an_empty_list_has_no_cursor() {
        let mut list = ListState::new(vec![Column::Name], Unit::Auto, Column::Name, false, false);
        assert_eq!(list.move_cursor(1, false), None);
    }

    #[test]
    fn pressing_past_the_end_is_ignored() {
        let mut list = state(Column::Name, false, false);
        list.press(50, false, false);
        assert_eq!(list.selected_count(), 0);
    }

    #[test]
    fn the_only_selected_row_is_reported_only_when_alone() {
        let mut list = state(Column::Name, false, false);
        assert!(list.only_selected().is_none());
        list.press(2, false, false);
        assert_eq!(list.only_selected().map(|(row, item)| (row, &*item.name)), Some((2, "Apple.txt")));
        list.press(3, true, false);
        assert!(list.only_selected().is_none());
    }

    #[test]
    fn items_can_be_selected_by_name() {
        let mut list = state(Column::Name, false, false);
        assert_eq!(list.select_name("zoo"), Some(1));
        assert!(list.is_row_selected(1));
        assert_eq!(list.select_name("missing"), None);
    }

    #[test]
    fn cut_items_are_dimmed_and_clearing_the_set_restores_them() {
        let mut list = state(Column::Name, false, false);
        let cut: HashSet<PathBuf> = [PathBuf::from(r"C:\data\zoo")].into();
        list.mark_cut(&cut);
        assert!(list.file_row(1).unwrap().dim);
        assert!(!list.file_row(0).unwrap().dim);
        list.mark_cut(&HashSet::new());
        assert!(!list.file_row(1).unwrap().dim);
    }

    #[test]
    fn tiles_describe_the_item_type() {
        let mut list = state(Column::Name, false, false);
        assert_eq!(list.file_row(0).unwrap().subtitle, "");
        list.view = View::Tiles;
        assert_eq!(list.file_row(0).unwrap().subtitle, "Folder");
        assert_eq!(list.file_row(2).unwrap().subtitle, "TXT File");
    }

    #[test]
    fn only_larger_views_show_thumbnails() {
        assert!(View::ExtraLarge.shows_thumbnails() && View::Medium.shows_thumbnails() && View::Tiles.shows_thumbnails());
        assert!(!View::Small.shows_thumbnails() && !View::List.shows_thumbnails() && !View::Details.shows_thumbnails());
    }

    #[test]
    fn selection_follows_items_through_a_resort() {
        let mut list = state(Column::Name, false, false);
        list.press(2, false, false);
        list.sort = Column::Size;
        list.descending = true;
        list.rebuild();
        assert_eq!(list.selected_items().next().unwrap().name.as_ref(), "Apple.txt");
        assert!(list.is_row_selected(2));
    }
}
