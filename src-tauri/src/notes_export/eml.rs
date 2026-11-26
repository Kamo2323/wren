//! The Windows Email button's draft: a `.eml` file (RFC 5322 + MIME) with the formatted notes as HTML and a
//! plain-text alternative, marked `X-Unsent: 1` so Outlook opens it as a new message ready to address and send.
//!
//! Everything non-ASCII is encoded: the subject as RFC 2047 encoded-words, both bodies as UTF-8 in base64.
//! The boundary starts with `=_`, which can never occur in base64 text, so it can't clash with a body.

use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;

/// Longest header line before folding (RFC 5322 recommends 78).
const MAX_PLAIN_SUBJECT: usize = 70;
/// UTF-8 bytes per encoded-word: `=?UTF-8?B?` + base64(45 bytes) = 60 chars + `?=` stays within 75.
const WORD_BYTES: usize = 45;

/// One line, no control characters (no header injection), at most 200 characters; empty → "Meeting notes".
pub fn clean_subject(subject: &str) -> String {
    let one_line: String = subject.chars().map(|c| if c.is_control() { ' ' } else { c }).collect();
    let mut s = one_line.split_whitespace().collect::<Vec<_>>().join(" ");
    if s.chars().count() > 200 {
        s = s.chars().take(200).collect::<String>().trim_end().to_string();
    }
    if s.is_empty() { "Meeting notes".into() } else { s }
}

/// The `Subject:` header value: as-is when it is short printable ASCII, otherwise folded encoded-words.
pub fn encode_subject(subject: &str) -> String {
    let s = clean_subject(subject);
    let plain_ok = s.len() <= MAX_PLAIN_SUBJECT && s.bytes().all(|b| (0x20..0x7f).contains(&b)) && !s.contains("=?");
    if plain_ok {
        return s;
    }
    let mut words = Vec::new();
    let mut chunk = String::new();
    for c in s.chars() {
        if chunk.len() + c.len_utf8() > WORD_BYTES {
            words.push(std::mem::take(&mut chunk));
        }
        chunk.push(c);
    }
    if !chunk.is_empty() {
        words.push(chunk);
    }
    words.iter().map(|w| format!("=?UTF-8?B?{}?=", B64.encode(w.as_bytes()))).collect::<Vec<_>>().join("\r\n ")
}

/// Canonical CRLF line endings, then base64 in 76-character lines.
fn base64_body(text: &str) -> String {
    let crlf = text.replace("\r\n", "\n").replace('\r', "\n").replace('\n', "\r\n");
    let encoded = B64.encode(crlf.as_bytes());
    encoded.as_bytes().chunks(76).map(|l| std::str::from_utf8(l).expect("base64 is ASCII")).collect::<Vec<_>>().join("\r\n")
}

