//! Text pipeline (spec §6): raw model text → the text Wren pastes. Pure functions only; every
//! step has its own table tests in its file, and `run` is tested end to end below.
//!
//! Order: silence guard (speech length, short-clip hallucinations, repetition collapse) →
//! whitespace → fillers (+ orphan commas, re-capitalised sentence starts) → "i" → replacements →
//! optional AI clean-up → replacements again → final spacing. Empty stays empty at every step.

pub mod guards;
pub mod replace;
pub mod slash;
pub mod tidy;

use crate::ai_cleanup::AppCategory;
use crate::constants::{
    AI_CLEANUP_LEAD_IN_MAX_CHARS, AI_CLEANUP_MAX_LENGTH_CHANGE, AI_CLEANUP_MIN_WORD_OVERLAP, AI_REWRITE_MAX_LENGTH_RATIO,
    AI_REWRITE_MIN_LENGTH_RATIO, AI_REWRITE_MIN_KEPT, AI_REWRITE_MIN_REPLY_OVERLAP,
};
use crate::dictionary::Replacement;
use crate::settings::CodingStyle;

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
    /// The kind of app the text goes into (slash commands and AI checks depend on it).
    pub target: AppCategory,
    /// How much the AI may change text in coding apps (Settings → AI clean-up).
    pub coding_style: CodingStyle,
}

/// How strictly an AI reply is checked before it is used.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AiCheck {
    /// Words kept: length within 30 %, 80 % word overlap both ways, no lead-in.
    Strict,
    /// Coding "Rewrite as a clear prompt": may drop ums, repeats and false starts and reorder, so
    /// only the reply's own words are checked, and it may be much shorter.
    Rewrite,
}

impl AiCheck {
    pub fn for_target(target: AppCategory, coding_style: CodingStyle) -> Self {
        if target == AppCategory::Coding && coding_style == CodingStyle::Rewrite { AiCheck::Rewrite } else { AiCheck::Strict }
    }
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
    let mut tidied = tidy::final_spacing(&after_rules);
    if input.target == AppCategory::Coding {
        tidied = slash::apply(&tidied);
    }

    // Email paragraphs and coding lists keep the AI's line breaks; other text stays on one line.
    let keep_lines = matches!(input.target, AppCategory::Coding | AppCategory::Email);
    let spacing = |t: &str| if keep_lines { tidy::final_spacing_lines(t) } else { tidy::final_spacing(t) };
    let mut final_text = tidied.clone();
    if !tidied.is_empty()
        && let Some(ai) = ai
        && let Some(candidate) = ai(&tidied)
    {
        // Judged as the model wrote it (before spacing clean-up folds a lead-in's line break away).
        let check = AiCheck::for_target(input.target, input.coding_style);
        let acceptable = ai_reply_acceptable(check, &tidied, candidate.trim());
        if acceptable {
            let (again, applied_again) = replace::apply(&spacing(&candidate), input.replacements);
            for id in applied_again {
                if !applied.contains(&id) {
                    applied.push(id);
                }
            }
            final_text = spacing(&again);
        }
    }

    PipelineOutput { tidied, final_text, applied_rule_ids: applied, repetition_flagged }
}

/// The length rule: the reply is non-empty and its length (in characters) is within
/// `AI_CLEANUP_MAX_LENGTH_CHANGE` of the tidied text.
pub fn ai_length_ok(tidied: &str, candidate: &str) -> bool {
    let before = tidied.chars().count() as f32;
    let after = candidate.chars().count() as f32;
    if after == 0.0 || before == 0.0 {
        return false;
    }
    ((after - before).abs() / before) <= AI_CLEANUP_MAX_LENGTH_CHANGE
}

fn is_apostrophe(c: char) -> bool {
    c == '\'' || c == '\u{2019}'
}

/// Lower-cased words (letters, digits and inner apostrophes).
fn ai_words(s: &str) -> Vec<String> {
    s.split(|c: char| !(c.is_alphanumeric() || is_apostrophe(c)))
        .map(|w| w.trim_matches(is_apostrophe).replace('\u{2019}', "'").to_lowercase())
        .filter(|w| !w.is_empty())
        .collect()
}

