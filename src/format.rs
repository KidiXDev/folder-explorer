const KIB: f64 = 1024.0;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Unit {
    #[default]
    Auto,
    Bytes,
    Kilobytes,
    Megabytes,
    Gigabytes,
    Terabytes,
}

impl Unit {
    pub const ALL: [Unit; 6] =
        [Unit::Auto, Unit::Bytes, Unit::Kilobytes, Unit::Megabytes, Unit::Gigabytes, Unit::Terabytes];

    pub fn index(self) -> usize {
        Self::ALL.iter().position(|unit| *unit == self).unwrap_or(0)
    }
}

pub fn group_digits(value: u64) -> String {
    let digits = value.to_string();
    let mut grouped = String::with_capacity(digits.len() + digits.len() / 3);
    for (position, digit) in digits.chars().enumerate() {
        if position > 0 && (digits.len() - position).is_multiple_of(3) {
            grouped.push(',');
        }
        grouped.push(digit);
    }
    grouped
}

fn scaled(bytes: u64, power: i32, suffix: &str) -> String {
    let value = bytes as f64 / KIB.powi(power);
    let fixed = format!("{value:.1}");
    let (whole, fraction) = fixed.split_once('.').unwrap_or((&fixed, "0"));
    format!("{}.{fraction} {suffix}", group_digits(whole.parse().unwrap_or(0)))
}

pub fn format_size(bytes: u64, unit: Unit) -> String {
    match unit {
        Unit::Bytes => format!("{} B", group_digits(bytes)),
        Unit::Kilobytes => scaled(bytes, 1, "KB"),
        Unit::Megabytes => scaled(bytes, 2, "MB"),
        Unit::Gigabytes => scaled(bytes, 3, "GB"),
        Unit::Terabytes => scaled(bytes, 4, "TB"),
        Unit::Auto => {
            let power = (0..=4).rev().find(|power| bytes as f64 >= KIB.powi(*power)).unwrap_or(0);
            let unit = Unit::ALL[power as usize + 1];
            format_size(bytes, unit)
        }
    }
}

pub fn format_elapsed(seconds: f64) -> String {
    if seconds < 60.0 {
        format!("{seconds:.1} s")
    } else {
        format!("{} min {} s", seconds as u64 / 60, seconds as u64 % 60)
    }
}

pub fn plural(count: u64, noun: &str) -> String {
    let suffix = if count == 1 { "" } else { "s" };
    format!("{} {noun}{suffix}", group_digits(count))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn digits_are_grouped_in_thousands() {
        assert_eq!(group_digits(0), "0");
        assert_eq!(group_digits(999), "999");
        assert_eq!(group_digits(1_000), "1,000");
        assert_eq!(group_digits(1_234_567), "1,234,567");
    }

    #[test]
    fn auto_picks_the_largest_fitting_unit() {
        assert_eq!(format_size(0, Unit::Auto), "0 B");
        assert_eq!(format_size(1023, Unit::Auto), "1,023 B");
        assert_eq!(format_size(1024, Unit::Auto), "1.0 KB");
        assert_eq!(format_size(1536, Unit::Auto), "1.5 KB");
        assert_eq!(format_size(5 * 1024 * 1024 * 1024, Unit::Auto), "5.0 GB");
    }

    #[test]
    fn fixed_units_keep_their_unit_even_when_large() {
        assert_eq!(format_size(2048, Unit::Bytes), "2,048 B");
        assert_eq!(format_size(3 * 1024 * 1024 * 1024, Unit::Megabytes), "3,072.0 MB");
        assert_eq!(format_size(0, Unit::Terabytes), "0.0 TB");
    }

    #[test]
    fn elapsed_switches_to_minutes() {
        assert_eq!(format_elapsed(2.34), "2.3 s");
        assert_eq!(format_elapsed(125.0), "2 min 5 s");
    }

    #[test]
    fn nouns_are_pluralized() {
        assert_eq!(plural(1, "file"), "1 file");
        assert_eq!(plural(2_000, "file"), "2,000 files");
    }
}
