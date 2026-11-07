use crate::format::{format_size, group_digits, Unit};
use crate::item::Item;
use crate::platform;
use crate::scan::{DirNode, FileEntry, Times};
use rayon::prelude::*;
use std::collections::HashMap;
use std::ffi::OsString;
use std::ops::Range;
use std::path::Path;
use std::sync::Arc;

const TICKS_PER_MONTH: u64 = 26_300_160_000_000;
const AGE_MONTHS: [u64; 23] =
    [0, 1, 2, 3, 6, 9, 12, 24, 36, 48, 60, 72, 84, 96, 108, 120, 132, 144, 156, 168, 180, 192, 204];

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DateField {
    #[default]
    Modified,
    Created,
    Accessed,
}

impl DateField {
    pub fn of(self, times: &Times) -> u64 {
        match self {
            DateField::Modified => times.modified,
            DateField::Created => times.created,
            DateField::Accessed => times.accessed,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DateRule {
    After,
    Before,
    On,
}

pub fn date_range(day: Range<u64>, rule: DateRule) -> Range<u64> {
    match rule {
        DateRule::After => day.end..u64::MAX,
        DateRule::Before => 0..day.start,
        DateRule::On => day,
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Query {
    pub text: String,
    pub extension: Option<String>,
    pub size: Option<Range<u64>>,
    pub date: Option<(DateField, Range<u64>)>,
}

impl Query {
    pub fn new(text: &str) -> Self {
        Self { text: text.trim().to_lowercase(), ..Self::default() }
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty() && self.extension.is_none() && self.size.is_none() && self.date.is_none()
    }

    fn files_only(&self) -> bool {
        self.extension.is_some() || self.size.is_some() || self.date.is_some()
    }

    fn matches_name(&self, name: &str) -> bool {
        self.text.is_empty() || contains_ignoring_case(name, &self.text)
    }

    fn matches_file(&self, name: &str, entry: &FileEntry) -> bool {
        self.matches_name(name)
            && !(self.files_only() && entry.is_folder_link())
            && self.extension.as_ref().is_none_or(|wanted| extension_of(name) == *wanted)
            && self.size.as_ref().is_none_or(|range| range.contains(&entry.size))
            && self.date.as_ref().is_none_or(|(field, range)| range.contains(&field.of(&entry.times)))
    }
}

fn contains_ignoring_case(haystack: &str, lowercase_needle: &str) -> bool {
    if lowercase_needle.is_ascii() {
        let needle = lowercase_needle.as_bytes();
        haystack.as_bytes().windows(needle.len()).any(|window| window.eq_ignore_ascii_case(needle))
    } else {
        haystack.to_lowercase().contains(lowercase_needle)
    }
}

pub fn extension_of(name: &str) -> String {
    match name.rsplit_once('.') {
        Some((stem, extension)) if !stem.is_empty() => extension.to_ascii_lowercase(),
        _ => String::new(),
    }
}

pub fn search(node: &DirNode, path: &Path, query: &Query) -> Vec<Item> {
    let parent: Arc<Path> = path.into();
    let mut hits: Vec<Item> = node
        .files()
        .filter(|(name, entry)| query.matches_file(&name.to_string_lossy(), entry))
        .map(|(name, entry)| Item::file(name, entry, &parent))
        .collect();
    if !query.files_only() {
        hits.extend(
            node.children
                .iter()
                .filter(|child| query.matches_name(&child.name.to_string_lossy()))
                .map(|child| Item::folder(child, &parent)),
        );
    }
    let nested: Vec<Item> = node
        .children
        .par_iter()
        .flat_map_iter(|child| search(child, &path.join(&*child.name), query))
        .collect();
    hits.extend(nested);
    hits
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SummaryKind {
    #[default]
    Extension,
    Folders,
    Age(DateField),
}

#[derive(Clone, Debug, PartialEq)]
pub struct SummaryEntry {
    pub label: String,
    pub files: u64,
    pub size: u64,
    pub filter: Query,
    pub folder: Option<OsString>,
}

impl SummaryEntry {
    pub fn detail(&self, unit: Unit) -> String {
        format!("{} · {}", format_size(self.size, unit), group_digits(self.files))
    }
}

fn flatten<'a>(node: &'a DirNode, into: &mut Vec<&'a DirNode>) {
    into.push(node);
    for child in &node.children {
        flatten(child, into);
    }
}

fn tally<K: Eq + std::hash::Hash + Send>(
    node: &DirNode,
    key_of: impl Fn(&str, &FileEntry) -> K + Sync,
) -> HashMap<K, (u64, u64)> {
    let mut nodes = Vec::new();
    flatten(node, &mut nodes);
    nodes
        .par_iter()
        .fold(HashMap::new, |mut totals: HashMap<K, (u64, u64)>, dir| {
            for (name, entry) in dir.files().filter(|(_, entry)| !entry.is_folder_link()) {
                let total = totals.entry(key_of(&name.to_string_lossy(), entry)).or_default();
                total.0 += 1;
                total.1 += entry.size;
            }
            totals
        })
        .reduce(HashMap::new, |mut merged, part| {
            for (key, (files, size)) in part {
                let total = merged.entry(key).or_default();
                total.0 += files;
                total.1 += size;
            }
            merged
        })
}

fn age_bucket(ticks: u64, now: u64) -> usize {
    let months = now.saturating_sub(ticks) / TICKS_PER_MONTH;
    AGE_MONTHS.partition_point(|threshold| *threshold <= months) - 1
}

fn age_label(bucket: usize) -> String {
    match AGE_MONTHS[bucket] {
        0 => "Under 1 month".into(),
        1 => "Over 1 month".into(),
        12 => "Over 1 year".into(),
        months if months < 12 => format!("Over {months} months"),
        months => format!("Over {} years", months / 12),
    }
}

fn age_range(bucket: usize, now: u64) -> Range<u64> {
    let end = match AGE_MONTHS[bucket] {
        0 => u64::MAX,
        months => now.saturating_sub(months * TICKS_PER_MONTH) + 1,
    };
    let start = AGE_MONTHS.get(bucket + 1).map_or(0, |next| now.saturating_sub(next * TICKS_PER_MONTH) + 1);
    start..end
}

pub fn summarize(node: &DirNode, kind: SummaryKind) -> Vec<SummaryEntry> {
    match kind {
        SummaryKind::Extension => {
            let mut entries: Vec<_> = tally(node, |name, _| extension_of(name))
                .into_iter()
                .map(|(extension, (files, size))| SummaryEntry {
                    label: if extension.is_empty() { "No extension".into() } else { format!(".{extension}") },
                    files,
                    size,
                    filter: Query { extension: Some(extension), ..Query::default() },
                    folder: None,
                })
                .collect();
            entries.sort_by(|a, b| b.size.cmp(&a.size).then_with(|| a.label.cmp(&b.label)));
            entries
        }
        SummaryKind::Folders => {
            let mut entries: Vec<_> = node
                .children
                .iter()
                .filter(|child| child.size > 0 || child.file_count > 0)
                .map(|child| {
                    let label = child.name.to_string_lossy().into_owned();
                    SummaryEntry {
                        files: child.file_count,
                        size: child.size,
                        filter: Query::new(&label),
                        folder: Some(child.name.to_os_string()),
                        label,
                    }
                })
                .collect();
            let direct_files: Vec<_> = node.files().filter(|(_, entry)| !entry.is_folder_link()).collect();
            if entries.is_empty() {
                entries.extend(direct_files.into_iter().map(|(name, entry)| {
                    let label = name.to_string_lossy().into_owned();
                    SummaryEntry { files: 1, size: entry.size, filter: Query::new(&label), folder: None, label }
                }));
            } else if !direct_files.is_empty() {
                let files = direct_files.len() as u64;
                let size: u64 = direct_files.iter().map(|(_, entry)| entry.size).sum();
                if size > 0 || files > 0 {
                    entries.push(SummaryEntry {
                        label: "[Folder files]".into(),
                        files,
                        size,
                        filter: Query::default(),
                        folder: None,
                    });
                }
            }
            entries.sort_by(|a, b| b.size.cmp(&a.size).then_with(|| a.label.cmp(&b.label)));
            entries
        }
        SummaryKind::Age(field) => {
            let now = platform::now_filetime();
            let mut buckets: Vec<_> =
                tally(node, |_, entry| age_bucket(field.of(&entry.times), now)).into_iter().collect();
            buckets.sort_by_key(|(bucket, _)| *bucket);
            buckets
                .into_iter()
                .map(|(bucket, (files, size))| SummaryEntry {
                    label: age_label(bucket),
                    files,
                    size,
                    filter: Query { date: Some((field, age_range(bucket, now))), ..Query::default() },
                    folder: None,
                })
                .collect()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scan::tests::{TempTree, scan};
    use std::fs;
    use std::os::windows::ffi::OsStringExt;
    use std::process::Command;

    const TICKS_PER_DAY: u64 = 864_000_000_000;

    fn sample() -> TempTree {
        let tree = TempTree::new();
        tree.file("Readme.TXT", 10)
            .file("notes.txt", 20)
            .file("photos/holiday.jpg", 300)
            .file("photos/raw/holiday.cr2", 4000)
            .file("Makefile", 50);
        tree
    }

    fn names(mut hits: Vec<Item>) -> Vec<String> {
        hits.sort_by(|a, b| a.name.cmp(&b.name));
        hits.iter().map(|hit| hit.name.to_string()).collect()
    }

    fn run(tree: &TempTree, query: &Query) -> Vec<Item> {
        search(&scan(&tree.0), &tree.0, query)
    }

    #[test]
    fn text_matches_names_ignoring_case_at_any_depth() {
        let tree = sample();
        assert_eq!(names(run(&tree, &Query::new("HOLIDAY"))), ["holiday.cr2", "holiday.jpg"]);
        assert_eq!(names(run(&tree, &Query::new("readme"))), ["Readme.TXT"]);
    }

    #[test]
    fn folders_match_by_name_too() {
        let tree = sample();
        assert_eq!(names(run(&tree, &Query::new("photos"))), ["photos"]);
    }

    #[test]
    fn non_ascii_names_match_ignoring_case() {
        let tree = TempTree::new();
        tree.file("ÉTÉ.txt", 1);
        assert_eq!(names(run(&tree, &Query::new("été"))), ["ÉTÉ.txt"]);
    }

    #[test]
    fn hits_know_where_they_live() {
        let tree = sample();
        let hits = run(&tree, &Query::new("holiday.cr2"));
        assert_eq!(hits[0].path(), tree.0.join("photos").join("raw").join("holiday.cr2"));
    }

    #[test]
    fn hits_reach_the_exact_file_when_names_are_not_valid_unicode() {
        let tree = TempTree::new();
        let names = [OsString::from_wide(&[0x61, 0xD800]), OsString::from_wide(&[0x61, 0xD801])];
        for (index, name) in names.iter().enumerate() {
            let folder = tree.0.join(name);
            fs::create_dir(&folder).unwrap();
            fs::write(folder.join(&names[1 - index]), vec![0; index + 1]).unwrap();
        }
        let hits = run(&tree, &Query::new("a"));
        assert_eq!(hits.len(), 4);
        for hit in hits.iter().filter(|hit| !hit.is_dir) {
            assert_eq!(fs::metadata(hit.path()).unwrap().len(), hit.size);
        }
        let folders: Vec<_> = hits.iter().filter(|hit| hit.is_dir).map(Item::path).collect();
        assert!(folders.iter().all(|folder| folder.is_dir()));
        assert_ne!(folders[0], folders[1]);
    }

    #[test]
    fn folder_rows_keep_the_exact_name_for_opening() {
        let tree = TempTree::new();
        let name = OsString::from_wide(&[0x61, 0xD800]);
        tree.file("seed.txt", 1);
        fs::create_dir(tree.0.join(&name)).unwrap();
        fs::write(tree.0.join(&name).join("inside.txt"), [0]).unwrap();
        let summary = summarize(&scan(&tree.0), SummaryKind::Folders);
        assert!(summary.iter().any(|row| row.folder.as_ref() == Some(&name)));
    }

    #[test]
    fn size_filters_return_files_only() {
        let tree = sample();
        let query = Query { size: Some(100..u64::MAX), ..Query::default() };
        assert_eq!(names(run(&tree, &query)), ["holiday.cr2", "holiday.jpg"]);
    }

    #[test]
    fn extension_filters_match_exactly_ignoring_case() {
        let tree = sample();
        let query = Query { extension: Some("txt".into()), ..Query::default() };
        assert_eq!(names(run(&tree, &query)), ["Readme.TXT", "notes.txt"]);
        let query = Query { extension: Some(String::new()), ..Query::default() };
        assert_eq!(names(run(&tree, &query)), ["Makefile"]);
    }

    #[test]
    fn folder_links_are_not_files() {
        let tree = sample();
        let junction = tree.0.join("link");
        let target = tree.0.join("photos");
        let made = Command::new("cmd").args(["/c", "mklink", "/J"]).arg(&junction).arg(&target).output().unwrap();
        assert!(made.status.success());
        let node = scan(&tree.0);
        let everything = Query { size: Some(0..u64::MAX), ..Query::default() };
        assert_eq!(search(&node, &tree.0, &everything).len(), 5);
        let counted: u64 = summarize(&node, SummaryKind::Extension).iter().map(|row| row.files).sum();
        assert_eq!(counted, 5);
        assert_eq!(names(run(&tree, &Query::new("link"))), ["link"]);
    }

    #[test]
    fn date_rules_treat_a_day_as_a_whole() {
        let start = 1_000 * TICKS_PER_DAY;
        let day = start..start + TICKS_PER_DAY;
        assert!(date_range(day.clone(), DateRule::On).contains(&(start + 5)));
        assert!(!date_range(day.clone(), DateRule::On).contains(&day.end));
        assert!(date_range(day.clone(), DateRule::After).contains(&day.end));
        assert!(!date_range(day.clone(), DateRule::After).contains(&(start + 5)));
        assert!(date_range(day.clone(), DateRule::Before).contains(&(start - 1)));
        assert!(!date_range(day, DateRule::Before).contains(&start));
    }

    #[test]
    fn date_rules_use_the_end_of_the_day_they_are_given() {
        let start = 1_000 * TICKS_PER_DAY;
        let short_day = start..start + TICKS_PER_DAY / 24 * 23;
        assert_eq!(date_range(short_day.clone(), DateRule::After).start, short_day.end);
        assert_eq!(date_range(short_day.clone(), DateRule::On), short_day);
    }

    #[test]
    fn date_filters_compare_the_chosen_field() {
        let tree = sample();
        let today = platform::now_filetime();
        let recent = Query { date: Some((DateField::Modified, today - TICKS_PER_DAY..u64::MAX)), ..Query::default() };
        assert_eq!(run(&tree, &recent).len(), 5);
        let old = Query { date: Some((DateField::Created, 0..today - TICKS_PER_DAY)), ..Query::default() };
        assert!(run(&tree, &old).is_empty());
    }

    #[test]
    fn extensions_are_summarized_largest_first() {
        let tree = sample();
        let summary = summarize(&scan(&tree.0), SummaryKind::Extension);
        let rows: Vec<_> = summary.iter().map(|row| (row.label.as_str(), row.files, row.size)).collect();
        assert_eq!(
            rows,
            [(".cr2", 1, 4000), (".jpg", 1, 300), ("No extension", 1, 50), (".txt", 2, 30)]
        );
    }

    #[test]
    fn summary_filters_find_the_same_files_they_counted() {
        let tree = sample();
        let node = scan(&tree.0);
        for kind in [SummaryKind::Extension, SummaryKind::Age(DateField::Modified)] {
            for row in summarize(&node, kind) {
                let hits = search(&node, &tree.0, &row.filter);
                assert_eq!(hits.len() as u64, row.files, "{}", row.label);
            }
        }
    }

    #[test]
    fn ages_land_in_the_bucket_they_have_reached() {
        let now = 500 * TICKS_PER_MONTH;
        let bucket_for_age = |months: u64| age_bucket(now - months * TICKS_PER_MONTH, now);
        assert_eq!(AGE_MONTHS[bucket_for_age(0)], 0);
        assert_eq!(AGE_MONTHS[bucket_for_age(1)], 1);
        assert_eq!(AGE_MONTHS[bucket_for_age(5)], 3);
        assert_eq!(AGE_MONTHS[bucket_for_age(13)], 12);
        assert_eq!(AGE_MONTHS[bucket_for_age(400)], 204);
        assert_eq!(age_bucket(now + 5, now), 0);
    }

    #[test]
    fn age_ranges_tile_time_without_gaps() {
        let now = 500 * TICKS_PER_MONTH;
        for bucket in 0..AGE_MONTHS.len() {
            let range = age_range(bucket, now);
            let inside = if bucket == 0 { now } else { range.end - 1 };
            assert_eq!(age_bucket(inside, now), bucket);
            if let Some(next) = AGE_MONTHS.get(bucket + 1).map(|_| bucket + 1) {
                assert_eq!(age_range(next, now).end, range.start);
            }
        }
    }

    #[test]
    fn labels_read_naturally() {
        assert_eq!(age_label(0), "Under 1 month");
        assert_eq!(age_label(1), "Over 1 month");
        assert_eq!(age_label(3), "Over 3 months");
        assert_eq!(age_label(6), "Over 1 year");
        assert_eq!(age_label(7), "Over 2 years");
    }

    #[test]
    fn empty_folders_summarize_to_nothing() {
        let tree = TempTree::new();
        fs::create_dir(tree.0.join("empty")).unwrap();
        assert!(summarize(&scan(&tree.0), SummaryKind::Extension).is_empty());
    }
}
