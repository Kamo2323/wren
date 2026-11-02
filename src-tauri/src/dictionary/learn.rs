//! Correction diff (spec §8): turn "what Wren pasted" + "what the user left in the box" into the
//! few word swaps worth learning. Pure, shared by the History "fix a word" path and the watcher.

use serde::{Deserialize, Serialize};

/// One learnable swap: `from` as Wren wrote it, `to` as the user corrected it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Correction {
    pub from: String,
    pub to: String,
}

/// What `Store::learn` did with a correction (drives the "Learned: X → Y · Undo" toast).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "reason", rename_all = "camelCase")]
pub enum LearnDecision {
    /// Only the term was new (the replacement was already known and active).
    AddedTerm,
    /// The replacement is now active (and the term too).
    AddedReplacement,
    /// Seen, but the replacement waits for a second identical sighting.
    CandidateObserved,
    /// Nothing learned; the reason is for logs and tests (never contains dictated text).
    Ignored(String),
}

/// Longest run of words (on either side) a single correction may cover.
const MAX_RUN_WORDS: usize = 3;
/// More separate edits than this means the user rewrote the text: learn nothing.
const MAX_EDITS: usize = 2;
/// Normalised edit distance at or below which two spellings count as "spelt alike".
const MAX_SPELLING_DISTANCE: f64 = 0.4;
/// Double Metaphone code length; longer than the crate default (4) so long words must match fully.
const METAPHONE_CODE_LEN: usize = 12;

/// A word as written (punctuation trimmed, curly apostrophe made straight) and the same word
/// lower-cased. The diff runs on `shown`, so a case fix next to a real fix joins the same run
/// ("clawed code" → "Claude Code"); runs that differ only in case are dropped afterwards.
struct Token {
    shown: String,
    key: String,
    /// First word of the text or of a sentence (its capital letter says nothing about the word).
    sentence_start: bool,
}

fn tokens(text: &str) -> Vec<Token> {
    let mut out = Vec::new();
    let mut sentence_start = true;
    for raw in text.split_whitespace() {
        let shown = raw.trim_matches(|c: char| !c.is_alphanumeric());
        let ends_sentence = raw.trim_end_matches(['"', '\'', ')', '\u{201D}']).ends_with(['.', '!', '?']);
        if !shown.is_empty() {
            let shown = shown.replace('\u{2019}', "'");
            out.push(Token { key: shown.to_lowercase(), shown, sentence_start });
            sentence_start = false;
        } // a lone dash or full stop is punctuation, not a word
        if ends_sentence {
            sentence_start = true;
        }
    }
    out
}

/// Drop a case-only pair at either end of a run when it is just sentence capitalisation
/// ("hello klode" → "Hello Claude" teaches "klode" → "Claude", not the greeting).
fn trim_sentence_case(old: &[Token], new: &[Token], o: &mut std::ops::Range<usize>, n: &mut std::ops::Range<usize>) {
    while o.len() > 1 && n.len() > 1 && old[o.start].key == new[n.start].key && old[o.start].sentence_start {
        o.start += 1;
        n.start += 1;
    }
    while o.len() > 1
        && n.len() > 1
        && old[o.end - 1].key == new[n.end - 1].key
        && old[o.end - 1].sentence_start
    {
        o.end -= 1;
        n.end -= 1;
    }
}

fn squash(words: &[Token]) -> String {
    words.iter().map(|t| t.key.as_str()).collect::<String>()
}

fn sounds_alike(a: &str, b: &str) -> bool {
    let dm = rphonetic::DoubleMetaphone::new(Some(METAPHONE_CODE_LEN));
    let (ra, rb) = (dm.double_metaphone(a), dm.double_metaphone(b));
    let (pa, aa, pb, ab) = (ra.primary(), ra.alternate(), rb.primary(), rb.alternate());
    if pa.is_empty() || pb.is_empty() {
        return false;
    }
    pa == pb || pa == ab || aa == pb || (!aa.is_empty() && aa == ab)
}

fn spelt_alike(a: &str, b: &str) -> bool {
    1.0 - strsim::normalized_levenshtein(a, b) <= MAX_SPELLING_DISTANCE
}