/// The word rule: at least `AI_CLEANUP_MIN_WORD_OVERLAP` of the tidied text's words are still in
/// the reply, and as large a share of the reply's words come from the tidied text (counted with
/// repeats, ignoring case).
pub fn ai_words_kept(tidied: &str, candidate: &str) -> bool {
    let (a, b) = (ai_words(tidied), ai_words(candidate));
    if a.is_empty() || b.is_empty() {
        return a.is_empty() && b.is_empty();
    }
    let mut pool: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    for w in &a {
        *pool.entry(w.as_str()).or_default() += 1;
    }
    let mut common = 0usize;
    for w in &b {
        if let Some(n) = pool.get_mut(w.as_str())
            && *n > 0
        {
            *n -= 1;
            common += 1;
        }
    }
    let kept = common as f32 / a.len() as f32;
    let own = common as f32 / b.len() as f32;
    kept >= AI_CLEANUP_MIN_WORD_OVERLAP && own >= AI_CLEANUP_MIN_WORD_OVERLAP
}

/// The reply opens with chatter ("Here is the corrected transcript:", "Sure!" + line break): words
/// before a ':' or line break near the start that the tidied text does not start with.
pub fn ai_lead_in(tidied: &str, candidate: &str) -> bool {
    let head: String = candidate.trim_start().chars().take(AI_CLEANUP_LEAD_IN_MAX_CHARS).collect();
    let Some(pos) = head.find([':', '\n']) else { return false };
    let lead = ai_words(&head[..pos]);
    if lead.is_empty() {
        return false;
    }
    let start = ai_words(tidied);
    !(start.len() >= lead.len() && start[..lead.len()] == lead[..])
}

/// The AI reply is used only if it passes the length rule and the word rule and has no lead-in.
pub fn ai_result_acceptable(tidied: &str, candidate: &str) -> bool {
    ai_length_ok(tidied, candidate) && ai_words_kept(tidied, candidate) && !ai_lead_in(tidied, candidate)
}

/// Share of the reply's words (counted with repeats, ignoring case) that come from the tidied text.
fn reply_overlap(tidied: &str, candidate: &str) -> f32 {
    let (a, b) = (ai_words(tidied), ai_words(candidate));
    if b.is_empty() {
        return 0.0;
    }
    let mut pool: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    for w in &a {
        *pool.entry(w.as_str()).or_default() += 1;
    }
    let common = b
        .iter()
        .filter(|w| match pool.get_mut(w.as_str()) {
            Some(n) if *n > 0 => {
                *n -= 1;
                true
            }
            _ => false,
        })
        .count();
    common as f32 / b.len() as f32
}

/// The looser check for a coding "Rewrite as a clear prompt" reply: not empty; between
/// `AI_REWRITE_MIN_LENGTH_RATIO` and `AI_REWRITE_MAX_LENGTH_RATIO` of the tidied length (ums,
/// repeats and false starts may go; backticks and list marks may be added); at least
/// `AI_REWRITE_MIN_REPLY_OVERLAP` of the reply's words come from the tidied text (nothing new was
/// written); and no lead-in.
pub fn ai_rewrite_acceptable(tidied: &str, candidate: &str) -> bool {
    let before = tidied.chars().count() as f32;
    let after = candidate.chars().count() as f32;
    if before == 0.0 || after == 0.0 {
        return false;
    }
    let ratio = after / before;
    (AI_REWRITE_MIN_LENGTH_RATIO..=AI_REWRITE_MAX_LENGTH_RATIO).contains(&ratio)
        && reply_overlap(tidied, candidate) >= AI_REWRITE_MIN_REPLY_OVERLAP
        && dictation_kept(tidied, candidate) >= AI_REWRITE_MIN_KEPT
        && !rewrite_lead_in(tidied, candidate)
}

