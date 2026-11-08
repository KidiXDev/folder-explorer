use crate::format::Unit;
use crate::item::Column;
use crate::list::View;
use crate::platform::WindowPlacement;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

pub fn app_folder() -> Option<PathBuf> {
    Some(PathBuf::from(std::env::var_os("APPDATA")?).join("FolderExplorer"))
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct TreeMemory {
    pub auto_expand_off: bool,
    pub computer_closed: bool,
    pub quick_access_closed: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Settings {
    pub unit: Unit,
    pub show_hidden: bool,
    pub summary_open: bool,
    pub columns: Vec<Column>,
    pub widths: HashMap<usize, f32>,
    pub sort: Column,
    pub descending: bool,
    pub sidebar_width: f32,
    pub placement: Option<WindowPlacement>,
    pub tree: TreeMemory,
    folder_views: HashMap<String, View>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            unit: Unit::Auto,
            show_hidden: true,
            summary_open: false,
            columns: Column::DEFAULT.to_vec(),
            widths: HashMap::new(),
            sort: Column::Name,
            descending: false,
            sidebar_width: 240.0,
            placement: None,
            tree: TreeMemory::default(),
            folder_views: HashMap::new(),
        }
    }
}

fn column_id(column: Column) -> usize {
    Column::ALL.iter().position(|known| *known == column).unwrap_or(0)
}

fn folder_key(folder: &Path) -> String {
    folder.to_string_lossy().to_lowercase()
}

impl Settings {
    pub fn width_of(&self, column: Column) -> f32 {
        self.widths.get(&column_id(column)).copied().unwrap_or(column.default_width())
    }

    pub fn set_width(&mut self, column: Column, width: f32) {
        self.widths.insert(column_id(column), width);
    }

    pub fn view_of(&self, folder: &Path) -> View {
        self.folder_views.get(&folder_key(folder)).copied().unwrap_or_default()
    }

    pub fn set_view_of(&mut self, folder: &Path, view: View) {
        if view == View::default() {
            self.folder_views.remove(&folder_key(folder));
        } else {
            self.folder_views.insert(folder_key(folder), view);
        }
    }

    fn file() -> Option<PathBuf> {
        Some(app_folder()?.join("settings.ini"))
    }

    pub fn load() -> Self {
        Self::file().and_then(|file| fs::read_to_string(file).ok()).map(|text| Self::parse(&text)).unwrap_or_default()
    }

    pub fn save(&self) {
        let Some(file) = Self::file() else { return };
        if let Some(folder) = file.parent() {
            let _ = fs::create_dir_all(folder);
        }
        let _ = fs::write(file, self.serialize());
    }

    fn parse(text: &str) -> Self {
        let mut settings = Self::default();
        for (key, value) in text.lines().filter_map(|line| line.split_once('=')) {
            settings.apply(key.trim(), value.trim());
        }
        settings
    }

    fn apply(&mut self, key: &str, value: &str) {
        match key {
            "unit" => {
                if let Some(unit) = value.parse().ok().and_then(|index: usize| Unit::ALL.get(index)) {
                    self.unit = *unit;
                }
            }
            "folder_view" => {
                if let Some((index, folder)) = value.split_once('|')
                    && let Some(view) = index.parse().ok().and_then(|index: usize| View::ALL.get(index))
                {
                    self.set_view_of(Path::new(folder), *view);
                }
            }
            "tree_auto_expand_off" => self.tree.auto_expand_off = value == "true",
            "tree_computer_closed" => self.tree.computer_closed = value == "true",
            "tree_quick_access_closed" => self.tree.quick_access_closed = value == "true",
            "show_hidden" => self.show_hidden = value == "true",
            "summary_open" => self.summary_open = value == "true",
            "columns" => {
                let mut columns: Vec<Column> = value
                    .split(',')
                    .filter_map(|id| id.parse().ok().and_then(|id: usize| Column::ALL.get(id)))
                    .copied()
                    .filter(|column| !column.is_fixed())
                    .collect();
                columns.splice(0..0, [Column::Name, Column::Size]);
                self.columns = columns;
            }
            "widths" => {
                self.widths = value
                    .split(',')
                    .filter_map(|pair| pair.split_once(':'))
                    .filter_map(|(id, width)| Some((id.parse().ok()?, width.parse().ok()?)))
                    .collect();
            }
            "sort" => {
                if let Some(column) = value.parse().ok().and_then(|id: usize| Column::ALL.get(id)) {
                    self.sort = *column;
                }
            }
            "descending" => self.descending = value == "true",
            "sidebar_width" => self.sidebar_width = value.parse().unwrap_or(self.sidebar_width),
            "window_placement" => self.placement = WindowPlacement::parse(value).or(self.placement),
            _ => {}
        }
    }

