//! Spotting files that look like secrets before they first go into the
//! store, which usually ends up in a git repository and often on a server.
//!
//! Three signals, each cheap and specific: the file is private (group and
//! others cannot read it, which is how people mark secrets), it holds a
//! private key, or it holds a token in one of the well-known formats. A
//! token has to stand on its own (not in the middle of a longer word) and
//! be long enough, which keeps false alarms rare.

use std::fs::File;
use std::io::Read;

use crate::fsx::{Kind, Meta};
use crate::perms;

/// How much of a file is read when looking for keys and tokens.
const SCAN_BYTES: u64 = 1024 * 1024;

/// Private-key markers, as they start a key file or block.
const KEY_MARKERS: &[&str] = &[
    "-----BEGIN RSA PRIVATE KEY-----",
    "-----BEGIN DSA PRIVATE KEY-----",
    "-----BEGIN EC PRIVATE KEY-----",
    "-----BEGIN OPENSSH PRIVATE KEY-----",
    "-----BEGIN PRIVATE KEY-----",
    "-----BEGIN ENCRYPTED PRIVATE KEY-----",
    "-----BEGIN PGP PRIVATE KEY BLOCK-----",
    "PuTTY-User-Key-File-",
];

/// A token format: a prefix, the characters that may follow it, and how
/// many of them there must at least be.
struct Token {
    name: &'static str,
    prefix: &'static str,
    chars: fn(u8) -> bool,
    min: usize,
}

fn alnum(b: u8) -> bool {
    b.is_ascii_alphanumeric()
}
fn word(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}
fn dashed(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'-'
}
fn upper(b: u8) -> bool {
    b.is_ascii_uppercase() || b.is_ascii_digit()
}

#[rustfmt::skip]
const TOKENS: &[Token] = &[
    Token { name: "a GitHub token", prefix: "ghp_", chars: alnum, min: 36 },
    Token { name: "a GitHub token", prefix: "gho_", chars: alnum, min: 36 },
    Token { name: "a GitHub token", prefix: "ghu_", chars: alnum, min: 36 },
    Token { name: "a GitHub token", prefix: "ghs_", chars: alnum, min: 36 },
    Token { name: "a GitHub token", prefix: "ghr_", chars: alnum, min: 36 },
    Token { name: "a GitHub token", prefix: "github_pat_", chars: word, min: 40 },
    Token { name: "a GitLab token", prefix: "glpat-", chars: dashed, min: 20 },
    Token { name: "a Slack token", prefix: "xoxb-", chars: dashed, min: 20 },
    Token { name: "a Slack token", prefix: "xoxp-", chars: dashed, min: 20 },
    Token { name: "a Slack token", prefix: "xoxa-", chars: dashed, min: 20 },
    Token { name: "an AWS access key", prefix: "AKIA", chars: upper, min: 16 },
    Token { name: "an AWS access key", prefix: "ASIA", chars: upper, min: 16 },
    Token { name: "a Stripe key", prefix: "sk_live_", chars: alnum, min: 24 },
    Token { name: "a Stripe key", prefix: "rk_live_", chars: alnum, min: 24 },
    Token { name: "an Anthropic API key", prefix: "sk-ant-", chars: dashed, min: 32 },
    Token { name: "an OpenAI API key", prefix: "sk-proj-", chars: dashed, min: 32 },
    Token { name: "a Google API key", prefix: "AIza", chars: dashed, min: 35 },
    Token { name: "an npm token", prefix: "npm_", chars: alnum, min: 36 },
    Token { name: "a PyPI token", prefix: "pypi-AgE", chars: dashed, min: 50 },
];

/// Why a file looks like a secret; `None` when it does not.
pub fn check(meta: &Meta) -> Option<String> {
    if meta.kind != Kind::File {
        return None;
    }
    if perms::is_private(meta.mode) {
        return Some(format!(
            "it is private (mode {})",
            perms::show(meta.mode & 0o777)
        ));
    }
    let mut data = Vec::new();
    File::open(&meta.path)
        .and_then(|f| f.take(SCAN_BYTES).read_to_end(&mut data))
        .ok()?;
    scan(&data)
}

/// The first key or token in `data`, described.
fn scan(data: &[u8]) -> Option<String> {
    if KEY_MARKERS
        .iter()
        .any(|m| find(data, m.as_bytes()).is_some())
    {
        return Some("it holds a private key".to_owned());
    }
    for t in TOKENS {
        let mut from = 0;
        while let Some(at) = find(&data[from..], t.prefix.as_bytes()).map(|i| i + from) {
            from = at + 1;
            // Standing on its own: not the tail of a longer word.
            if at > 0 && dashed(data[at - 1]) {
                continue;
            }
            let rest = &data[at + t.prefix.len()..];
            let run = rest.iter().take_while(|b| (t.chars)(**b)).count();
            if run >= t.min {
                return Some(format!("it holds what looks like {}", t.name));
            }
        }
    }
    None
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_and_tokens_are_found() {
        let key = b"-----BEGIN OPENSSH PRIVATE KEY-----\nb3BlbnNzaC1rZXk=\n";
        assert_eq!(scan(key).as_deref(), Some("it holds a private key"));
        let gh = format!("oauth_token: ghp_{}\n", "a1B2".repeat(9));
        assert_eq!(
            scan(gh.as_bytes()).as_deref(),
            Some("it holds what looks like a GitHub token")
        );
        let aws = b"aws_access_key_id = AKIAIOSFODNN7EXAMPLE\n";
        assert_eq!(
            scan(aws).as_deref(),
            Some("it holds what looks like an AWS access key")
        );
    }

    #[test]
    fn ordinary_config_is_left_alone() {
        for text in [
            "export PATH=$HOME/bin:$PATH\nalias gs='git status'\n",
            // Too short, or part of a longer word.
            "ghp_short\n",
            "MYAKIAIOSFODNN7EXAMPLEX\n",
            "task-sk-ant-thing\n",
            "-----BEGIN PUBLIC KEY-----\n",
            "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAI me@host\n",
        ] {
            assert_eq!(scan(text.as_bytes()), None, "{text}");
        }
    }
}
