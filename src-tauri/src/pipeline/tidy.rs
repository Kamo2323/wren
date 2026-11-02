//! Steps 2, 3, 4 and 8 (spec §6): whitespace, fillers, "i", final spacing.

/// Fillers removed as whole words, any case except ALL CAPS (so "ER" the ward survives).
/// Never on this list: like, you know, I mean, so, well, basically, actually, ah, yeah.
const FILLERS: &[&str] = &["um", "umm", "uh", "uhh", "er", "err", "erm", "ermm", "hmm", "mm"];

/// Lower-case pronoun forms that are capitalised.
const LOWER_I: &[&str] = &["i", "i'm", "i've", "i'll", "i'd"];

/// Step 2: every run of whitespace (including newlines) becomes one space; ends trimmed.
pub fn normalise_whitespace(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

const SENTENCE_END: &[char] = &['.', '!', '?', '…'];

/// Splits a whitespace-free token into (leading punctuation, word, trailing punctuation).
fn split_token(token: &str) -> (&str, &str, &str) {
    let start = token.char_indices().find(|(_, c)| c.is_alphanumeric()).map(|(i, _)| i);
    let Some(start) = start else {
        return (token, "", "");
    };
    let end = token
        .char_indices()
        .rev()
        .find(|(_, c)| c.is_alphanumeric())
        .map(|(i, c)| i + c.len_utf8())
        .unwrap_or(token.len());
    (&token[..start], &token[start..end], &token[end..])
}

fn is_filler(word: &str) -> bool {
    let has_letter = word.chars().any(char::is_alphabetic);
    let all_caps = has_letter && word.chars().filter(|c| c.is_alphabetic()).all(char::is_uppercase);
    !all_caps && FILLERS.contains(&word.to_lowercase().as_str())
}

fn capitalise_first_letter(token: &str) -> String {
    let mut out = String::with_capacity(token.len());
    let mut done = false;
    for c in token.chars() {
        if !done && c.is_alphabetic() {
            out.extend(c.to_uppercase());
            done = true;
        } else {
            out.push(c);
        }
    }
    out
}

pub fn remove_fillers(text: &str) -> String {
    let mut kept: Vec<String> = Vec::new();
    let mut removed_any = false;
    let mut capitalise_next = false;
    for token in text.split_whitespace() {
        let (_, word, trail) = split_token(token);
        if !is_filler(word) {
            if capitalise_next {
                kept.push(capitalise_first_letter(token));
                capitalise_next = false;
            } else {
                kept.push(token.to_string());
            }
            continue;
        }
        removed_any = true;
        let at_sentence_start = kept.last().is_none_or(|prev| prev.ends_with(SENTENCE_END));
        if at_sentence_start {
            // "Um, so…" / "Okay. Uh, yes." — the next word now starts the sentence.
            capitalise_next = true;
            continue;
        }
        let prev = kept.last_mut().expect("not at sentence start, so a previous word exists");
        let end: String = trail.chars().filter(|c| SENTENCE_END.contains(c)).collect();
        if !end.is_empty() {
            // "Right, um." → "Right." — the filler's full stop / question mark closes the sentence.
            let trimmed_len = prev.trim_end_matches([',', ';', ':']).len();
            prev.truncate(trimmed_len);
            if !prev.ends_with(SENTENCE_END) {
                prev.push_str(&end);
            }
        } else if trail.contains(',') && prev.ends_with(',') {
            // "So, um, I think" → "So I think" — the comma pair around the filler goes.
            prev.pop();
        }
    }
    if !removed_any {
        return text.to_string();
    }
    let mut out = kept.join(" ");
    // A filler that ended the text can leave "…, " behind.
    let trimmed_len = out.trim_end_matches([',', ';', ':', ' ']).len();
    out.truncate(trimmed_len);
    out
}

pub fn capitalise_i(text: &str) -> String {
    text.split(' ')
        .map(|token| {
            let (lead, word, trail) = split_token(token);
            let straight = word.replace('\u{2019}', "'");
            if word.starts_with('i') && LOWER_I.contains(&straight.as_str()) {
                format!("{lead}I{}{trail}", &word[1..])
            } else {
                token.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

pub fn final_spacing(text: &str) -> String {
    let text = normalise_whitespace(text);
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    for (i, &c) in chars.iter().enumerate() {
        // Drop a space before closing punctuation that ends a word (" ," " ." but not " .NET").
        if c == ' '
            && let Some(&next) = chars.get(i + 1)
            && [',', '.', '!', '?', ';', ':'].contains(&next)
            && chars.get(i + 2).is_none_or(|&a| a == ' ')
        {
            continue;
        }
        out.push(c);
    }
    out.trim_end().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn whitespace_table() {
        for (input, want) in [
            ("", ""),
            ("   ", ""),
            ("  hello   world  ", "hello world"),
            ("line one\nline two\r\n", "line one line two"),
            ("tab\there", "tab here"),
        ] {
            assert_eq!(normalise_whitespace(input), want, "{input:?}");
        }
    }

    #[test]
    fn filler_table() {
        for (input, want) in [
            // The brief's cases.
            ("Uh, I think so.", "I think so."),
            ("I told her about the error.", "I told her about the error."),
            ("Take him to the ER now.", "Take him to the ER now."),
            ("UM is an acronym here.", "UM is an acronym here."),
            // Every filler, any case but all-caps.
            ("um umm uh uhh er err erm ermm hmm mm okay", "Okay"),
            ("Um, umm, Uh, Uhh, Er, Err, Erm, Ermm, Hmm, Mm, right.", "Right."),
            // Orphaned commas.
            ("So, um, I think we should go.", "So I think we should go."),
            ("I, uh, don't know.", "I don't know."),
            ("We could, er, try again, um, tomorrow.", "We could try again tomorrow."),
            ("Right, um.", "Right."),
            ("It's done, hmm?", "It's done?"),
            ("That's it um", "That's it"),
            ("Okay. Um, so the plan is fine.", "Okay. So the plan is fine."),
            ("Okay? Uh, yes.", "Okay? Yes."),
            ("Um.", ""),
            ("uh, um, er", ""),
            // Not whole words: kept.
            ("Hummus and umbrellas, errand, herd, mmm.", "Hummus and umbrellas, errand, herd, mmm."),
            ("uh-huh, mm-hmm", "uh-huh, mm-hmm"),
            // Never removed.
            (
                "Like, you know, I mean, so, well, basically, actually, ah, yeah.",
                "Like, you know, I mean, so, well, basically, actually, ah, yeah.",
            ),
            // Untouched text is returned unchanged (no extra capitalising).
            ("e.g. this stays lower", "e.g. this stays lower"),
            ("", ""),
        ] {
            assert_eq!(remove_fillers(input), want, "{input:?}");
        }
    }

    #[test]
    fn capitalise_i_table() {
        for (input, want) in [
            ("i'm here and i think i've got it, i'll see, i'd say.", "I'm here and I think I've got it, I'll see, I'd say."),
            ("i’m curly", "I’m curly"),
            ("i", "I"),
            ("(i) and \"i\"", "(I) and \"I\""),
            ("it is in iris, ice, i.e. idea", "it is in iris, ice, i.e. idea"),
            ("I'm fine", "I'm fine"),
            ("", ""),
        ] {
            assert_eq!(capitalise_i(input), want, "{input:?}");
        }
    }

    #[test]
    fn final_spacing_table() {
        for (input, want) in [
            ("Hello world. ", "Hello world."),
            ("Hello world.\n", "Hello world."),
            ("Hello ,  world .", "Hello, world."),
            ("Really ?", "Really?"),
            ("  ", ""),
            ("", ""),
            ("Hi\r\n\r\n", "Hi"),
        ] {
            assert_eq!(final_spacing(input), want, "{input:?}");
        }
    }
}
