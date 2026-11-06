use std::fs;
use std::io;
use std::path::Path;

const NEW_FOLDER: &str = "New folder";
const MAX_NEW_FOLDER_ATTEMPTS: u32 = 10_000;
const FORBIDDEN_CHARACTERS: [char; 9] = ['\\', '/', ':', '*', '?', '"', '<', '>', '|'];
const RESERVED_NAMES: [&str; 22] = [
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8", "COM9", "LPT1", "LPT2",
    "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

pub fn validate_name(name: &str) -> Result<(), String> {
    if name.trim().is_empty() {
        return Err("A name can't be empty.".into());
    }
    if name.contains(FORBIDDEN_CHARACTERS) || name.chars().any(char::is_control) {
        return Err("A name can't contain any of these characters:  \\ / : * ? \" < > |".into());
    }
    if name.ends_with(['.', ' ']) {
        return Err("A name can't end with a dot or a space.".into());
    }
    let stem = name.split('.').next().unwrap_or(name).trim_end();
    if RESERVED_NAMES.iter().any(|reserved| reserved.eq_ignore_ascii_case(stem)) {
        return Err(format!("\"{stem}\" is reserved by Windows."));
    }
    Ok(())
}

pub fn create_new_folder(parent: &Path) -> io::Result<String> {
    for attempt in 1..=MAX_NEW_FOLDER_ATTEMPTS {
        let name = if attempt == 1 { NEW_FOLDER.to_string() } else { format!("{NEW_FOLDER} ({attempt})") };
        match fs::create_dir(parent.join(&name)) {
            Ok(()) => return Ok(name),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
    }
    Err(io::ErrorKind::AlreadyExists.into())
}

pub fn editable_stem_length(name: &str, is_dir: bool) -> usize {
    match name.rfind('.') {
        Some(dot) if !is_dir && dot > 0 => dot,
        _ => name.len(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scan::tests::TempTree;

    #[test]
    fn ordinary_names_are_accepted() {
        for name in ["report.pdf", "日本語", ".gitignore", "a b c", "console.txt"] {
            assert_eq!(validate_name(name), Ok(()), "{name}");
        }
    }

    #[test]
    fn forbidden_characters_are_rejected() {
        for name in ["a/b", "a\\b", "what?", "a:b", "star*", "\"q\"", "<x>", "a|b", "tab\there"] {
            assert!(validate_name(name).is_err(), "{name}");
        }
    }

    #[test]
    fn empty_names_are_rejected() {
        assert!(validate_name("").is_err());
        assert!(validate_name("   ").is_err());
    }

    #[test]
    fn trailing_dots_and_spaces_are_rejected() {
        assert!(validate_name("name.").is_err());
        assert!(validate_name("name ").is_err());
    }

    #[test]
    fn reserved_device_names_are_rejected_with_any_extension() {
        for name in ["CON", "con", "nul.txt", "COM1.log", "lpt9"] {
            assert!(validate_name(name).is_err(), "{name}");
        }
    }

    #[test]
    fn new_folders_take_the_first_free_name() {
        let tree = TempTree::new();
        assert_eq!(create_new_folder(&tree.0).unwrap(), "New folder");
        assert_eq!(create_new_folder(&tree.0).unwrap(), "New folder (2)");
        assert_eq!(create_new_folder(&tree.0).unwrap(), "New folder (3)");
        assert!(tree.0.join("New folder (3)").is_dir());
    }

    #[test]
    fn a_file_with_the_same_name_does_not_get_replaced() {
        let tree = TempTree::new();
        tree.file("New folder", 5);
        assert_eq!(create_new_folder(&tree.0).unwrap(), "New folder (2)");
        assert_eq!(fs::read(tree.0.join("New folder")).unwrap().len(), 5);
    }

    #[test]
    fn creating_inside_a_missing_folder_reports_the_error() {
        let tree = TempTree::new();
        assert!(create_new_folder(&tree.0.join("missing")).is_err());
    }

    #[test]
    fn only_the_stem_of_a_file_is_selected_for_editing() {
        assert_eq!(editable_stem_length("report.final.pdf", false), 12);
        assert_eq!(editable_stem_length("README", false), 6);
        assert_eq!(editable_stem_length(".gitignore", false), 10);
        assert_eq!(editable_stem_length("my.folder", true), 9);
        assert_eq!(editable_stem_length("日本語.txt", false), 9);
    }
}
