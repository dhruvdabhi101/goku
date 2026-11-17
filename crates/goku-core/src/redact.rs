use std::sync::LazyLock;

use regex::Regex;

use crate::types::SecretHit;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Redaction {
    pub text: String,
    pub hits: Vec<SecretHit>,
    pub labels: Vec<String>,
}

pub fn redact_text(input: &str) -> Redaction {
    let spans = resolve(collect_spans(input));
    let text = apply(input, &spans);
    let mut labels = Vec::new();
    let hits = spans
        .iter()
        .map(|span| {
            if !labels.iter().any(|label| label == span.label) {
                labels.push(span.label.to_string());
            }
            SecretHit {
                label: span.label.to_string(),
                start: span.start,
                end: span.end,
                region: None,
            }
        })
        .collect();
    Redaction { text, hits, labels }
}

struct Span {
    start: usize,
    end: usize,
    label: &'static str,
    rank: u8,
}

struct Detector {
    pem: Regex,
    jwt: Regex,
    providers: Vec<(&'static str, Regex)>,
    userinfo: Regex,
    entropy: Regex,
    keyword: Regex,
}

static DETECTOR: LazyLock<Detector> = LazyLock::new(|| {
    Detector {
    pem: Regex::new(
        r"(?s)-----BEGIN (?:[A-Z0-9]+ )?PRIVATE KEY-----.*?-----END (?:[A-Z0-9]+ )?PRIVATE KEY-----",
    )
    .expect("pem pattern"),
    jwt: Regex::new(r"eyJ[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}").expect("jwt pattern"),
    providers: vec![
        (
            "ANTHROPIC_KEY",
            Regex::new(r"sk-ant-[A-Za-z0-9_-]{16,}").expect("anthropic"),
        ),
        (
            "OPENAI_KEY",
            Regex::new(r"sk-[A-Za-z0-9_-]{20,}").expect("openai"),
        ),
        (
            "STRIPE_KEY",
            Regex::new(r"(?:sk_live_|sk_test_|rk_live_|rk_test_)[A-Za-z0-9]{10,}").expect("stripe"),
        ),
        (
            "GITHUB_TOKEN",
            Regex::new(r"(?:github_pat_[A-Za-z0-9_]{20,}|ghp_[A-Za-z0-9]{20,}|gho_[A-Za-z0-9]{20,})")
                .expect("github"),
        ),
        (
            "GITLAB_PAT",
            Regex::new(r"glpat-[A-Za-z0-9_-]{16,}").expect("gitlab"),
        ),
        (
            "SLACK_TOKEN",
            Regex::new(r"xox[a-z]-[A-Za-z0-9-]{10,}").expect("slack"),
        ),
        (
            "AWS_ACCESS_KEY",
            Regex::new(r"AKIA[A-Z0-9]{16}").expect("aws"),
        ),
    ],
    userinfo: Regex::new(r"(?i)[a-z][a-z0-9+.-]*://([^\s/@]+):([^\s/@]+)@").expect("userinfo"),
    entropy: Regex::new(r"[A-Za-z0-9+/=_-]{20,}").expect("entropy"),
    keyword: Regex::new(
        r"(?i)\b(?:password|secret|token|api[_-]?key|credential|bearer|passwd|private[_-]?key)\b",
    )
    .expect("keyword"),
}
});

fn collect_spans(input: &str) -> Vec<Span> {
    let mut spans = Vec::new();
    push_matches(&DETECTOR.pem, input, "PRIVATE_KEY", 100, &mut spans);
    push_matches(&DETECTOR.jwt, input, "JWT", 90, &mut spans);
    for (label, re) in &DETECTOR.providers {
        push_matches(re, input, label, 80, &mut spans);
    }
    push_userinfo(input, &mut spans);
    push_queries(input, &mut spans);
    push_assignments(input, &mut spans);
    push_entropy(input, &mut spans);
    spans
}

fn push_matches(re: &Regex, input: &str, label: &'static str, rank: u8, out: &mut Vec<Span>) {
    for found in re.find_iter(input) {
        if found.start() == found.end() || allowlisted(found.as_str()) {
            continue;
        }
        if label == "OPENAI_KEY" && found.as_str().starts_with("sk-ant-") {
            continue;
        }
        let alphabet = if label == "JWT" { b64_byte } else { key_byte };
        if !edge_ok(input, found.start(), found.end(), alphabet) {
            continue;
        }
        out.push(Span {
            start: found.start(),
            end: found.end(),
            label,
            rank,
        });
    }
}

fn edge_ok(input: &str, start: usize, end: usize, token: fn(u8) -> bool) -> bool {
    let bytes = input.as_bytes();
    let prev_ok = start == 0 || !token(bytes[start - 1]);
    let next_ok = end >= bytes.len() || !token(bytes[end]);
    prev_ok && next_ok
}

fn key_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'-'
}