/// Spec §8: accept only 1–3 word runs replaced by 1–3 words with unchanged neighbours, where the
/// old and new words sound alike (Double Metaphone) or are spelt alike (normalised edit distance
/// ≤ 0.4). Case-only and punctuation-only edits, pure insertions/deletions and anything with more
/// than two separate edits (a rewrite) give nothing.
pub fn diff_corrections(pasted: &str, edited: &str) -> Vec<Correction> {
    let old = tokens(pasted);
    let new = tokens(edited);
    if old.is_empty() || new.is_empty() {
        return Vec::new();
    }
    let old_keys: Vec<&str> = old.iter().map(|t| t.shown.as_str()).collect();
    let new_keys: Vec<&str> = new.iter().map(|t| t.shown.as_str()).collect();
    let ops = similar::capture_diff_slices(similar::Algorithm::Myers, &old_keys, &new_keys);

    // Merge each stretch of non-equal ops into one edit (old range, new range).
    let mut edits: Vec<(std::ops::Range<usize>, std::ops::Range<usize>)> = Vec::new();
    let mut open: Option<(std::ops::Range<usize>, std::ops::Range<usize>)> = None;
    for op in &ops {
        let (tag, o, n) = op.as_tag_tuple();
        if tag == similar::DiffTag::Equal {
            if let Some(e) = open.take() {
                edits.push(e);
            }
            continue;
        }
        open = Some(match open.take() {
            None => (o, n),
            Some((po, pn)) => (po.start..o.end.max(po.end), pn.start..n.end.max(pn.end)),
        });
    }
    if let Some(e) = open.take() {
        edits.push(e);
    }
    // A run that differs only in case is no edit at all (spec: ignore case-only changes).
    for (o, n) in edits.iter_mut() {
        trim_sentence_case(&old, &new, o, n);
    }
    edits.retain(|(o, n)| {
        !old[o.clone()].iter().map(|t| &t.key).eq(new[n.clone()].iter().map(|t| &t.key))
    });
    if edits.len() > MAX_EDITS {
        return Vec::new();
    }

    edits
        .into_iter()
        .filter_map(|(o, n)| {
            if o.is_empty() || n.is_empty() || o.len() > MAX_RUN_WORDS || n.len() > MAX_RUN_WORDS {
                return None; // insertion, deletion, or too long a run
            }
            let (from_words, to_words) = (&old[o], &new[n]);
            let (a, b) = (squash(from_words), squash(to_words));
            if a == b || !(sounds_alike(&a, &b) || spelt_alike(&a, &b)) {
                return None;
            }
            let join = |ws: &[Token]| ws.iter().map(|t| t.shown.as_str()).collect::<Vec<_>>().join(" ");
            Some(Correction { from: join(from_words), to: join(to_words) })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(from: &str, to: &str) -> Correction {
        Correction { from: from.into(), to: to.into() }
    }

    #[test]
    fn single_word_sound_alike_swap_is_learned() {
        assert_eq!(
            diff_corrections("stand in the Q", "stand in the queue"),
            vec![c("Q", "queue")]
        );
        assert_eq!(
            diff_corrections("Klode is great", "Claude is great"),
            vec![c("Klode", "Claude")]
        );
    }

    #[test]
    fn spelt_alike_swap_is_learned() {
        assert_eq!(
            diff_corrections("send it to Maximillian today", "send it to Maximilian today"),
            vec![c("Maximillian", "Maximilian")]
        );
    }

    #[test]
    fn multi_word_runs_up_to_three_are_learned() {
        assert_eq!(
            diff_corrections("open clawed code now", "open Claude Code now"),
            vec![c("clawed code", "Claude Code")]
        );
        assert_eq!(
            diff_corrections("I use tower ee daily", "I use Tauri daily"),
            vec![c("tower ee", "Tauri")]
        );
    }

    #[test]
    fn surrounding_punctuation_is_not_part_of_the_learned_words() {
        assert_eq!(
            diff_corrections("Ask Klode, then wait.", "Ask Claude, then wait."),
            vec![c("Klode", "Claude")]
        );
    }

    #[test]
    fn sentence_capitals_next_to_a_fix_are_not_learned() {
        assert_eq!(
            diff_corrections("hello klode, how are you", "Hello Claude, how are you?"),
            vec![c("klode", "Claude")]
        );
        assert_eq!(
            diff_corrections("thanks klode. see you", "thanks Claude. See you"),
            vec![c("klode", "Claude")]
        );
    }

    #[test]
    fn two_separate_edits_are_both_learned() {
        assert_eq!(
            diff_corrections("Klode and Maximillian met", "Claude and Maximilian met"),
            vec![c("Klode", "Claude"), c("Maximillian", "Maximilian")]
        );
    }

    #[test]
    fn edits_that_teach_nothing_give_no_corrections() {
        let none: Vec<Correction> = vec![];
        // identical
        assert_eq!(diff_corrections("hello there", "hello there"), none);
        // case only
        assert_eq!(diff_corrections("send it to claude", "send it to Claude"), none);
        // punctuation only
        assert_eq!(diff_corrections("hello there how are you", "Hello there, how are you?"), none);
        // pure insertion
        assert_eq!(diff_corrections("stand in the queue", "stand in the long queue"), none);
        // pure deletion
        assert_eq!(diff_corrections("stand in the long queue", "stand in the queue"), none);
        // 3+ separate edits = a rewrite
        assert_eq!(
            diff_corrections(
                "Klode met Maximillian at the cafe on Tuesday",
                "Claude met Maximilian at the café on Thursday"
            ),
            none
        );
        // neither sounds nor looks alike
        assert_eq!(diff_corrections("I like the cat", "I like the dog"), none);
        // run longer than three words
        assert_eq!(
            diff_corrections("we went one two three four home", "we went five six seven eight home"),
            none
        );
        // empty sides
        assert_eq!(diff_corrections("", "hello"), none);
        assert_eq!(diff_corrections("hello", ""), none);
    }

    #[test]
    fn whitespace_differences_are_ignored() {
        assert_eq!(
            diff_corrections("Klode  is\ngreat", "Claude is great"),
            vec![c("Klode", "Claude")]
        );
    }
}
