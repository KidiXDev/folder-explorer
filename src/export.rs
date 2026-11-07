use crate::item::{Column, Item};
use std::fs::File;
use std::io::{self, BufWriter, Write};
use std::path::Path;

pub const DELIMITERS: [(&str, &str, char); 5] = [
    ("Comma separated (*.csv)", "csv", ','),
    ("Tab separated (*.tsv)", "tsv", '\t'),
    ("Semicolon separated (*.txt)", "txt", ';'),
    ("Pipe separated (*.txt)", "txt", '|'),
    ("Tilde separated (*.txt)", "txt", '~'),
];

fn write_field(out: &mut impl Write, field: &str, delimiter: char) -> io::Result<()> {
    if field.contains([delimiter, '"', '\n', '\r']) {
        write!(out, "\"{}\"", field.replace('"', "\"\""))
    } else {
        out.write_all(field.as_bytes())
    }
}

fn write_row(
    out: &mut impl Write,
    fields: impl Iterator<Item = impl AsRef<str>>,
    delimiter: char,
) -> io::Result<()> {
    for (position, field) in fields.enumerate() {
        if position > 0 {
            write!(out, "{delimiter}")?;
        }
        write_field(out, field.as_ref(), delimiter)?;
    }
    out.write_all(b"\r\n")
}

pub fn write_list<'a>(
    out: &mut impl Write,
    columns: &[Column],
    items: impl Iterator<Item = &'a Item>,
    delimiter: char,
) -> io::Result<()> {
    out.write_all("\u{feff}".as_bytes())?;
    write_row(out, columns.iter().map(|column| column.export_title()), delimiter)?;
    for item in items {
        write_row(out, columns.iter().map(|column| column.export_cell(item)), delimiter)?;
    }
    Ok(())
}

pub fn export_file<'a>(
    path: &Path,
    columns: &[Column],
    items: impl Iterator<Item = &'a Item>,
    delimiter: char,
) -> io::Result<()> {
    let mut out = BufWriter::new(File::create(path)?);
    write_list(&mut out, columns, items, delimiter)?;
    out.flush()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::item::ItemState;
    use crate::scan::Times;
    use std::sync::Arc;

    fn item(name: &str, size: u64) -> Item {
        Item {
            name: name.into(),
            native_name: None,
            parent: Arc::from(Path::new(r"C:\data")),
            is_dir: false,
            attrs: 0,
            times: Times::default(),
            size,
            file_count: 0,
            dir_count: 0,
            state: ItemState::Ready,
            hashes: None,
        }
    }

    fn exported(items: &[Item], delimiter: char) -> String {
        let mut out = Vec::new();
        write_list(&mut out, &[Column::Name, Column::Size], items.iter(), delimiter).unwrap();
        String::from_utf8(out).unwrap().trim_start_matches('\u{feff}').to_string()
    }

    #[test]
    fn rows_follow_the_header() {
        assert_eq!(exported(&[item("a.txt", 5)], ','), "Name,Size (bytes)\r\na.txt,5\r\n");
    }

    #[test]
    fn fields_holding_the_delimiter_or_quotes_are_quoted() {
        assert_eq!(exported(&[item("a,b.txt", 1)], ',').lines().nth(1), Some("\"a,b.txt\",1"));
        assert_eq!(exported(&[item("say \"hi\".txt", 1)], ',').lines().nth(1), Some("\"say \"\"hi\"\".txt\",1"));
    }

    #[test]
    fn other_delimiters_leave_commas_alone() {
        assert_eq!(exported(&[item("a,b.txt", 1)], '\t').lines().nth(1), Some("a,b.txt\t1"));
    }
}
