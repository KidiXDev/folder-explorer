use crate::format::Unit;
use crate::item::Column;
use crate::list::View;
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

#[derive(Clone, Debug, PartialEq)]
pub struct Settings {
    pub unit: Unit,
    pub view: View,
    pub show_hidden: bool,
    pub summary_open: bool,
    pub columns: Vec<Column>,
    pub widths: HashMap<usize, f32>,
    pub sort: Column,
    pub descending: bool,
    pub sidebar_width: f32,
    pub window_size: (u32, u32),
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            unit: Unit::Auto,
            view: View::default(),
            show_hidden: true,
            summary_open: false,
            columns: Column::DEFAULT.to_vec(),
            widths: HashMap::new(),
            sort: Column::Name,
            descending: false,
            sidebar_width: 240.0,
            window_size: (1180, 720),
        }
    }
}

fn column_id(column: Column) -> usize {
    Column::ALL.iter().position(|known| *known == column).unwrap_or(0)
}

impl Settings {
    pub fn width_of(&self, column: Column) -> f32 {
        self.widths.get(&column_id(column)).copied().unwrap_or(column.default_width())
    }

    pub fn set_width(&mut self, column: Column, width: f32) {
        self.widths.insert(column_id(column), width);
    }

    fn file() -> Option<PathBuf> {
        let base = std::env::var_os("APPDATA")?;
        Some(PathBuf::from(base).join("FolderExplorer").join("settings.ini"))
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
            "view" => {
                if let Some(view) = value.parse().ok().and_then(|index: usize| View::ALL.get(index)) {
                    self.view = *view;
                }
            }
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
            "window_size" => {
                if let Some(size) = value.split_once('x').and_then(|(w, h)| Some((w.parse().ok()?, h.parse().ok()?))) {
                    self.window_size = size;
                }
            }
            _ => {}
        }
    }

    fn serialize(&self) -> String {
        let columns: Vec<String> = self.columns.iter().map(|column| column_id(*column).to_string()).collect();
        let mut widths: Vec<_> = self.widths.iter().collect();
        widths.sort_by_key(|(id, _)| **id);
        let widths: Vec<String> = widths.iter().map(|(id, width)| format!("{id}:{width}")).collect();
        format!(
            "unit={}\nview={}\nshow_hidden={}\nsummary_open={}\ncolumns={}\nwidths={}\nsort={}\ndescending={}\nsidebar_width={}\nwindow_size={}x{}\n",
            self.unit.index(),
            self.view.index(),
            self.show_hidden,
            self.summary_open,
            columns.join(","),
            widths.join(","),
            column_id(self.sort),
            self.descending,
            self.sidebar_width,
            self.window_size.0,
            self.window_size.1,
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
            view: View::Tiles,
            show_hidden: false,
            summary_open: true,
            columns: vec![Column::Name, Column::Size, Column::Sha256],
            sort: Column::Size,
            descending: true,
            sidebar_width: 300.0,
            window_size: (1600, 900),
            ..Settings::default()
        };
        settings.set_width(Column::Name, 410.5);
        assert_eq!(Settings::parse(&settings.serialize()), settings);
    }

    #[test]
    fn broken_files_fall_back_to_defaults() {
        let garbage = "unit=99\nview=99\ncolumns=abc\nwindow_size=wide\nsort=-1\nnonsense\n=\n";
        let settings = Settings::parse(garbage);
        assert_eq!(settings.unit, Unit::Auto);
        assert_eq!(settings.view, View::Details);
        assert_eq!(settings.window_size, (1180, 720));
        assert_eq!(settings.columns, [Column::Name, Column::Size]);
    }

    #[test]
    fn name_and_size_columns_are_always_kept_first() {
        let settings = Settings::parse("columns=4,0,1\n");
        assert_eq!(settings.columns, [Column::Name, Column::Size, Column::Modified]);
    }
}