fn b64_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'-'
}

fn entropy_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'+' | b'/' | b'=' | b'_' | b'-')
}

fn push_userinfo(input: &str, out: &mut Vec<Span>) {
    for cap in DETECTOR.userinfo.captures_iter(input) {
        let (Some(user), Some(pass)) = (cap.get(1), cap.get(2)) else {
            continue;
        };
        if allowlisted(pass.as_str()) {
            continue;
        }
        out.push(Span {
            start: user.start(),
            end: pass.end(),
            label: "URL_USERINFO",
            rank: 70,
        });
    }
}

const QUERY_NAMES: &[&str] = &["access_token", "api_key", "token", "secret", "sig", "key"];

fn push_queries(input: &str, out: &mut Vec<Span>) {
    let lower = input.to_ascii_lowercase();
    let bytes = lower.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'?' && bytes[i] != b'&' {
            i += 1;
            continue;
        }
        let name_start = i + 1;
        let Some(rel) = lower[name_start..].find(['=', '&', '#', ' ', '\n', '\r', '"', '\''])
        else {
            break;
        };
        if bytes.get(name_start + rel) != Some(&b'=') {
            i = name_start;
            continue;
        }
        let name = &lower[name_start..name_start + rel];
        let val_start = name_start + rel + 1;
        if !QUERY_NAMES.contains(&name) {
            i = val_start;
            continue;
        }
        let val_end = query_value_end(input, val_start);
        if val_end > val_start && !allowlisted(&input[val_start..val_end]) {
            out.push(Span {
                start: val_start,
                end: val_end,
                label: "QUERY_SECRET",
                rank: 60,
            });
        }
        i = val_end.max(val_start + 1);
    }
}

fn query_value_end(input: &str, start: usize) -> usize {
    input[start..]
        .find(|c: char| matches!(c, '&' | '#' | '"' | '\'') || c.is_whitespace())
        .map(|n| start + n)
        .unwrap_or(input.len())
}

const ASSIGN_KEYS: &[(&str, &str)] = &[
    ("api_key", "API_KEY"),
    ("password", "PASSWORD"),
    ("secret", "SECRET"),
    ("token", "TOKEN"),
];

fn push_assignments(input: &str, out: &mut Vec<Span>) {
    let lower = input.to_ascii_lowercase();
    let mut i = 0;
    while i < lower.len() {
        if let Some((label, key_end)) = assignment_key(&lower, i) {
            if let Some((start, end)) = assignment_value(input, key_end) {
                if !allowlisted(&input[start..end]) {
                    out.push(Span {
                        start,
                        end,
                        label,
                        rank: 50,
                    });
                }
                i = end.max(i + 1);
                continue;
            }
            i = key_end.max(i + 1);
            continue;
        }
        i += 1;
    }
}

fn assignment_key(lower: &str, i: usize) -> Option<(&'static str, usize)> {
    let bytes = lower.as_bytes();
    if i > 0 && bytes[i - 1].is_ascii_alphanumeric() {
        return None;
    }
    for (key, label) in ASSIGN_KEYS {
        if !bytes[i..].starts_with(key.as_bytes()) {
            continue;
        }
        let end = i + key.len();
        if bytes.get(end).is_some_and(|b| b.is_ascii_alphanumeric()) {
            continue;
        }
        return Some((label, end));
    }
    None
}

fn assignment_value(input: &str, mut i: usize) -> Option<(usize, usize)> {
    let bytes = input.as_bytes();
    while bytes.get(i).is_some_and(|b| b.is_ascii_whitespace()) {
        i += 1;
    }
    if bytes.get(i) != Some(&b'=') && bytes.get(i) != Some(&b':') {
        return None;
    }
    i += 1;
    while bytes.get(i).is_some_and(|b| b.is_ascii_whitespace()) {
        i += 1;
    }
    let quote = bytes.get(i).copied();
    if quote == Some(b'"') || quote == Some(b'\'') {
        let start = i + 1;
        let end = input[start..]
            .find(quote.unwrap() as char)
            .map(|n| start + n)?;
        if end == start {
            return None;
        }
        return Some((start, end));
    }
    let start = i;
    let mut end = i;
    while bytes.get(end).is_some_and(|b| !is_value_end(*b)) {
        end += 1;
    }
    if end == start {
        return None;
    }
    Some((start, end))
}