/// Share of the dictation's words still in the reply; a word said twice in a row ("the the") counts once.
fn dictation_kept(tidied: &str, candidate: &str) -> f32 {
    let mut a = ai_words(tidied);
    a.dedup();
    if a.is_empty() {
        return 0.0;
    }
    let mut pool: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for w in ai_words(candidate) {
        *pool.entry(w).or_default() += 1;
    }
    let kept = a
        .iter()
        .filter(|w| match pool.get_mut(w.as_str()) {
            Some(n) if *n > 0 => {
                *n -= 1;
                true
            }
            _ => false,
        })
        .count();
    kept as f32 / a.len() as f32
}

/// Words that open an assistant's reply, not a dictation.
const ACK_OPENERS: [&str; 11] = ["sure", "okay", "ok", "certainly", "absolutely", "here", "here's", "i'll", "alright", "understood", "noted"];
/// Two-word openers, matched as a pair: "of" and "got" alone are everyday words.
const ACK_OPENER_PAIRS: [[&str; 2]; 2] = [["of", "course"], ["got", "it"]];

/// The reply opens like an answer to the dictation ("Sure, I'll add…", "Here's…") with a word
/// that was never said ("the logs are attached here" → "Here are the logs…" is fine).
fn acknowledges(tidied: &str, candidate: &str) -> bool {
    let (reply, said) = (ai_words(candidate), ai_words(tidied));
    let Some(first) = reply.first() else { return false };
    if ACK_OPENERS.contains(&first.as_str()) {
        return !said.contains(first);
    }
    ACK_OPENER_PAIRS.iter().any(|pair| {
        reply.len() >= 2 && reply[0] == pair[0] && reply[1] == pair[1] && !said.windows(2).any(|w| w[0] == pair[0] && w[1] == pair[1])
    })
}

/// Requests that wrap a question ("can you tell me what…", "do you know why…").
const EMBEDDED_QUESTION_LEADS: [&[&str]; 3] = [&["tell", "me"], &["do", "you", "know"], &["explain"]];

const QUESTION_WORDS: [&str; 12] = ["what", "what's", "why", "how", "how's", "when", "where", "where's", "who", "who's", "which", "whose"];

/// The dictation asks a what/why/how… question and the reply has no question mark: likely an
/// answer. Also a wrapped question ("tell me what the capital of France is") whose request words
/// the reply dropped ("The capital of France is Paris.").
fn answers_question(tidied: &str, candidate: &str) -> bool {
    if candidate.contains('?') {
        return false;
    }
    let (said, reply) = (ai_words(tidied), ai_words(candidate));
    for lead in EMBEDDED_QUESTION_LEADS {
        let asked = said
            .windows(lead.len() + 1)
            .any(|w| w[..lead.len()].iter().zip(lead.iter()).all(|(a, b)| a == b) && QUESTION_WORDS.contains(&w[lead.len()].as_str()));
        if asked && !reply.windows(lead.len()).any(|w| w.iter().zip(lead.iter()).all(|(a, b)| a == b)) {
            return true;
        }
    }
    let mut sentence = String::new();
    for c in tidied.chars() {
        if c == '?' {
            if ai_words(&sentence).first().is_some_and(|w| QUESTION_WORDS.contains(&w.as_str())) {
                return true;
            }
            sentence.clear();
        } else if matches!(c, '.' | '!' | '\n') {
            sentence.clear();
        } else {
            sentence.push(c);
        }
    }
    false
}

/// A rewrite may reorder, so its opening words need not match the dictation's; a lead-in is words
/// before a ':' or line break near the start that mostly did not come from the dictation
/// ("Here is the cleaned-up prompt:", "Sure!").
fn rewrite_lead_in(tidied: &str, candidate: &str) -> bool {
    let head: String = candidate.trim_start().chars().take(AI_CLEANUP_LEAD_IN_MAX_CHARS).collect();
    let Some(pos) = head.find([':', '\n']) else { return false };
    !ai_words(&head[..pos]).is_empty() && reply_overlap(tidied, &head[..pos]) < 0.5
}

