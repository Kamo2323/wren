//! Step 1 — silence guard (spec §6): too little speech → empty; on short clips, the things Whisper
//! says over silence → empty; a phrase repeated more than `REPETITION_MAX_REPEATS` times in a row →
//! collapsed to one copy and flagged.
//!
//! The engine runs with VAD on, so most silence never reaches the model; this is the last line of
//! defence. The phrase list comes from the spec plus what faster-whisper produced on silent clips
//! in the benchmark (docs/research/stt-benchmark.md).

use crate::constants::{
    MIN_SPEECH_MS, PROMPT_ECHO_MIN_WORDS, REPETITION_MAX_PHRASE_WORDS, REPETITION_MAX_REPEATS, SHORT_CLIP_MS,
};

/// Whole outputs (after `normalise_for_match`) that are dropped on short clips.
const HALLUCINATIONS: &[&str] = &[
    "",
    "thank you",
    "thank you for watching",
    "thanks for watching",
    "you",
    "bye",
    "we'll be right back",
    "we will be right back",
];

/// Outputs starting with these words (after `normalise_for_match`) are dropped on short clips.
/// "glossary" is Wren's own prompt ("Glossary: term, term.") read back.
const HALLUCINATION_PREFIXES: &[&str] = &[
    "closed captioning by",
    "captioning by",
    "captions by",
    "subtitles by",
    "transcribed by",
    "glossary",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Guarded {
    pub text: String,
    pub repetition_flagged: bool,
}

/// Runs the whole guard step.
pub fn apply(raw: &str, speech_ms: u64, clip_ms: u64, prompt: &str) -> Guarded {
    if speech_ms < MIN_SPEECH_MS {
        return Guarded { text: String::new(), repetition_flagged: false };
    }
    let (text, repetition_flagged) = collapse_repetitions(raw);
    if clip_ms < SHORT_CLIP_MS && is_hallucination(&text, prompt) {
        return Guarded { text: String::new(), repetition_flagged };
    }
    Guarded { text, repetition_flagged }
}

/// Lower-case, straight apostrophes, everything but letters, digits, apostrophes and spaces dropped,
/// spaces collapsed.
pub fn normalise_for_match(s: &str) -> String {
    let cleaned: String = s
        .chars()
        .map(|c| match c {
            '\u{2019}' | '\u{2018}' => '\'',
            c if c.is_alphanumeric() || c == '\'' => c,
            _ => ' ',
        })
        .collect();
    cleaned.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase()
}

/// True when `text` is a known silence hallucination or an echo of the prompt (short clips only;
/// the caller checks the clip length).
pub fn is_hallucination(text: &str, prompt: &str) -> bool {
    let norm = normalise_for_match(text);
    if HALLUCINATIONS.contains(&norm.as_str()) {
        return true;
    }
    if HALLUCINATION_PREFIXES.iter().any(|p| norm == *p || norm.starts_with(&format!("{p} "))) {
        return true;
    }
    is_prompt_echo(&norm, &normalise_for_match(prompt))
}

/// Both arguments already normalised. The whole prompt (with or without its "glossary" lead
/// word), or a run of at least `PROMPT_ECHO_MIN_WORDS` of its words, is an echo.
fn is_prompt_echo(norm_text: &str, norm_prompt: &str) -> bool {
    if norm_prompt.is_empty() || norm_text.is_empty() {
        return false;
    }
    if norm_text == norm_prompt || norm_prompt.strip_prefix("glossary ") == Some(norm_text) {
        return true;
    }
    let words = norm_text.split(' ').count();
    words >= PROMPT_ECHO_MIN_WORDS && format!(" {norm_prompt} ").contains(&format!(" {norm_text} "))
}

/// Collapses any run of more than `REPETITION_MAX_REPEATS` copies of the same 1–N word phrase
/// (compared ignoring case and punctuation) to its first copy. Returns the text (words joined by
/// single spaces) and whether anything was collapsed.
pub fn collapse_repetitions(text: &str) -> (String, bool) {
    let words: Vec<&str> = text.split_whitespace().collect();
    let keys: Vec<String> = words.iter().map(|w| normalise_for_match(w)).collect();
    let mut out: Vec<String> = Vec::with_capacity(words.len());
    let mut flagged = false;
    let mut i = 0;
    'outer: while i < words.len() {
        for n in 1..=REPETITION_MAX_PHRASE_WORDS {
            if i + n > words.len() {
                break;
            }
            let phrase = &keys[i..i + n];
            let mut copies = 1;
            while i + (copies + 1) * n <= words.len() && keys[i + copies * n..i + (copies + 1) * n] == *phrase {
                copies += 1;
            }
            if copies > REPETITION_MAX_REPEATS {
                let mut kept: Vec<String> = words[i..i + n].iter().map(|w| w.to_string()).collect();
                // The last copy's closing punctuation ends the phrase ("Go, go, go, go, go!" → "Go!").
                let tail = trailing_punctuation(words[i + copies * n - 1]).to_string();
                if let Some(end) = kept.last_mut() {
                    let core_len = end.len() - trailing_punctuation(end).len();
                    end.truncate(core_len);
                    end.push_str(&tail);
                }
                out.extend(kept);
                flagged = true;
                i += copies * n;
                continue 'outer;
            }
        }
        out.push(words[i].to_string());
        i += 1;
    }
    (out.join(" "), flagged)
}

