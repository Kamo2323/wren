//! Slash commands in coding apps (brief 2026-11-26): "slash compact" → `/compact`, "slash code
//! review" → `/code-review`. Deterministic, so it works with AI clean-up off or slow. Only used when
//! the target is a Coding app.
//!
//! Rules: "slash" must start the text or a sentence (after `.`, `!`, `?` or a line break). The name
//! is the words after it up to the next punctuation, a clause word ("then", "and", "please", "now",
//! "so") or `SLASH_MAX_NAME_WORDS` words, lower-cased and joined with hyphens. "slash" with no name
//! after it stays as it is. A full stop straight after a command that ends the text is dropped
//! (`/compact.` would not run as a command).

use crate::constants::SLASH_MAX_NAME_WORDS;

/// Words that start a new clause rather than continue a command name.
const CLAUSE_WORDS: [&str; 5] = ["then", "and", "please", "now", "so"];

/// A name word: letters, digits, inner hyphens/underscores. Returns it and any punctuation glued
/// to its end ("send," → ("send", ",")); None if the token is not a plain word.
fn split_word(token: &str) -> Option<(&str, &str)> {
    let end = token.trim_end_matches(|c: char| !(c.is_alphanumeric() || c == '-' || c == '_')).len();
    let (word, punct) = token.split_at(end);
    if word.is_empty() || !word.chars().all(|c| c.is_alphanumeric() || c == '-' || c == '_') {
        return None;
    }
    Some((word, punct))
}

fn ends_sentence(prev: &str) -> bool {
    prev.ends_with(['.', '!', '?'])
}

/// Turns spoken slash commands into `/name` in one line of text.
fn line(text: &str) -> String {
    let tokens: Vec<&str> = text.split(' ').collect();
    let mut out: Vec<String> = Vec::with_capacity(tokens.len());
    let mut i = 0;
    while i < tokens.len() {
        let tok = tokens[i];
        let at_start = out.iter().all(|t| t.is_empty()) || out.last().is_some_and(|p| ends_sentence(p));
        if at_start && tok.eq_ignore_ascii_case("slash") {
            let mut name: Vec<String> = Vec::new();
            let mut tail = "";
            let mut j = i + 1;
            while j < tokens.len() && name.len() < SLASH_MAX_NAME_WORDS {
                let Some((word, punct)) = split_word(tokens[j]) else { break };
                if !name.is_empty() && CLAUSE_WORDS.contains(&word.to_lowercase().as_str()) {
                    break;
                }
                name.push(word.to_lowercase());
                j += 1;
                if !punct.is_empty() {
                    tail = punct;
                    break;
                }
            }
            if !name.is_empty() {
                let mut cmd = format!("/{}{tail}", name.join("-"));
                if tail == "." && tokens[j..].iter().all(|t| t.is_empty()) {
                    cmd.pop();
                }
                out.push(cmd);
                i = j;
                continue;
            }
        }
        out.push(tok.to_string());
        i += 1;
    }
    out.join(" ")
}

/// Spoken slash commands → `/name`, line by line.
pub fn apply(text: &str) -> String {
    text.split('\n').map(line).collect::<Vec<_>>().join("\n")
}

/// The `/name` commands in `text` (to check an AI reply kept them).
pub fn commands(text: &str) -> Vec<&str> {
    text.split_whitespace()
        .filter(|t| t.starts_with('/') && t.len() > 1)
        .map(|t| t.trim_end_matches(|c: char| !(c.is_alphanumeric() || c == '-' || c == '_')))
        .filter(|t| t.len() > 1 && t[1..].chars().next().is_some_and(char::is_alphanumeric))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slash_table() {
        for (said, want) in [
            // The brief's examples.
            ("slash compact", "/compact"),
            ("slash code review", "/code-review"),
            ("slash full send, then file the PR", "/full-send, then file the PR"),
            // Whisper capitalises and punctuates.
            ("Slash compact.", "/compact"),
            ("Slash code review.", "/code-review"),
            ("Slash full send, then file the PR.", "/full-send, then file the PR."),
            ("Slash review PR please.", "/review-pr please."),
            ("Slash full send then file the PR.", "/full-send then file the PR."),
            ("Slash compact now.", "/compact now."),
            // At the start of a later sentence.
            ("That's done. Slash compact.", "That's done. /compact"),
            ("Looks good! slash code review and then merge.", "Looks good! /code-review and then merge."),
            // Name capped at four words.
            ("slash one two three four five", "/one-two-three-four five"),
            ("slash v2 deploy", "/v2-deploy"),
            ("slash code-review", "/code-review"),
            // Mid-sentence "slash" is left alone.
            ("Use and slash or here.", "Use and slash or here."),
            ("Put a slash between them.", "Put a slash between them."),
            ("I said slash compact earlier.", "I said slash compact earlier."),
            // No name: unchanged.
            ("slash", "slash"),
            ("Slash.", "Slash."),
            ("Slash, I mean the character.", "Slash, I mean the character."),
            ("slash /compact", "slash /compact"),
            // Already a command, or empty.
            ("/compact", "/compact"),
            ("", ""),
        ] {
            assert_eq!(apply(said), want, "{said:?}");
        }
    }

    #[test]
    fn each_line_is_its_own_start() {
        assert_eq!(apply("Fix the build\nslash compact"), "Fix the build\n/compact");
    }

    #[test]
    fn commands_are_listed() {
        assert_eq!(commands("/full-send, then file the PR and /compact."), vec!["/full-send", "/compact"]);
        assert!(commands("a/b and / alone and 1/2").is_empty());
        assert!(commands("see `/usr/bin`").is_empty(), "inside backticks the token starts with a backtick");
    }
}
