//! Text pipeline (spec §6): raw model text → the text Wren pastes. Pure functions only; every
//! step has its own table tests in its file, and `run` is tested end to end below.
//!
//! Order: silence guard (speech length, short-clip hallucinations, repetition collapse) →
//! whitespace → fillers (+ orphan commas, re-capitalised sentence starts) → "i" → replacements →
//! optional AI clean-up → replacements again → final spacing. Empty stays empty at every step.

pub mod guards;
pub mod replace;
pub mod tidy;

use crate::constants::AI_CLEANUP_MAX_LENGTH_CHANGE;
use crate::dictionary::Replacement;

pub struct PipelineInput<'a> {
    /// Text exactly as the speech model returned it.
    pub raw: &'a str,
    /// Speech the VAD detected in the clip.
    pub speech_ms: u64,
    /// Length of the whole recording.
    pub clip_ms: u64,
    /// The prompt the model was given (a model can echo it back on silence).
    pub prompt: &'a str,
    /// Active dictionary replacements.
    pub replacements: &'a [Replacement],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PipelineOutput {
    /// After steps 1–5 (guards, fillers, "i", replacements): what is used if the AI step is off or rejected.
    pub tidied: String,
    /// What gets pasted (empty = paste nothing).
    pub final_text: String,
    /// Replacement IDs that changed the text, each once, in first-applied order.
    pub applied_rule_ids: Vec<i64>,
    /// A phrase was repeated more than `REPETITION_MAX_REPEATS` times and was collapsed.
    pub repetition_flagged: bool,
}

/// Runs the whole pipeline. `ai` is the optional clean-up step (spec §10): it gets the tidied text
/// and returns its version, or `None` (off, timed out, failed). The caller enforces the time limit.
#[allow(clippy::type_complexity)] // the signature is the planned interface
pub fn run(input: &PipelineInput, ai: Option<&dyn Fn(&str) -> Option<String>>) -> PipelineOutput {
    let guarded = guards::apply(input.raw, input.speech_ms, input.clip_ms, input.prompt);
    let repetition_flagged = guarded.repetition_flagged;

    let mut text = tidy::normalise_whitespace(&guarded.text);
    text = tidy::remove_fillers(&text);
    text = tidy::capitalise_i(&text);
    let (after_rules, mut applied) = replace::apply(&text, input.replacements);
    let tidied = tidy::final_spacing(&after_rules);

    let mut final_text = tidied.clone();
    if !tidied.is_empty()
        && let Some(ai) = ai
        && let Some(candidate) = ai(&tidied)
    {
        let candidate = tidy::final_spacing(&candidate);
        if ai_result_acceptable(&tidied, &candidate) {
            let (again, applied_again) = replace::apply(&candidate, input.replacements);
            for id in applied_again {
                if !applied.contains(&id) {
                    applied.push(id);
                }
            }
            final_text = tidy::final_spacing(&again);
        }
    }

    PipelineOutput { tidied, final_text, applied_rule_ids: applied, repetition_flagged }
}