/// The run of non-alphanumeric characters at the end of `word`.
fn trailing_punctuation(word: &str) -> &str {
    let core_end = word
        .char_indices()
        .rev()
        .find(|(_, c)| c.is_alphanumeric())
        .map(|(i, c)| i + c.len_utf8())
        .unwrap_or(0);
    &word[core_end..]
}

#[cfg(test)]
mod tests {
    use super::*;

    const PROMPT: &str = "Glossary: Claude, Claude Code, Overseer, Taroting, ElevenLabs.";

    #[test]
    fn normalise_for_match_table() {
        for (input, want) in [
            ("Thank you.", "thank you"),
            ("  We’ll  be right back! ", "we'll be right back"),
            (".", ""),
            ("...", ""),
            ("Glossary, Claude.", "glossary claude"),
        ] {
            assert_eq!(normalise_for_match(input), want, "{input:?}");
        }
    }

    #[test]
    fn known_hallucinations_are_recognised() {
        for text in [
            "Thank you.",
            "thank you",
            "Thanks for watching!",
            "Thank you for watching.",
            "you",
            "You.",
            "Bye.",
            ".",
            "",
            "We'll be right back.",
            "We will be right back.",
            "Closed Captioning by ABC Media.",
            "Subtitles by the Amara.org community",
            "Glossary, Claude, Wren.",
            "Glossary: Claude, Claude Code, Overseer, Taroting, ElevenLabs.",
            // The prompt text itself, or 3+ words of it.
            "Claude, Claude Code, Overseer, Taroting, ElevenLabs.",
            "Overseer, Taroting, ElevenLabs.",
        ] {
            assert!(is_hallucination(text, PROMPT), "{text:?}");
        }
    }

    #[test]
    fn real_short_dictations_are_kept() {
        for text in [
            "Yes.",
            "Thanks.",
            "Okay, send it.",
            "Thank you so much for that.",
            "You too.",
            "Claude.",               // one prompt word is a real dictation, not an echo
            "Claude Code.",          // two prompt words: still real
            "Ask Claude Code now.",  // prompt words inside other words
            "Your error is here.",
        ] {
            assert!(!is_hallucination(text, PROMPT), "{text:?}");
        }
    }

    #[test]
    fn empty_prompt_never_matches_as_echo() {
        assert!(!is_hallucination("Hello there friend.", ""));
    }

    #[test]
    fn apply_respects_speech_and_clip_lengths() {
        assert_eq!(apply("Hello.", MIN_SPEECH_MS - 1, 10_000, "").text, "");
        assert_eq!(apply("Hello.", MIN_SPEECH_MS, 10_000, "").text, "Hello.");
        assert_eq!(apply("Thank you.", 1_000, SHORT_CLIP_MS - 1, "").text, "");
        assert_eq!(apply("Thank you.", 1_000, SHORT_CLIP_MS, "").text, "Thank you.");
        assert_eq!(apply("Glossary, Claude.", 1_000, SHORT_CLIP_MS - 1, PROMPT).text, "");
        // A long clip that happens to say "you" is kept.
        assert_eq!(apply("you", 1_000, 5_000, "").text, "you");
    }

    #[test]
    fn repeated_hallucination_collapses_then_drops_on_short_clip() {
        let g = apply("Thank you. Thank you. Thank you. Thank you. Thank you.", 800, 1_500, "");
        assert_eq!(g.text, "");
        assert!(g.repetition_flagged);
    }

    #[test]
    fn collapse_table() {
        for (input, want, flagged) in [
            ("no no no no no", "no", true),
            ("no no no no", "no no no no", false), // exactly 4 = allowed
            ("I think I think I think I think I think so", "I think so", true),
            ("Go, go, go, go, go!", "Go!", true), // first copy, last copy's end punctuation
            ("We're going. We're going. We're going. We're going. We're going. Done.", "We're going. Done.", true),
            ("a b c a b c a b c a b c a b c a b c", "a b c", true),
            ("plain sentence with no loops", "plain sentence with no loops", false),
            ("", "", false),
            ("  spaced   out  ", "spaced out", false),
            ("one two one two one two one two", "one two one two one two one two", false),
        ] {
            let (got, f) = collapse_repetitions(input);
            assert_eq!(got, want, "{input:?}");
            assert_eq!(f, flagged, "{input:?}");
        }
    }

    #[test]
    fn collapse_handles_phrases_up_to_the_limit() {
        let phrase: Vec<String> = (0..REPETITION_MAX_PHRASE_WORDS).map(|i| format!("w{i}")).collect();
        let phrase = phrase.join(" ");
        let looped = [phrase.as_str(); REPETITION_MAX_REPEATS + 1].join(" ");
        assert_eq!(collapse_repetitions(&looped), (phrase.clone(), true));
    }
}
