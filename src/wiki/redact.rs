//! Conservative, best-effort secret redaction for Wiki and trace-derived text.
//!
//! This is not comprehensive secret detection. It cannot recover secrets that
//! were already written elsewhere and cannot erase values from process memory.
//! Callers must still avoid collecting raw prompts, tool output, environment
//! dumps, and command output in the first place.

use crate::error::Result;

const REDACTED: &str = "[REDACTED]";
const REDACTED_TOKEN: &str = "[REDACTED_SECRET]";
const REDACTED_PRIVATE_KEY: &str = "[REDACTED_PRIVATE_KEY]";

/// Redact representative secret forms while preserving surrounding diagnostics.
pub fn redact(input: &str) -> Result<String> {
    let text = redact_pem_private_keys(input);
    let text = redact_url_credentials(&text);
    let text = redact_authorization_headers(&text);
    let text = redact_bearer_tokens(&text);
    let text = redact_assignments(&text);
    Ok(redact_known_tokens(&text))
}

fn redact_pem_private_keys(input: &str) -> String {
    let mut output = String::with_capacity(input.len());
    let mut in_private_key = false;

    for line in input.split_inclusive('\n') {
        let trimmed = line.trim();
        let upper = trimmed.to_ascii_uppercase();

        if !in_private_key && upper.starts_with("-----BEGIN ") && upper.contains("PRIVATE KEY-----")
        {
            in_private_key = true;
            if !output.is_empty() && !output.ends_with('\n') {
                output.push('\n');
            }
            output.push_str(REDACTED_PRIVATE_KEY);
            output.push('\n');
            continue;
        }

        if in_private_key {
            if upper.starts_with("-----END ") && upper.contains("PRIVATE KEY-----") {
                in_private_key = false;
            }
            continue;
        }

        output.push_str(line);
    }

    output
}

fn redact_authorization_headers(input: &str) -> String {
    let mut output = String::with_capacity(input.len());

    for line in input.split_inclusive('\n') {
        let content = line.trim_end_matches(['\r', '\n']);
        let ending = &line[content.len()..];
        let lower = content.to_ascii_lowercase();

        if let Some(index) = lower.find("authorization:") {
            let after = index + "authorization:".len();
            let value = content[after..].trim_start();
            let scheme = value.split_whitespace().next().unwrap_or("");
            let preserve_scheme = matches!(
                scheme.to_ascii_lowercase().as_str(),
                "bearer" | "basic" | "token" | "digest"
            );

            output.push_str(&content[..after]);
            if preserve_scheme {
                output.push(' ');
                output.push_str(scheme);
            }
            output.push(' ');
            output.push_str(REDACTED);
            output.push_str(ending);
        } else {
            output.push_str(line);
        }
    }

    output
}

fn redact_bearer_tokens(input: &str) -> String {
    let mut output = String::with_capacity(input.len());
    let mut rest = input;

    loop {
        let lower = rest.to_ascii_lowercase();
        let Some(index) = lower.find("bearer ") else {
            output.push_str(rest);
            break;
        };

        output.push_str(&rest[..index]);
        output.push_str(&rest[index..index + "bearer ".len()]);

        let token_start = index + "bearer ".len();
        let token_len = rest[token_start..]
            .char_indices()
            .find(|(_, character)| is_token_terminator(*character))
            .map(|(offset, _)| offset)
            .unwrap_or(rest.len() - token_start);

        if token_len == 0 {
            rest = &rest[token_start..];
        } else {
            output.push_str(REDACTED);
            rest = &rest[token_start + token_len..];
        }
    }

    output
}

fn is_token_terminator(character: char) -> bool {
    character.is_whitespace()
        || matches!(
            character,
            '"' | '\'' | ',' | ';' | '=' | ')' | ']' | '}' | '<' | '>' | '`'
        )
}