/// The check for the target's mode, plus: every `/command` in the tidied text is still in the reply.
pub fn ai_reply_acceptable(check: AiCheck, tidied: &str, candidate: &str) -> bool {
    let commands_kept = slash::commands(tidied).iter().all(|c| candidate.contains(c));
    commands_kept
        && !acknowledges(tidied, candidate)
        && !answers_question(tidied, candidate)
        && match check {
            AiCheck::Strict => ai_result_acceptable(tidied, candidate),
            AiCheck::Rewrite => ai_rewrite_acceptable(tidied, candidate),
        }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::constants::{MIN_SPEECH_MS, SHORT_CLIP_MS};

    fn rule(id: i64, from: &str, to: &str) -> Replacement {
        Replacement { id, from: from.into(), to: to.into() }
    }

    fn input<'a>(raw: &'a str, replacements: &'a [Replacement]) -> PipelineInput<'a> {
        PipelineInput {
            raw,
            speech_ms: 5_000,
            clip_ms: 6_000,
            prompt: "Glossary: Claude, Wren.",
            replacements,
            target: AppCategory::General,
            coding_style: CodingStyle::Rewrite,
        }
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
        assert!(ai_length_ok(&base, &"a".repeat(130)));
        assert!(!ai_length_ok(&base, &"a".repeat(131)));
        assert!(ai_length_ok(&base, &"a".repeat(70)));
        assert!(!ai_length_ok(&base, &"a".repeat(69)));
        assert!(!ai_length_ok(&base, ""));
        assert!(!ai_result_acceptable(&base, ""));
    }

    #[test]
    fn ai_reply_must_keep_the_words_and_add_no_lead_in() {
        let long = "I think we should ship the new build to the testers on Friday after lunch and then tell the whole team about the plan in the morning meeting";
        // A chatty lead-in on a long text slips past the 30 % length rule…
        let chatty = format!("Here is the corrected transcript: {long}.");
        assert!(ai_length_ok(long, &chatty));
        // …but not past the word rules.
        assert!(!ai_result_acceptable(long, &chatty));
        assert!(!ai_result_acceptable(long, &format!("Sure!\n{long}.")));
        // A reply instead of the text.
        assert!(!ai_result_acceptable("can you send it to sam", "Sure, I will send it to Sam."));
        // Real clean-ups pass: punctuation, capitals, one mis-hearing fixed.
        assert!(ai_result_acceptable(long, &format!("{}.", long.replace("I think", "I think,"))));
        assert!(ai_result_acceptable("I asked clawed about it", "I asked Claude about it."));
        assert!(ai_result_acceptable("dear sam thanks for the notes", "Dear Sam: thanks for the notes."), "a colon the text itself needs");
        assert!(ai_result_acceptable("what is the capital of france", "What is the capital of France?"));
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

    fn in_app<'a>(raw: &'a str, target: AppCategory, coding_style: CodingStyle) -> PipelineInput<'a> {
        PipelineInput { target, coding_style, ..input(raw, &[]) }
    }

    #[test]
    fn slash_commands_only_in_coding_apps_and_without_ai() {
        let coding = in_app("Slash code review.", AppCategory::Coding, CodingStyle::Rewrite);
        assert_eq!(run(&coding, None).final_text, "/code-review");
        assert_eq!(run(&coding, None).tidied, "/code-review", "the AI sees the command too");
        for target in [AppCategory::General, AppCategory::Email, AppCategory::Messaging] {
            assert_eq!(run(&in_app("Slash code review.", target, CodingStyle::Rewrite), None).final_text, "Slash code review.");
        }
        let out = run(&in_app("slash full send, then file the PR", AppCategory::Coding, CodingStyle::Tidy), None);
        assert_eq!(out.final_text, "/full-send, then file the PR");
    }

    #[test]
    fn coding_rewrite_accepts_a_tighter_prompt() {
        let said = "um so can you uh can you fix the paste bug in paste mod dot rs it's it's dropping the last word and then run the tests";
        let rewrite = "Can you fix the paste bug in `paste/mod.rs`? It's dropping the last word.\n- Then run the tests.";
        let ai = |_: &str| Some(rewrite.to_string());
        let out = run(&in_app(said, AppCategory::Coding, CodingStyle::Rewrite), Some(&ai));
        assert_eq!(out.final_text, rewrite, "kept, line break included");
        // The same reply is too far from the words for the strict modes.
        for (target, style) in [
            (AppCategory::Coding, CodingStyle::Tidy),
            (AppCategory::General, CodingStyle::Rewrite),
            (AppCategory::Email, CodingStyle::Rewrite),
            (AppCategory::Messaging, CodingStyle::Rewrite),
        ] {
            assert_eq!(run(&in_app(said, target, style), Some(&ai)).final_text, run(&in_app(said, target, style), None).final_text, "{target:?}/{style:?}");
        }
    }

    #[test]
    fn coding_rewrite_still_rejects_answers_lead_ins_and_new_content() {
        let said = "can you add a dark mode toggle to the settings page and make it remember the choice";
        let check = |reply: &'static str| {
            let ai = move |_: &str| Some(reply.to_string());
            run(&in_app(said, AppCategory::Coding, CodingStyle::Rewrite), Some(&ai)).final_text
        };
        let plain = run(&in_app(said, AppCategory::Coding, CodingStyle::Rewrite), None).final_text;
        // Accepted: reordered and listed.
        assert_eq!(
            check("Can you add a dark mode toggle to the settings page:\n- Make it remember the choice."),
            "Can you add a dark mode toggle to the settings page:\n- Make it remember the choice."
        );
        // A chatty lead-in.
        assert_eq!(check("Here is the cleaned-up prompt:\nCan you add a dark mode toggle to the settings page and make it remember the choice?"), plain);
        // Carrying out the request instead of tidying it.
        assert_eq!(check("Sure! Here's a React component with a toggle that stores the theme in localStorage and applies a dark class."), plain);
        // Far too short.
        assert_eq!(check("Add dark mode."), plain);
        // Acknowledging the request (review S1): the words match, but it's a reply, not the dictation.
        assert_eq!(check("Sure, I'll add a dark mode toggle to the settings page and make it remember the choice."), plain);
        assert_eq!(check("Okay. Add a dark mode toggle to the settings page and make it remember the choice."), plain);
        // "Of course" / "Got it" as a pair, though "of" and "got" were said (review round 3, M1).
        let said = "add a list of the open files to the sidebar now that we got the tree working";
        let ai = |_: &str| Some("Of course. Add a list of the open files to the sidebar now that we got the tree working.".to_string());
        assert_eq!(run(&in_app(said, AppCategory::Coding, CodingStyle::Rewrite), Some(&ai)).final_text, run(&in_app(said, AppCategory::Coding, CodingStyle::Rewrite), None).final_text);
        let ai = |_: &str| Some("Got it. Add a list of the open files to the sidebar now that we got the tree working.".to_string());
        assert_eq!(run(&in_app(said, AppCategory::Coding, CodingStyle::Rewrite), Some(&ai)).final_text, run(&in_app(said, AppCategory::Coding, CodingStyle::Rewrite), None).final_text);
        // A curly apostrophe doesn't hide the opener (review round 2, N3).
        assert_eq!(check("Here\u{2019}s the prompt: can you add a dark mode toggle to the settings page and make it remember the choice"), plain);
        // Answering the dictation (review S1): most of what was said is gone.
        let said = "can you tell me what the capital of France is";
        let ai = |_: &str| Some("The capital of France is Paris.".to_string());
        let out = run(&in_app(said, AppCategory::Coding, CodingStyle::Rewrite), Some(&ai));
        assert_eq!(out.final_text, run(&in_app(said, AppCategory::Coding, CodingStyle::Rewrite), None).final_text);
        // A slash command the AI dropped.
        let ai = |_: &str| Some("Compact the conversation.".to_string());
        assert_eq!(run(&in_app("slash compact", AppCategory::Coding, CodingStyle::Rewrite), Some(&ai)).final_text, "/compact");
    }

    #[test]
    fn coding_rewrite_keeps_good_rewrites_of_rambling_speech() {
        // Review round 2: N1 (filler-heavy dictation), N2 (an opener word that was said).
        for (said, reply) in [
            ("so basically what I kind of want is like for the app to you know remember the window size", "I want the app to remember the window size."),
            ("the logs are attached here can you check them", "Here are the logs, can you check them?"),
            ("the tests keep failing got any ideas", "Got any ideas why the tests keep failing?"),
            ("can you tell me what the paste module does", "Tell me what the paste module does."),
        ] {
            let ai = move |_: &str| Some(reply.to_string());
            assert_eq!(run(&in_app(said, AppCategory::Coding, CodingStyle::Rewrite), Some(&ai)).final_text, reply, "{said}");
        }
    }

    #[test]
    fn an_answer_to_a_spoken_question_is_not_pasted() {
        // Review S5: the strict word rule alone lets "Two plus two is four." through.
        for (target, style) in [(AppCategory::General, CodingStyle::Rewrite), (AppCategory::Coding, CodingStyle::Rewrite), (AppCategory::Coding, CodingStyle::Tidy)] {
            let said = "what is two plus two?";
            let ai = |_: &str| Some("Two plus two is four.".to_string());
            let out = run(&in_app(said, target, style), Some(&ai));
            assert_eq!(out.final_text, run(&in_app(said, target, style), None).final_text, "{target:?}/{style:?}");
        }
        // A question kept as a question is fine.
        let said = "what's causing the paste bug? can you check the paste module";
        let reply = "What's causing the paste bug? Can you check the paste module?";
        let ai = |_: &str| Some(reply.to_string());
        assert_eq!(run(&in_app(said, AppCategory::Coding, CodingStyle::Rewrite), Some(&ai)).final_text, reply);
    }

    #[test]
    fn email_keeps_paragraph_breaks_and_strict_checks() {
        let said = "hi sam thanks for sending the notes over i'll read them tonight best max";
        let reply = "Hi Sam,\n\nThanks for sending the notes over. I'll read them tonight.\n\nBest,\nMax";
        let ai = |_: &str| Some(reply.to_string());
        assert_eq!(run(&in_app(said, AppCategory::Email, CodingStyle::Rewrite), Some(&ai)).final_text, reply);
        // Added content fails the strict word rule.
        let padded = |_: &str| Some("Hi Sam,\n\nThanks so much for sending the notes over, I really appreciate it. I'll read them tonight.\n\nBest,\nMax".to_string());
        assert_eq!(
            run(&in_app(said, AppCategory::Email, CodingStyle::Rewrite), Some(&padded)).final_text,
            run(&in_app(said, AppCategory::Email, CodingStyle::Rewrite), None).final_text
        );
        // Messaging and General fold line breaks.
        let lines = |_: &str| Some("Hi Sam, thanks for sending the notes over.\nI'll read them tonight. Best, Max".to_string());
        let out = run(&in_app(said, AppCategory::Messaging, CodingStyle::Rewrite), Some(&lines)).final_text;
        assert_eq!(out, "Hi Sam, thanks for sending the notes over. I'll read them tonight. Best, Max");
    }

    #[test]
    fn coding_tidy_keeps_words_and_allows_formatting() {
        let said = "run npm run build then node scripts slash unit tests dot mjs";
        let reply = "Run `npm run build`, then `node scripts/unit-tests.mjs`.";
        let ai = |_: &str| Some(reply.to_string());
        // "slash" mid-sentence is not a command; the AI formats the command and the path, and the
        // spoken "slash" / "dot" it turned into symbols stay within the strict word rule.
        let out = run(&in_app(said, AppCategory::Coding, CodingStyle::Tidy), Some(&ai));
        assert_eq!(out.tidied, "run npm run build then node scripts slash unit tests dot mjs");
        assert_eq!(out.final_text, reply);
        // A reply that drops words is too much for Tidy.
        let ai = |_: &str| Some("Run the build and the unit tests.".to_string());
        assert_eq!(run(&in_app(said, AppCategory::Coding, CodingStyle::Tidy), Some(&ai)).final_text, said);
        let said = "can you check the build log and tell me why it failed";
        let reply = "Can you check the build log and tell me why it failed?";
        let ai = |_: &str| Some(reply.to_string());
        assert_eq!(run(&in_app(said, AppCategory::Coding, CodingStyle::Tidy), Some(&ai)).final_text, reply);
    }
}