fn is_value_end(b: u8) -> bool {
    b.is_ascii_whitespace() || matches!(b, b',' | b';' | b'&' | b'#')
}

fn push_entropy(input: &str, out: &mut Vec<Span>) {
    for found in DETECTOR.entropy.find_iter(input) {
        let token = found.as_str();
        if !edge_ok(input, found.start(), found.end(), entropy_byte)
            || token.len() < 20
            || allowlisted(token)
            || is_uuid(token)
            || is_git_sha(token)
            || looks_like_path(input, found.start(), found.end())
            || !token.bytes().any(|b| b.is_ascii_alphabetic())
            || !high_entropy(token)
            || !keyword_near(input, found.start(), found.end())
        {
            continue;
        }
        out.push(Span {
            start: found.start(),
            end: found.end(),
            label: "HIGH_ENTROPY",
            rank: 10,
        });
    }
}

fn allowlisted(value: &str) -> bool {
    matches!(
        value.to_ascii_lowercase().as_str(),
        "example" | "test" | "xxxx" | "changeme"
    )
}

fn is_uuid(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.len() != 36 {
        return false;
    }
    let groups = [8, 4, 4, 4, 12];
    let mut i = 0;
    for (group, len) in groups.iter().enumerate() {
        if group > 0 {
            if bytes.get(i) != Some(&b'-') {
                return false;
            }
            i += 1;
        }
        for _ in 0..*len {
            if !bytes.get(i).is_some_and(|b| b.is_ascii_hexdigit()) {
                return false;
            }
            i += 1;
        }
    }
    i == bytes.len()
}

fn is_git_sha(value: &str) -> bool {
    (value.len() == 40 || value.len() == 64) && value.bytes().all(|b| b.is_ascii_hexdigit())
}

fn looks_like_path(input: &str, start: usize, end: usize) -> bool {
    if input[start..end].starts_with('/') || input[start..end].starts_with('\\') {
        return true;
    }
    let prev = start
        .checked_sub(1)
        .and_then(|i| input.as_bytes().get(i).copied());
    let next = input.as_bytes().get(end).copied();
    matches!(prev, Some(b'/') | Some(b'\\')) || matches!(next, Some(b'/') | Some(b'\\'))
}

fn high_entropy(token: &str) -> bool {
    let bytes = token.as_bytes();
    if bytes.len() < 20 {
        return false;
    }
    let mut counts = [0u32; 256];
    let mut classes = 0u8;
    let mut seen = [false; 4];
    for &b in bytes {
        counts[b as usize] += 1;
        let class = if b.is_ascii_lowercase() {
            0
        } else if b.is_ascii_uppercase() {
            1
        } else if b.is_ascii_digit() {
            2
        } else {
            3
        };
        if !seen[class] {
            seen[class] = true;
            classes += 1;
        }
    }
    if classes < 2 {
        return false;
    }
    let n = bytes.len() as f64;
    let mut entropy = 0.0;
    for count in counts {
        if count == 0 {
            continue;
        }
        let p = count as f64 / n;
        entropy -= p * p.log2();
    }
    entropy >= 3.5
}

fn keyword_near(input: &str, start: usize, end: usize) -> bool {
    DETECTOR.keyword.is_match(window_before(input, start, 40))
        || DETECTOR.keyword.is_match(window_after(input, end, 40))
}

fn window_before(input: &str, start: usize, chars: usize) -> &str {
    let mut i = start;
    let mut left = chars;
    while left > 0 && i > 0 {
        i -= 1;
        while i > 0 && !input.is_char_boundary(i) {
            i -= 1;
        }
        left -= 1;
    }
    &input[i..start]
}

fn window_after(input: &str, end: usize, chars: usize) -> &str {
    let mut i = end;
    let mut left = chars;
    while left > 0 && i < input.len() {
        let ch = input[i..].chars().next().unwrap_or('\0');
        if ch == '\0' {
            break;
        }
        i += ch.len_utf8();
        left -= 1;
    }
    &input[end..i]
}

fn resolve(mut spans: Vec<Span>) -> Vec<Span> {
    spans.sort_by(|a, b| {
        b.rank
            .cmp(&a.rank)
            .then((b.end - b.start).cmp(&(a.end - a.start)))
            .then(a.start.cmp(&b.start))
    });
    let mut taken = Vec::new();
    for span in spans {
        if span.start >= span.end {
            continue;
        }
        if taken
            .iter()
            .any(|kept: &Span| kept.start < span.end && span.start < kept.end)
        {
            continue;
        }
        taken.push(span);
    }
    taken.sort_by_key(|span| span.start);
    taken
}

