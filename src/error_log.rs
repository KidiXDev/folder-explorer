use crate::platform;
use crate::settings;
use std::backtrace::Backtrace;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::panic;
use std::path::{Path, PathBuf};
use std::thread;

const LOG_LIMIT_BYTES: u64 = 512 * 1024;
const LISTED_PATHS: usize = 5;

fn log_file() -> Option<PathBuf> {
    Some(settings::app_folder()?.join("errors.log"))
}

fn previous_log(file: &Path) -> PathBuf {
    file.with_extension("old.log")
}

fn append(file: &Path, entry: &str, limit: u64) -> io::Result<()> {
    if let Some(folder) = file.parent() {
        fs::create_dir_all(folder)?;
    }
    if fs::metadata(file).is_ok_and(|metadata| metadata.len() + entry.len() as u64 > limit) {
        fs::rename(file, previous_log(file))?;
    }
    OpenOptions::new().create(true).append(true).open(file)?.write_all(entry.as_bytes())
}

pub fn record(message: &str) {
    let Some(file) = log_file() else { return };
    let entry = format!("{} {message}\n", platform::format_time(platform::now_filetime()));
    let _ = append(&file, &entry, LOG_LIMIT_BYTES);
}

pub fn describe_paths(paths: &[PathBuf]) -> String {
    let listed: Vec<String> = paths.iter().take(LISTED_PATHS).map(|path| path.display().to_string()).collect();
    match paths.len().saturating_sub(LISTED_PATHS) {
        0 => listed.join(", "),
        more => format!("{} and {more} more", listed.join(", ")),
    }
}

pub fn install() {
    let previous = panic::take_hook();
    panic::set_hook(Box::new(move |info| {
        let thread = thread::current();
        let name = thread.name().unwrap_or("unnamed");
        record(&format!("panic in thread '{name}': {info}\n{}", Backtrace::force_capture()));
        previous(info);
    }));
    platform::report_native_crashes();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scan::tests::TempTree;

    #[test]
    fn entries_accumulate_below_the_limit() {
        let tree = TempTree::new();
        let file = tree.0.join("logs").join("errors.log");
        append(&file, "first\n", 100).unwrap();
        append(&file, "second\n", 100).unwrap();
        assert_eq!(fs::read_to_string(&file).unwrap(), "first\nsecond\n");
        assert!(!previous_log(&file).exists());
    }

    #[test]
    fn a_full_log_rotates_and_replaces_the_older_one() {
        let tree = TempTree::new();
        let file = tree.0.join("errors.log");
        append(&file, "aaaaaaaa\n", 12).unwrap();
        append(&file, "bbbbbbbb\n", 12).unwrap();
        append(&file, "cccccccc\n", 12).unwrap();
        assert_eq!(fs::read_to_string(&file).unwrap(), "cccccccc\n");
        assert_eq!(fs::read_to_string(previous_log(&file)).unwrap(), "bbbbbbbb\n");
    }

    #[test]
    fn long_path_lists_are_shortened() {
        let paths: Vec<PathBuf> = (0..7).map(|index| PathBuf::from(format!(r"C:\{index}"))).collect();
        assert_eq!(describe_paths(&paths[..2]), r"C:\0, C:\1");
        assert_eq!(describe_paths(&paths), r"C:\0, C:\1, C:\2, C:\3, C:\4 and 2 more");
    }
}