fn redact_assignments(input: &str) -> String {
    let mut output = String::with_capacity(input.len());

    for line in input.split_inclusive('\n') {
        let content = line.trim_end_matches(['\r', '\n']);
        let ending = &line[content.len()..];
        output.push_str(&redact_assignment_line(content));
        output.push_str(ending);
    }

    output
}

fn redact_assignment_line(content: &str) -> String {
    let separator = content
        .char_indices()
        .find(|(_, character)| matches!(character, '=' | ':'));

    let Some((index, separator)) = separator else {
        return content.to_owned();
    };

    let key = assignment_key(&content[..index]);
    let value = &content[index + separator.len_utf8()..];
    if key == "authorization" && value.contains(REDACTED) {
        return content.to_owned();
    }
    if !is_sensitive_key(&key) {
        return content.to_owned();
    }

    let leading_whitespace = value.len() - value.trim_start().len();
    let mut output = String::with_capacity(content.len());
    output.push_str(&content[..index + separator.len_utf8()]);
    output.push_str(&value[..leading_whitespace]);
    output.push_str(REDACTED);
    output
}

fn assignment_key(prefix: &str) -> String {
    let prefix = prefix.trim_end();
    let mut start = prefix.len();
    for (index, character) in prefix.char_indices().rev() {
        if character.is_ascii_alphanumeric() || matches!(character, '_' | '-' | '.' | '"' | '\'') {
            start = index;
        } else {
            break;
        }
    }

    prefix[start..]
        .trim_matches(['"', '\''])
        .trim_start_matches('-')
        .to_ascii_lowercase()
}

fn is_sensitive_key(key: &str) -> bool {
    matches!(
        key,
        "key"
            | "token"
            | "secret"
            | "password"
            | "passwd"
            | "pwd"
            | "pass"
            | "credential"
            | "credentials"
            | "auth"
            | "authorization"
    ) || key.contains("secret")
        || key.contains("token")
        || key.contains("password")
        || key.contains("passwd")
        || key.contains("private_key")
        || key.contains("api_key")
        || key.contains("apikey")
        || key.contains("access_key")
        || key.contains("client_secret")
        || key.contains("credential")
        || key.ends_with("_pass")
        || key.ends_with("_pwd")
}

fn redact_url_credentials(input: &str) -> String {
    let mut output = String::with_capacity(input.len());
    let mut last = 0;
    let mut search = 0;

    while let Some(relative) = input[search..].find("://") {
        let scheme_end = search + relative;
        let user_start = scheme_end + "://".len();
        let end = input[user_start..]
            .char_indices()
            .find(|(_, character)| {
                character.is_whitespace()
                    || matches!(
                        character,
                        '"' | '\'' | ')' | ']' | '}' | ',' | '<' | '>' | '`' | ';'
                    )
            })
            .map(|(offset, _)| user_start + offset)
            .unwrap_or(input.len());

        let authority = &input[user_start..end];
        if let Some(at_relative) = authority.rfind('@') {
            let at = user_start + at_relative;
            let userinfo = &input[user_start..at];
            if !userinfo.is_empty() {
                output.push_str(&input[last..user_start]);
                if let Some(colon_relative) = userinfo.rfind(':') {
                    let colon = user_start + colon_relative;
                    output.push_str(&input[user_start..colon + 1]);
                    output.push_str(REDACTED);
                } else {
                    output.push_str(REDACTED);
                }
                last = at;
            }
        }

        search = end.max(user_start);
    }

    output.push_str(&input[last..]);
    output
}

fn redact_known_tokens(input: &str) -> String {
    let mut output = String::with_capacity(input.len());
    let bytes = input.as_bytes();
    let mut last = 0;
    let mut index = 0;

    while index < bytes.len() {
        if is_token_byte(bytes[index]) {
            let start = index;
            while index < bytes.len() && is_token_byte(bytes[index]) {
                index += 1;
            }
            let token = &input[start..index];
            if looks_like_secret_token(token) {
                output.push_str(&input[last..start]);
                output.push_str(REDACTED_TOKEN);
                last = index;
            }
        } else {
            index += 1;
        }
    }

    output.push_str(&input[last..]);
    output
}