fn apply(input: &str, spans: &[Span]) -> String {
    let mut out = String::with_capacity(input.len());
    let mut cursor = 0;
    for span in spans {
        out.push_str(&input[cursor..span.start]);
        out.push_str("[REDACTED:");
        out.push_str(span.label);
        out.push(']');
        cursor = span.end;
    }
    out.push_str(&input[cursor..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gone(input: &str, secret: &str, label: &str) {
        let redacted = redact_text(input);
        assert!(
            !redacted.text.contains(secret),
            "secret survived in {}",
            redacted.text
        );
        assert!(
            redacted.text.contains(&format!("[REDACTED:{label}]")),
            "missing label in {}",
            redacted.text
        );
        assert!(
            redacted
                .hits
                .iter()
                .any(|hit| { hit.label == label && input.get(hit.start..hit.end) == Some(secret) }),
            "hit did not point at the input secret"
        );
    }

    #[test]
    fn provider_keys_are_labeled_and_removed() {
        gone(
            "openai sk-abcdefghijklmnopqrstuvwxyz012345 end",
            "sk-abcdefghijklmnopqrstuvwxyz012345",
            "OPENAI_KEY",
        );
        gone(
            "anthropic sk-ant-api03-abcdefghijklmnopqrstuvwxyz",
            "sk-ant-api03-abcdefghijklmnopqrstuvwxyz",
            "ANTHROPIC_KEY",
        );
        let anthropic = redact_text("sk-ant-api03-abcdefghijklmnopqrstuvwxyz");
        assert!(!anthropic.labels.iter().any(|label| label == "OPENAI_KEY"));
        gone(
            "stripe sk_live_abcdefghijklmnopqrst",
            "sk_live_abcdefghijklmnopqrst",
            "STRIPE_KEY",
        );
        gone(
            "stripe sk_test_abcdefghijklmnopqrst",
            "sk_test_abcdefghijklmnopqrst",
            "STRIPE_KEY",
        );
        gone(
            "stripe rk_live_abcdefghijklmnopqrst",
            "rk_live_abcdefghijklmnopqrst",
            "STRIPE_KEY",
        );
        gone(
            "gh ghp_abcdefghijklmnopqrstuvwxyz123456",
            "ghp_abcdefghijklmnopqrstuvwxyz123456",
            "GITHUB_TOKEN",
        );
        gone(
            "gh gho_abcdefghijklmnopqrstuvwxyz123456",
            "gho_abcdefghijklmnopqrstuvwxyz123456",
            "GITHUB_TOKEN",
        );
        gone(
            "gh github_pat_abcdefghijklmnopqrstuvwxyz123456",
            "github_pat_abcdefghijklmnopqrstuvwxyz123456",
            "GITHUB_TOKEN",
        );
        gone(
            "gl glpat-abcdefghijklmnopqrst",
            "glpat-abcdefghijklmnopqrst",
            "GITLAB_PAT",
        );
        gone(
            "slack xoxb-123456789012-abcdefghij",
            "xoxb-123456789012-abcdefghij",
            "SLACK_TOKEN",
        );
        gone(
            "aws AKIAIOSFODNN7EXAMPLE next",
            "AKIAIOSFODNN7EXAMPLE",
            "AWS_ACCESS_KEY",
        );
    }

    #[test]
    fn jwt_and_pem_blocks_are_removed() {
        let jwt = "eyJhbGciOiJub25lIn0.eyJzdWIiOiIxIn0.signaturevalue";
        gone(&format!("auth {jwt}"), jwt, "JWT");
        let pem = "-----BEGIN RSA PRIVATE KEY-----\nabcdefghijklmnopqrstuvwxyz0123456789\n-----END RSA PRIVATE KEY-----";
        gone(&format!("key\n{pem}\n"), pem, "PRIVATE_KEY");
        let public_key = "-----BEGIN PUBLIC KEY-----\nMIIBIjANBgkqhkiG9w0BAQEFAAOCAQ8A\n-----END PUBLIC KEY-----";
        let redacted = redact_text(public_key);
        assert_eq!(redacted.text, public_key);
        assert!(redacted.hits.is_empty());
    }

    #[test]
    fn connection_strings_redact_userinfo_and_emails_stay() {
        let input = "postgres://ada:s3cret@localhost:5432/db ada@example.com";
        let redacted = redact_text(input);
        assert!(!redacted.text.contains("s3cret"));
        assert!(redacted
            .text
            .contains("postgres://[REDACTED:URL_USERINFO]@localhost:5432/db"));
        assert!(redacted.text.contains("ada@example.com"));
        assert_eq!(
            &input[redacted.hits[0].start..redacted.hits[0].end],
            "ada:s3cret"
        );
        let user_only = redact_text("postgres://ada@localhost/db");
        assert_eq!(user_only.text, "postgres://ada@localhost/db");
        assert!(redact_text("https://user:test@example.com/x")
            .text
            .contains("https://user:test@example.com/x"));
    }

    #[test]
    fn secret_query_values_are_redacted_and_names_stay() {
        let input = "https://example.com/cb?token=abcdef&keyboard=value&key=example&sig=z9z9";
        let redacted = redact_text(input);
        assert_eq!(
            redacted.text,
            "https://example.com/cb?token=[REDACTED:QUERY_SECRET]&keyboard=value&key=example&sig=[REDACTED:QUERY_SECRET]"
        );
        assert!(!redacted.text.contains("abcdef"));
        assert!(!redacted.text.contains("z9z9"));
    }

    #[test]
    fn assignments_keep_the_keyword_and_skip_the_allowlist() {
        let redacted = redact_text("password=hunter2 password: \"nope\" api_key='abcd'");
        assert!(redacted.text.contains("password=[REDACTED:PASSWORD]"));
        assert!(redacted.text.contains("password: \"[REDACTED:PASSWORD]\""));
        assert!(redacted.text.contains("api_key='[REDACTED:API_KEY]'"));
        assert!(!redacted.text.contains("hunter2"));
        assert!(!redacted.text.contains("nope"));
        for value in ["example", "test", "xxxx", "changeme", "EXAMPLE"] {
            let input = format!("password={value}");
            assert_eq!(redact_text(&input).text, input, "{value}");
        }
        assert_eq!(redact_text("password=").text, "password=");
        assert_eq!(
            redact_text("set the password later").text,
            "set the password later"
        );
    }

    #[test]
    fn high_entropy_tokens_need_a_nearby_keyword_and_skip_ids_and_paths() {
        let token = "aB3xQ9mN2pL7vR4tY8wZ";
        gone(&format!("the api_key is {token}"), token, "HIGH_ENTROPY");
        let far = format!("secret{}{token}", " ".repeat(41));
        assert!(redact_text(&far).text.contains(token));
        let near = format!("secret{}{token}", " ".repeat(34));
        assert!(!redact_text(&near).text.contains(token));
        let uuid = "550e8400-e29b-41d4-a716-446655440000";
        assert!(redact_text(&format!("secret {uuid}")).text.contains(uuid));
        let sha = "0123456789abcdef0123456789abcdef01234567";
        assert_eq!(sha.len(), 40);
        assert!(redact_text(&format!("secret {sha}")).text.contains(sha));
        let path = "secret file /Users/dhruv/Work/goku/very-long-secret-folder/notes.txt";
        assert!(redact_text(path).text.contains("very-long-secret-folder"));
        assert_eq!(
            redact_text("Ada Lovelace called +1-415-555-0134").text,
            "Ada Lovelace called +1-415-555-0134"
        );
    }

    #[test]
    fn hits_are_byte_spans_into_the_input() {
        let secret = "sk-abcdefghijklmnopqrstuvwxyz012345";
        let input = format!("密钥 {secret}");
        let redacted = redact_text(&input);
        let hit = redacted
            .hits
            .iter()
            .find(|hit| hit.label == "OPENAI_KEY")
            .unwrap();
        assert_eq!(&input[hit.start..hit.end], secret);
        assert!(hit.region.is_none());
        assert!(!redacted.text.contains(secret));
    }

    #[test]
    fn a_specific_key_wins_over_the_assignment_and_entropy_labels() {
        let key = "sk-abcdefghijklmnopqrstuvwxyz012345";
        let redacted = redact_text(&format!("api_key={key}"));
        assert_eq!(redacted.text, "api_key=[REDACTED:OPENAI_KEY]");
        assert_eq!(redacted.hits.len(), 1);
        assert_eq!(redacted.labels, vec!["OPENAI_KEY".to_string()]);
    }

    #[test]
    fn empty_text_has_no_hits() {
        let redacted = redact_text("");
        assert_eq!(redacted.text, "");
        assert!(redacted.hits.is_empty());
    }
}
