//! Natural ordering for directory entries: case-insensitive text, with digit runs compared by
//! numeric value, so "file2" sorts before "file10".

use std::cmp::Ordering;
use std::iter::Peekable;
use std::str::Chars;

/// Orders `a` and `b` naturally. Ties on the folded text fall back to a byte-wise comparison,
/// so the result is a total order and sorting is deterministic.
pub fn natural_cmp(a: &str, b: &str) -> Ordering {
    folded_cmp(a, b).then_with(|| a.cmp(b))
}

fn folded_cmp(a: &str, b: &str) -> Ordering {
    let mut a = a.chars().peekable();
    let mut b = b.chars().peekable();
    loop {
        match (a.peek().copied(), b.peek().copied()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(x), Some(y)) if x.is_ascii_digit() && y.is_ascii_digit() => {
                let ord = cmp_digit_runs(&mut a, &mut b);
                if ord != Ordering::Equal {
                    return ord;
                }
            }
            (Some(_), Some(_)) => {
                let ord = a.next().unwrap().to_lowercase().cmp(b.next().unwrap().to_lowercase());
                if ord != Ordering::Equal {
                    return ord;
                }
            }
        }
    }
}

/// Consumes one digit run from each side and compares their values.
fn cmp_digit_runs(a: &mut Peekable<Chars>, b: &mut Peekable<Chars>) -> Ordering {
    let da = take_digits(a);
    let db = take_digits(b);
    let (na, nb) = (da.trim_start_matches('0'), db.trim_start_matches('0'));
    // Longer runs (after stripping zeros) are larger; equal lengths compare digit by digit.
    na.len()
        .cmp(&nb.len())
        .then_with(|| na.cmp(nb))
        // Equal value: the run with fewer leading zeros sorts first ("1" before "01").
        .then_with(|| da.len().cmp(&db.len()))
}

fn take_digits(chars: &mut Peekable<Chars>) -> String {
    let mut digits = String::new();
    while let Some(c) = chars.next_if(char::is_ascii_digit) {
        digits.push(c);
    }
    digits
}

#[cfg(test)]
mod tests {
    use super::natural_cmp;
    use std::cmp::Ordering::{Equal, Greater, Less};

    #[test]
    fn digit_runs_compare_numerically() {
        assert_eq!(natural_cmp("file2", "file10"), Less);
        assert_eq!(natural_cmp("x10", "x9z"), Greater);
        assert_eq!(natural_cmp("a100b", "a99c"), Greater);
    }

    #[test]
    fn text_is_case_insensitive() {
        assert_eq!(natural_cmp("apple", "Banana"), Less);
        assert_eq!(natural_cmp("Zebra", "apple"), Greater);
        // Same letters in different case still get a stable order.
        assert_ne!(natural_cmp("Dir", "dir"), Equal);
    }

    #[test]
    fn leading_zeros_break_numeric_ties() {
        assert_eq!(natural_cmp("img1", "img01"), Less);
        assert_eq!(natural_cmp("img007", "img7"), Greater);
    }

    #[test]
    fn empty_and_prefix_sort_first() {
        assert_eq!(natural_cmp("", "a"), Less);
        assert_eq!(natural_cmp("a", "ab"), Less);
        assert_eq!(natural_cmp("a", "a"), Equal);
    }
}