fn is_token_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.')
}

fn looks_like_secret_token(token: &str) -> bool {
    let long_enough = token.len() >= 16;
    if token.starts_with("sk-") && token.len() >= 16 {
        return true;
    }
    if ["ghp_", "gho_", "ghu_", "ghs_", "ghr_", "github_pat_"]
        .iter()
        .any(|prefix| token.starts_with(prefix) && token.len() >= 20)
    {
        return true;
    }
    if token.starts_with("AKIA") && token.len() >= 20 {
        return true;
    }
    if token.starts_with("AIza") && token.len() >= 35 {
        return true;
    }
    if token.starts_with("xox") && token.contains('-') && token.len() >= 16 {
        return true;
    }
    if ["hf_", "glpat-", "npm_", "pypi-", "SG."]
        .iter()
        .any(|prefix| token.starts_with(prefix) && long_enough)
    {
        return true;
    }
    if token.starts_with("eyJ") && token.len() >= 40 && token.matches('.').count() >= 2 {
        return true;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_assignments_and_password_forms() {
        let input = "KEY=plain\nTOKEN=abc123\nSECRET: hunter2\npassword = \"letmein\"\nexport API_KEY=sk-test-abcdefghijklmnop\nDB_PASS=p@ss";
        let redacted = redact(input).unwrap();

        for secret in ["plain", "abc123", "hunter2", "letmein", "sk-test", "p@ss"] {
            assert!(!redacted.contains(secret), "{secret} leaked in {redacted}");
        }
        assert!(redacted.contains("KEY=[REDACTED]"));
        assert!(redacted.contains("SECRET: [REDACTED]"));
        assert!(redacted.contains("password = [REDACTED]"));
    }

    #[test]
    fn redacts_bearer_and_authorization_headers() {
        let input = "Authorization: Bearer abcdefghijklmnop\nlog says Bearer zyxwvutsrqponmlk done";
        let redacted = redact(input).unwrap();

        assert!(!redacted.contains("abcdefghijklmnop"));
        assert!(!redacted.contains("zyxwvutsrqponmlk"));
        assert!(redacted.contains("Authorization: Bearer [REDACTED]"));
        assert!(redacted.contains("Bearer [REDACTED] done"));
    }

    #[test]
    fn redacts_pem_private_key_blocks() {
        let input = "before\n-----BEGIN RSA PRIVATE KEY-----\nMIIEsecret\nmore\n-----END RSA PRIVATE KEY-----\nafter";
        let redacted = redact(input).unwrap();

        assert!(!redacted.contains("MIIEsecret"));
        assert!(redacted.contains(REDACTED_PRIVATE_KEY));
        assert!(redacted.contains("before"));
        assert!(redacted.contains("after"));
    }

    #[test]
    fn redacts_env_style_urls_and_common_tokens() {
        let input = "DATABASE_URL=postgres://user:dbpassword@example.com/db\n\
                     token sk-proj-abcdefghijklmnopqrstuvwxyz\n\
                     GH=ghp_abcdefghijklmnopqrstuvwxyz012345\n\
                     AWS=AKIAABCDEFGHIJKLMNOP";
        let redacted = redact(input).unwrap();

        assert!(!redacted.contains("dbpassword"));
        assert!(!redacted.contains("sk-proj"));
        assert!(!redacted.contains("ghp_"));
        assert!(!redacted.contains("AKIA"));
        assert!(redacted.contains("postgres://user:[REDACTED]@example.com/db"));
        assert!(redacted.contains(REDACTED_TOKEN));
    }

    #[test]
    fn preserves_non_secret_diagnostics() {
        let input = "cargo test exited 0\nupdated: 2026-10-22\nsrc/main.rs";
        assert_eq!(redact(input).unwrap(), input);
    }
}
