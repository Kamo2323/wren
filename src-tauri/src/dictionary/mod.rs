//! Dictionary data types and the common-words list (spec §8).
//!
//! The entries themselves live in the store (`store::Store::dictionary_*`); this module holds the
//! shapes they travel in and the pure helpers the learner needs.

pub mod learn;

use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::sync::OnceLock;

/// What a dictionary entry does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum EntryKind {
    /// A word sent to the model to bias recognition.
    Term,
    /// `from → to`, applied whole-word and case-aware after recognition.
    Replacement,
}

/// Where an entry came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Source {
    /// Kept in the schema but unused: there is no Willow import (spec §8).
    WillowSeed,
    Manual,
    LearnedHistory,
    LearnedPaste,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Status {
    Candidate,
    Active,
    Disabled,
}

impl EntryKind {
    pub fn as_db(self) -> &'static str {
        match self {
            EntryKind::Term => "term",
            EntryKind::Replacement => "replacement",
        }
    }
    pub fn from_db(s: &str) -> Option<Self> {
        match s {
            "term" => Some(EntryKind::Term),
            "replacement" => Some(EntryKind::Replacement),
            _ => None,
        }
    }
}

impl Source {
    pub fn as_db(self) -> &'static str {
        match self {
            Source::WillowSeed => "willow_seed",
            Source::Manual => "manual",
            Source::LearnedHistory => "learned_history",
            Source::LearnedPaste => "learned_paste",
        }
    }
    pub fn from_db(s: &str) -> Option<Self> {
        match s {
            "willow_seed" => Some(Source::WillowSeed),
            "manual" => Some(Source::Manual),
            "learned_history" => Some(Source::LearnedHistory),
            "learned_paste" => Some(Source::LearnedPaste),
            _ => None,
        }
    }
}

impl Status {
    pub fn as_db(self) -> &'static str {
        match self {
            Status::Candidate => "candidate",
            Status::Active => "active",
            Status::Disabled => "disabled",
        }
    }
    pub fn from_db(s: &str) -> Option<Self> {
        match s {
            "candidate" => Some(Status::Candidate),
            "active" => Some(Status::Active),
            "disabled" => Some(Status::Disabled),
            _ => None,
        }
    }
}

/// One row of `dictionary_entries`, as the UI sees it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DictionaryEntry {
    pub id: i64,
    pub kind: EntryKind,
    /// Only for replacements: the text as heard.
    pub from_text: Option<String>,
    /// The term itself, or what a replacement writes.
    pub to_text: String,
    pub source: Source,
    pub status: Status,
    /// How many times this exact correction has been seen (promotion counter).
    pub observations: u32,
    /// How many dictations this entry has changed or biased.
    pub hits: u32,
    /// RFC 3339 UTC.
    pub created_at: String,
}

/// An active replacement rule handed to the text pipeline (spec §6 step 5).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Replacement {
    pub id: i64,
    pub from: String,
    pub to: String,
}

const COMMON_WORDS_TXT: &str = include_str!("../../resources/common-words.txt");

fn common_words() -> &'static HashSet<&'static str> {
    static SET: OnceLock<HashSet<&'static str>> = OnceLock::new();
    SET.get_or_init(|| {
        COMMON_WORDS_TXT
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .collect()
    })
}

/// True when `word` is in the bundled top-20k English list (case-insensitive; surrounding
/// punctuation ignored; a curly apostrophe counts as a straight one). A phrase of several words
/// is common only when every word in it is.
pub fn is_common_word(word: &str) -> bool {
    let words: Vec<String> = word
        .split_whitespace()
        .map(|w| {
            w.trim_matches(|c: char| !c.is_alphanumeric())
                .to_lowercase()
                .replace('\u{2019}', "'")
        })
        .filter(|w| !w.is_empty())
        .collect();
    if words.is_empty() {
        return false;
    }
    let set = common_words();
    words.iter().all(|w| set.contains(w.as_str()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn common_word_list_is_loaded_without_its_header() {
        let set = common_words();
        assert!(set.len() >= 19_000, "only {} words loaded", set.len());
        assert!(!set.iter().any(|w| w.starts_with('#')));
    }

    #[test]
    fn common_words_are_recognised_case_insensitively() {
        assert!(is_common_word("the"));
        assert!(is_common_word("Queue"));
        assert!(is_common_word("stand,"));
        assert!(is_common_word("don\u{2019}t"));
        assert!(is_common_word("in the"));
    }

    #[test]
    fn rare_words_and_blanks_are_not_common() {
        assert!(!is_common_word("Klode"));
        assert!(!is_common_word("Overseer Klode"));
        assert!(!is_common_word(""));
        assert!(!is_common_word("  ...  "));
    }

    #[test]
    fn db_strings_round_trip() {
        for k in [EntryKind::Term, EntryKind::Replacement] {
            assert_eq!(EntryKind::from_db(k.as_db()), Some(k));
        }
        for s in [Source::WillowSeed, Source::Manual, Source::LearnedHistory, Source::LearnedPaste] {
            assert_eq!(Source::from_db(s.as_db()), Some(s));
        }
        for s in [Status::Candidate, Status::Active, Status::Disabled] {
            assert_eq!(Status::from_db(s.as_db()), Some(s));
        }
        assert_eq!(serde_json::to_string(&Source::LearnedPaste).unwrap(), "\"learnedPaste\"");
    }
}