    fn serialize(&self) -> String {
        let columns: Vec<String> = self.columns.iter().map(|column| column_id(*column).to_string()).collect();
        let mut widths: Vec<_> = self.widths.iter().collect();
        widths.sort_by_key(|(id, _)| **id);
        let widths: Vec<String> = widths.iter().map(|(id, width)| format!("{id}:{width}")).collect();
        let mut folder_views: Vec<_> = self.folder_views.iter().collect();
        folder_views.sort_by_key(|(folder, _)| *folder);
        let folder_views: String =
            folder_views.iter().map(|(folder, view)| format!("folder_view={}|{folder}\n", view.index())).collect();
        format!(
            "unit={}\nshow_hidden={}\nsummary_open={}\ncolumns={}\nwidths={}\nsort={}\ndescending={}\nsidebar_width={}\nwindow_placement={}\ntree_auto_expand_off={}\ntree_computer_closed={}\ntree_quick_access_closed={}\n{folder_views}",
            self.unit.index(),
            self.show_hidden,
            self.summary_open,
            columns.join(","),
            widths.join(","),
            column_id(self.sort),
            self.descending,
            self.sidebar_width,
            self.placement.map_or_else(String::new, |placement| placement.serialize()),
            self.tree.auto_expand_off,
            self.tree.computer_closed,
            self.tree.quick_access_closed,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_survive_a_round_trip() {
        let mut settings = Settings {
            unit: Unit::Gigabytes,
            show_hidden: false,
            summary_open: true,
            columns: vec![Column::Name, Column::Size, Column::Sha256],
            sort: Column::Size,
            descending: true,
            sidebar_width: 300.0,
            placement: Some(WindowPlacement { rect: [-1200, 40, 400, 940], maximized: true }),
            tree: TreeMemory { auto_expand_off: true, computer_closed: true, quick_access_closed: true },
            ..Settings::default()
        };
        settings.set_width(Column::Name, 410.5);
        settings.set_view_of(Path::new(r"C:\Old=Data\Photos"), View::Tiles);
        settings.set_view_of(Path::new(r"D:\"), View::Large);
        assert_eq!(Settings::parse(&settings.serialize()), settings);
    }

    #[test]
    fn broken_files_fall_back_to_defaults() {
        let garbage =
            "unit=99\nview=99\ncolumns=abc\nwindow_placement=wide\nsort=-1\nfolder_view=99|C:\\a\nfolder_view=C:\\b\nnonsense\n=\n";
        let settings = Settings::parse(garbage);
        assert_eq!(settings.unit, Unit::Auto);
        assert!(settings.folder_views.is_empty());
        assert_eq!(settings.placement, None);
        assert_eq!(settings.columns, [Column::Name, Column::Size]);
    }

    #[test]
    fn every_folder_starts_in_details_and_keeps_its_own_view() {
        let mut settings = Settings::default();
        let photos = Path::new(r"C:\Users\Me\Photos");
        assert_eq!(settings.view_of(photos), View::Details);
        settings.set_view_of(photos, View::Large);
        assert_eq!(settings.view_of(Path::new(r"c:\users\me\PHOTOS")), View::Large);
        assert_eq!(settings.view_of(Path::new(r"C:\Users\Me")), View::Details);
    }

    #[test]
    fn folders_back_on_details_are_not_stored() {
        let mut settings = Settings::default();
        let photos = Path::new(r"C:\Photos");
        settings.set_view_of(photos, View::Large);
        settings.set_view_of(photos, View::Details);
        assert!(settings.folder_views.is_empty());
    }

    #[test]
    fn name_and_size_columns_are_always_kept_first() {
        let settings = Settings::parse("columns=4,0,1\n");
        assert_eq!(settings.columns, [Column::Name, Column::Size, Column::Modified]);
    }
}