/// The whole draft. `date` is an RFC 2822 date; `boundary_seed` makes the MIME boundary unique.
pub fn build_eml(subject: &str, html_fragment: &str, plain: &str, date: &str, boundary_seed: &str) -> String {
    let boundary = format!("=_WrenNotes_{}", boundary_seed.chars().filter(char::is_ascii_alphanumeric).collect::<String>());
    let html = format!("<!doctype html><html><head><meta charset=\"utf-8\"></head><body>{html_fragment}</body></html>");
    let mut out = String::new();
    out.push_str("MIME-Version: 1.0\r\n");
    out.push_str(&format!("Date: {date}\r\n"));
    out.push_str(&format!("Subject: {}\r\n", encode_subject(subject)));
    out.push_str("X-Unsent: 1\r\n");
    out.push_str(&format!("Content-Type: multipart/alternative; boundary=\"{boundary}\"\r\n"));
    out.push_str("\r\n");
    out.push_str("This is a multi-part message in MIME format.\r\n");
    for (kind, body) in [("text/plain", plain), ("text/html", html.as_str())] {
        out.push_str(&format!("\r\n--{boundary}\r\n"));
        out.push_str(&format!("Content-Type: {kind}; charset=\"utf-8\"\r\n"));
        out.push_str("Content-Transfer-Encoding: base64\r\n\r\n");
        out.push_str(&base64_body(body));
        out.push_str("\r\n");
    }
    out.push_str(&format!("\r\n--{boundary}--\r\n"));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const DATE: &str = "Wed, 26 Nov 2026 10:00:00 +0000";

    fn decode_words(header: &str) -> String {
        header
            .split("\r\n ")
            .map(|w| {
                let b = w.strip_prefix("=?UTF-8?B?").and_then(|w| w.strip_suffix("?=")).expect("encoded-word");
                String::from_utf8(B64.decode(b).unwrap()).unwrap()
            })
            .collect()
    }

    fn header<'a>(eml: &'a str, name: &str) -> &'a str {
        let head = eml.split("\r\n\r\n").next().unwrap();
        let start = head.find(&format!("{name}: ")).unwrap() + name.len() + 2;
        let rest = &head[start..];
        // A folded header continues on lines starting with a space.
        let mut end = 0;
        for line in rest.split_inclusive("\r\n") {
            if end > 0 && !line.starts_with(' ') {
                break;
            }
            end += line.len();
        }
        rest[..end].trim_end_matches("\r\n")
    }

    /// (content-type line, decoded body) for each part.
    fn parts(eml: &str) -> Vec<(String, String)> {
        let boundary = header(eml, "Content-Type").split("boundary=\"").nth(1).unwrap().trim_end_matches('"').to_string();
        let sep = format!("\r\n--{boundary}");
        let sections: Vec<&str> = eml.split(&sep).collect();
        assert!(sections.last().unwrap().starts_with("--\r\n"), "closing boundary");
        sections[1..sections.len() - 1]
            .iter()
            .map(|s| {
                let (head, body) = s.trim_start_matches("\r\n").split_once("\r\n\r\n").unwrap();
                assert!(head.contains("Content-Transfer-Encoding: base64"));
                assert!(body.lines().all(|l| l.trim_end().len() <= 76), "base64 lines ≤ 76");
                let b: String = body.split_whitespace().collect();
                (head.lines().next().unwrap().to_string(), String::from_utf8(B64.decode(b).unwrap()).unwrap())
            })
            .collect()
    }

    #[test]
    fn draft_has_unsent_flag_and_both_parts() {
        let eml = build_eml("Weekly sync", "<h1>Weekly sync</h1><p>Ship it ✓</p>", "Weekly sync\nShip it ✓", DATE, "abc123");
        assert_eq!(header(&eml, "X-Unsent"), "1");
        assert_eq!(header(&eml, "MIME-Version"), "1.0");
        assert_eq!(header(&eml, "Subject"), "Weekly sync");
        assert_eq!(header(&eml, "Date"), DATE);
        assert_eq!(header(&eml, "Content-Type"), "multipart/alternative; boundary=\"=_WrenNotes_abc123\"");
        assert!(!eml.contains("\nTo:") && !eml.starts_with("To:"), "no recipient: the person adds one");
        let p = parts(&eml);
        assert_eq!(p.len(), 2);
        assert_eq!(p[0].0, "Content-Type: text/plain; charset=\"utf-8\"");
        assert_eq!(p[0].1, "Weekly sync\r\nShip it ✓");
        assert_eq!(p[1].0, "Content-Type: text/html; charset=\"utf-8\"");
        assert!(p[1].1.starts_with("<!doctype html><html><head><meta charset=\"utf-8\">"));
        assert!(p[1].1.contains("<h1>Weekly sync</h1><p>Ship it ✓</p>"));
        // Every line of the file is CRLF-terminated 7-bit ASCII.
        assert!(eml.is_ascii());
        assert!(!eml.replace("\r\n", "").contains('\n'));
    }

    #[test]
    fn non_ascii_and_long_subjects_are_encoded_words() {
        let subject = "Résumé review — “Q3” plan with Zoë and 李雷 about the very long agenda for the offsite in Kraków";
        let enc = encode_subject(subject);
        assert!(enc.is_ascii());
        for line in enc.split("\r\n") {
            assert!(line.trim_start().len() <= 75, "{line}");
        }
        assert_eq!(decode_words(&enc), subject);
        let long = "a".repeat(90);
        assert_eq!(decode_words(&encode_subject(&long)), long);
        assert_eq!(decode_words(&encode_subject("=?looks encoded?=")), "=?looks encoded?=");
    }

    #[test]
    fn subject_cannot_inject_headers() {
        let eml = build_eml("Notes\r\nBcc: someone@example.com", "<p>x</p>", "x", DATE, "s");
        assert_eq!(header(&eml, "Subject"), "Notes Bcc: someone@example.com");
        assert!(!eml.contains("\r\nBcc:"));
        assert_eq!(clean_subject(" \t "), "Meeting notes");
        assert_eq!(clean_subject(&"x".repeat(300)).len(), 200);
    }

    #[test]
    fn boundary_seed_is_sanitised() {
        let eml = build_eml("s", "<p>x</p>", "x", DATE, "a\"b\r\nc-d");
        assert_eq!(header(&eml, "Content-Type"), "multipart/alternative; boundary=\"=_WrenNotes_abcd\"");
    }
}
