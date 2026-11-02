//! Steps 5 and 7 (spec §6): active dictionary replacements — whole word, case-aware, longest first.
//!
//! One left-to-right pass: at each word start the longest matching `from` wins, its `to` is written
//! and scanning continues after the match, so one rule's output is never rewritten by another.
//!
//! Case-aware: `from` matches regardless of case. A `to` with any capital letter is written exactly
//! as the user typed it ("Claude", "ElevenLabs"). An all-lower-case `to` follows the heard word: a
//! capitalised word gives a capitalised `to` (sentence starts), an ALL-CAPS word gives ALL CAPS.

use crate::dictionary::Replacement;

/// Applies `rules` to `text`. Returns the new text and the IDs of the rules that changed it (each
/// once, in first-applied order). A match whose output equals the matched text is not "applied".
pub fn apply(text: &str, rules: &[Replacement]) -> (String, Vec<i64>) {
    struct Prepared<'a> {
        id: i64,
        from: Vec<char>,
        to: &'a str,
    }
    let mut prepared: Vec<Prepared> = rules
        .iter()
        .filter_map(|r| {
            let from: Vec<char> = r.from.split_whitespace().collect::<Vec<_>>().join(" ").chars().map(lower).collect();
            (!from.is_empty()).then_some(Prepared { id: r.id, from, to: r.to.as_str() })
        })
        .collect();
    // Longest first; equal lengths by id so the result never depends on input order.
    prepared.sort_by(|a, b| b.from.len().cmp(&a.from.len()).then(a.id.cmp(&b.id)));

    let chars: Vec<char> = text.chars().collect();
    let lowered: Vec<char> = chars.iter().copied().map(lower).collect();
    let mut out = String::with_capacity(text.len());
    let mut applied: Vec<i64> = Vec::new();
    let mut i = 0;
    'scan: while i < chars.len() {
        for rule in &prepared {
            let m = rule.from.len();
            if i + m > chars.len() || lowered[i..i + m] != rule.from[..] {
                continue;
            }
            let left_ok = !is_word_char(rule.from[0]) || i == 0 || !is_word_char(chars[i - 1]);
            let right_ok = !is_word_char(rule.from[m - 1]) || ends_word(&chars, i + m);
            if !(left_ok && right_ok) {
                continue;
            }
            let matched: String = chars[i..i + m].iter().collect();
            let replacement = follow_case(rule.to, &matched);
            if replacement != matched && !applied.contains(&rule.id) {
                applied.push(rule.id);
            }
            out.push_str(&replacement);
            i += m;
            continue 'scan;
        }
        out.push(chars[i]);
        i += 1;
    }
    (out, applied)
}

/// One-to-one lower-casing (characters whose lower case is several characters are kept as they
/// are), so positions in the lowered text line up with the original.
fn lower(c: char) -> char {
    let mut it = c.to_lowercase();
    match (it.next(), it.next()) {
        (Some(l), None) => l,
        _ => c,
    }
}

fn is_word_char(c: char) -> bool {
    c.is_alphanumeric()
}

/// True when a word ends at `pos`: end of text or a non-word character — except an apostrophe
/// that continues the word ("don" must not match in "don't"); a possessive "'s" still ends it.
fn ends_word(chars: &[char], pos: usize) -> bool {
    let Some(&next) = chars.get(pos) else {
        return true;
    };
    if is_word_char(next) {
        return false;
    }
    if next == '\'' || next == '\u{2019}' {
        if let Some(&after) = chars.get(pos + 1) {
            if after.is_alphabetic() {
                let possessive = (after == 's' || after == 'S') && chars.get(pos + 2).is_none_or(|c| !is_word_char(*c));
                return possessive;
            }
        }
    }
    true
}

