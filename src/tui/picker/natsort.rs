//! Natural sort ("pic2" < "pic10"), ported from yazi.
//!
//! Ported from yazi (https://github.com/sxyazi/yazi), `yazi-shared/src/natsort.rs`
//! at commit b4f0d123952e52c4dafe874178de2b34c82f1ab2, itself a port of Martin
//! Pool's `strnatcmp.c` (http://sourcefrog.net/projects/natsort/). Changes: the
//! `unsafe` unchecked unwraps are replaced by safe comparisons.
//!
//! Copyright (c) 2023 - sxyazi. Licensed under the MIT License:
//!
//! Permission is hereby granted, free of charge, to any person obtaining a copy
//! of this software and associated documentation files (the "Software"), to deal
//! in the Software without restriction, including without limitation the rights
//! to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
//! copies of the Software, and to permit persons to whom the Software is
//! furnished to do so, subject to the following conditions:
//!
//! The above copyright notice and this permission notice shall be included in all
//! copies or substantial portions of the Software.
//!
//! THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
//! IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
//! FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
//! AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
//! LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
//! OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
//! SOFTWARE.

use std::cmp::Ordering;

macro_rules! return_unless_equal {
    ($ord:expr) => {
        match $ord {
            Ordering::Equal => {}
            ord => return ord,
        }
    };
}

/// Compares digit runs with a leading zero as fractions: first difference wins.
fn compare_left(left: &[u8], right: &[u8], li: &mut usize, ri: &mut usize) -> Ordering {
    loop {
        let l = left.get(*li).filter(|b| b.is_ascii_digit());
        let r = right.get(*ri).filter(|b| b.is_ascii_digit());
        match (l, r) {
            (Some(l), Some(r)) => return_unless_equal!(l.cmp(r)),
            (Some(_), None) => return Ordering::Greater,
            (None, Some(_)) => return Ordering::Less,
            (None, None) => return Ordering::Equal,
        }
        *li += 1;
        *ri += 1;
    }
}

/// Compares integer digit runs: the longer run wins, else the first difference.
fn compare_right(left: &[u8], right: &[u8], li: &mut usize, ri: &mut usize) -> Ordering {
    let mut bias = Ordering::Equal;
    loop {
        let l = left.get(*li).filter(|b| b.is_ascii_digit());
        let r = right.get(*ri).filter(|b| b.is_ascii_digit());
        match (l, r) {
            (Some(l), Some(r)) => {
                if bias == Ordering::Equal {
                    bias = l.cmp(r);
                }
            }
            (Some(_), None) => return Ordering::Greater,
            (None, Some(_)) => return Ordering::Less,
            (None, None) => return bias,
        }
        *li += 1;
        *ri += 1;
    }
}

pub fn natsort(left: &[u8], right: &[u8], insensitive: bool) -> Ordering {
    let mut li = 0;
    let mut ri = 0;
    let mut l = left.get(li);
    let mut r = right.get(ri);

    macro_rules! left_next {
        () => {{
            li += 1;
            l = left.get(li);
        }};
    }
    macro_rules! right_next {
        () => {{
            ri += 1;
            r = right.get(ri);
        }};
    }

    loop {
        while l.is_some_and(|c| c.is_ascii_whitespace()) {
            left_next!();
        }
        while r.is_some_and(|c| c.is_ascii_whitespace()) {
            right_next!();
        }

        match (l, r) {
            (Some(&ll), Some(&rr)) => {
                if ll.is_ascii_digit() && rr.is_ascii_digit() {
                    if ll == b'0' || rr == b'0' {
                        return_unless_equal!(compare_left(left, right, &mut li, &mut ri));
                    } else {
                        return_unless_equal!(compare_right(left, right, &mut li, &mut ri));
                    }
                    l = left.get(li);
                    r = right.get(ri);
                    continue;
                }
                if insensitive {
                    return_unless_equal!(ll.to_ascii_lowercase().cmp(&rr.to_ascii_lowercase()));
                } else {
                    return_unless_equal!(ll.cmp(&rr));
                }
            }
            (Some(_), None) => return Ordering::Greater,
            (None, Some(_)) => return Ordering::Less,
            (None, None) => return Ordering::Equal,
        }

        left_next!();
        right_next!();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cmp(left: &[&str]) {
        let mut right = left.to_vec();
        right.sort_by(|a, b| natsort(a.as_bytes(), b.as_bytes(), true));
        assert_eq!(left, right);
    }

    // Fixtures from yazi's own natsort tests.
    #[test]
    fn natsort_matches_yazi_fixtures() {
        let dates = [
            "1999-3-3",
            "1999-12-25",
            "2000-1-2",
            "2000-1-10",
            "2000-3-23",
        ];
        let fractions = [
            "1.002.01", "1.002.03", "1.002.08", "1.009.02", "1.009.10", "1.009.20", "1.010.12",
            "1.011.02",
        ];
        let words = [
            "1-02",
            "1-2",
            "1-20",
            "10-20",
            "fred",
            "jane",
            "pic01",
            "pic02",
            "pic02a",
            "pic02000",
            "pic05",
            "pic2",
            "pic3",
            "pic4",
            "pic 4 else",
            "pic 5",
            "pic 5 ",
            "pic 5 something",
            "pic 6",
            "pic   7",
            "pic100",
            "pic100a",
            "pic120",
            "pic121",
            "tom",
            "x2-g8",
            "x2-y08",
            "x2-y7",
            "x8-y8",
        ];
        cmp(&dates);
        cmp(&fractions);
        cmp(&words);
    }
}