/// The AI reply is used only if it is non-empty and its length (in characters) is within
/// `AI_CLEANUP_MAX_LENGTH_CHANGE` of the tidied text.
pub fn ai_result_acceptable(tidied: &str, candidate: &str) -> bool {
    let before = tidied.chars().count() as f32;
    let after = candidate.chars().count() as f32;
    if after == 0.0 || before == 0.0 {
        return false;
    }
    ((after - before).abs() / before) <= AI_CLEANUP_MAX_LENGTH_CHANGE
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::constants::{MIN_SPEECH_MS, SHORT_CLIP_MS};

    fn rule(id: i64, from: &str, to: &str) -> Replacement {
        Replacement { id, from: from.into(), to: to.into() }
    }

    fn input<'a>(raw: &'a str, replacements: &'a [Replacement]) -> PipelineInput<'a> {
        PipelineInput { raw, speech_ms: 5_000, clip_ms: 6_000, prompt: "Glossary: Claude, Wren.", replacements }
    }

    #[test]
    fn end_to_end_tidies_and_replaces() {
        let rules = [rule(7, "clawed", "Claude")];
        let out = run(&input("  um, i think clawed is  great, uh,  really. ", &rules), None);
        // The comma pair around a filler goes with it (see tidy::filler_table).
        assert_eq!(out.tidied, "I think Claude is great really.");
        assert_eq!(out.final_text, out.tidied);
        assert_eq!(out.applied_rule_ids, vec![7]);
        assert!(!out.repetition_flagged);
    }

    #[test]
    fn empty_stays_empty_and_ai_is_not_called() {
        let called = std::cell::Cell::new(false);
        let ai = |_: &str| {
            called.set(true);
            Some("Something".to_string())
        };
        for raw in ["", "   ", "Um.", "uh, um, er"] {
            let out = run(&input(raw, &[]), Some(&ai));
            assert_eq!(out.final_text, "", "{raw:?}");
            assert_eq!(out.tidied, "", "{raw:?}");
        }
        assert!(!called.get());
    }

    #[test]
    fn too_little_speech_is_empty() {
        let mut i = input("Hello there.", &[]);
        i.speech_ms = MIN_SPEECH_MS - 1;
        assert_eq!(run(&i, None).final_text, "");
        i.speech_ms = MIN_SPEECH_MS;
        assert_eq!(run(&i, None).final_text, "Hello there.");
    }

    #[test]
    fn hallucination_dropped_only_on_short_clips() {
        let mut i = input("Thank you.", &[]);
        i.clip_ms = SHORT_CLIP_MS - 1;
        assert_eq!(run(&i, None).final_text, "");
        i.clip_ms = SHORT_CLIP_MS;
        assert_eq!(run(&i, None).final_text, "Thank you.");
    }

    #[test]
    fn repetition_is_collapsed_and_flagged() {
        let out = run(&input("Okay okay okay okay okay okay let's go.", &[]), None);
        assert_eq!(out.final_text, "Okay let's go.");
        assert!(out.repetition_flagged);
    }

    #[test]
    fn ai_step_used_when_close_in_length_and_replacements_reapplied() {
        let rules = [rule(3, "clawed", "Claude"), rule(4, "tory", "Tauri")];
        // The AI "fixes" punctuation but spells Claude the old way again; step 7 puts it right.
        let ai = |t: &str| {
            assert_eq!(t, "I asked Claude about it");
            Some("I asked clawed about it.".to_string())
        };
        let out = run(&input("i asked clawed about it", &rules), Some(&ai));
        assert_eq!(out.tidied, "I asked Claude about it");
        assert_eq!(out.final_text, "I asked Claude about it.");
        assert_eq!(out.applied_rule_ids, vec![3]);
    }

    #[test]
    fn ai_step_rejected_when_length_changes_over_30_percent() {
        let long = |_: &str| Some("Sure! Here is the corrected text: I asked about it.".to_string());
        let out = run(&input("I asked about it.", &[]), Some(&long));
        assert_eq!(out.final_text, "I asked about it.");
        let short = |_: &str| Some("Asked.".to_string());
        assert_eq!(run(&input("I asked about it.", &[]), Some(&short)).final_text, "I asked about it.");
        let empty = |_: &str| Some("   ".to_string());
        assert_eq!(run(&input("I asked about it.", &[]), Some(&empty)).final_text, "I asked about it.");
        let none = |_: &str| None;
        assert_eq!(run(&input("I asked about it.", &[]), Some(&none)).final_text, "I asked about it.");
    }

    #[test]
    fn ai_length_rule_boundaries() {
        let base = "a".repeat(100);
        assert!(ai_result_acceptable(&base, &"a".repeat(130)));
        assert!(!ai_result_acceptable(&base, &"a".repeat(131)));
        assert!(ai_result_acceptable(&base, &"a".repeat(70)));
        assert!(!ai_result_acceptable(&base, &"a".repeat(69)));
        assert!(!ai_result_acceptable(&base, ""));
    }

    #[test]
    fn output_never_ends_in_space_or_newline() {
        let ai = |t: &str| Some(format!("{t}\n"));
        for raw in ["Hello world. ", "Hello world.\n", "Hello\nworld \r\n", "Hello world um"] {
            let out = run(&input(raw, &[]), Some(&ai));
            assert!(!out.final_text.ends_with(char::is_whitespace), "{raw:?} -> {:?}", out.final_text);
            assert!(!out.final_text.contains('\n'), "{raw:?} -> {:?}", out.final_text);
            assert!(!out.tidied.ends_with(char::is_whitespace));
        }
    }
}