/// Case-aware output: a `to` with a capital is the user's spelling; an all-lower-case `to` takes
/// the heard word's shape (ALL CAPS, or a capital first letter).
fn follow_case(to: &str, matched: &str) -> String {
    if to.chars().any(char::is_uppercase) {
        return to.to_string();
    }
    let letters: Vec<char> = matched.chars().filter(|c| c.is_alphabetic()).collect();
    if letters.len() >= 2 && letters.iter().all(|c| c.is_uppercase()) {
        return to.to_uppercase();
    }
    if letters.first().is_some_and(|c| c.is_uppercase()) {
        let mut chars = to.chars();
        return match chars.next() {
            Some(first) => first.to_uppercase().chain(chars).collect(),
            None => String::new(),
        };
    }
    to.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(id: i64, from: &str, to: &str) -> Replacement {
        Replacement { id, from: from.into(), to: to.into() }
    }

    #[test]
    fn replacement_table() {
        let rules = vec![
            rule(1, "clawed", "Claude"),
            rule(2, "claude code", "Claude Code"),
            rule(3, "gonna", "going to"),
            rule(4, "eleven labs", "ElevenLabs"),
            rule(5, "tory", "Tauri"),
            rule(6, "c++", "C++"),
            rule(7, "new york", "New York City"),
            rule(8, "york", "Yorkshire"),
            rule(9, "api", "API"),
        ];
        for (input, want, ids) in [
            ("I asked clawed.", "I asked Claude.", vec![1]),
            ("Clawed said no.", "Claude said no.", vec![1]),
            ("CLAWED", "Claude", vec![1]),
            // Whole word only.
            ("clawedback and unclawed", "clawedback and unclawed", vec![]),
            ("history and story", "history and story", vec![]),
            // Possessive and punctuation boundaries still match.
            ("clawed's answer, (clawed)", "Claude's answer, (Claude)", vec![1]),
            // Lower-case `to` follows the heard word's case.
            ("gonna go", "going to go", vec![3]),
            ("Gonna go", "Going to go", vec![3]),
            ("GONNA GO", "GOING TO GO", vec![3]),
            // Multi-word, case-insensitive, any spacing case.
            ("open Claude code now", "open Claude Code now", vec![2]),
            ("built with eleven labs and tory", "built with ElevenLabs and Tauri", vec![4, 5]),
            // Longest first, and one rule's output is not rewritten by a shorter rule.
            ("fly to new york", "fly to New York City", vec![7]),
            ("visit york", "visit Yorkshire", vec![8]),
            // A `from` that ends in punctuation.
            ("write c++ code", "write C++ code", vec![6]),
            // Already right: matched but unchanged, so not counted.
            ("Claude Code is here", "Claude Code is here", vec![]),
            ("the API works", "the API works", vec![]),
            // Repeated hits are recorded once.
            ("clawed, clawed and clawed", "Claude, Claude and Claude", vec![1]),
            ("", "", vec![]),
        ] {
            let (got, applied) = apply(input, &rules);
            assert_eq!(got, want, "{input:?}");
            assert_eq!(applied, ids, "{input:?}");
        }
    }

    #[test]
    fn longest_wins_regardless_of_rule_order() {
        let rules = vec![rule(8, "york", "Yorkshire"), rule(7, "new york", "New York City")];
        assert_eq!(apply("new york", &rules), ("New York City".to_string(), vec![7]));
    }

    #[test]
    fn unicode_text_is_safe() {
        let rules = vec![rule(1, "café", "Café Nero"), rule(2, "naive", "naïve")];
        assert_eq!(apply("at the café, so naive — ünïcödé ✓", &rules).0, "at the Café Nero, so naïve — ünïcödé ✓");
        assert_eq!(apply("CAFÉ", &rules).0, "Café Nero");
    }

    #[test]
    fn empty_or_blank_from_is_ignored() {
        let rules = vec![rule(1, "", "x"), rule(2, "  ", "y")];
        assert_eq!(apply("hello world", &rules), ("hello world".to_string(), vec![]));
    }
}
