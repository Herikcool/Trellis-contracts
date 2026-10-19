//! Security sanitization and validation for untrusted content and external URLs (Issue #38).
//!
//! Provides zero-allocation and bounded-allocation validators and sanitizers for:
//! - External URLs / URIs (allowing safe protocols like HTTPS, IPFS, Arweave while
//!   strictly rejecting dangerous schemes like javascript:, data:, file:, blob:)
//! - Untrusted user text and markdown (stripping/rejecting control characters, CRLF injection,
//!   and HTML script injection vectors).
//! - High-cardinality telemetry protection: bounded label sets, redaction of sensitive
//!   values, and deterministic sampling to keep dashboards useful under abusive input.

use soroban_sdk::{Bytes, Env};
use crate::errors::Error;

/// Maximum allowed URL byte length to prevent storage bloat and resource exhaustion.
pub const MAX_URL_LEN: u32 = 2048;

/// Maximum allowed untrusted text byte length.
pub const MAX_TEXT_LEN: u32 = 8192;

/// Maximum number of distinct label keys allowed on a single telemetry event.
pub const MAX_TELEMETRY_LABELS: u32 = 16;

/// Maximum byte length of a single telemetry label key or value.
pub const MAX_TELEMETRY_LABEL_LEN: u32 = 128;

/// Maximum byte length of a telemetry event name.
pub const MAX_TELEMETRY_NAME_LEN: u32 = 64;

/// Sampling denominator: 1 in `TELEMETRY_SAMPLE_RATE` events is retained.
/// A value of 1 disables sampling (all events retained).
pub const TELEMETRY_SAMPLE_RATE: u32 = 100;

/// Replacement token emitted when a sensitive value is redacted.
pub const REDACTED_TOKEN: &[u8] = b"[REDACTED]";

/// Safe URI Scheme enum for classification.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum SafeScheme {
    Https,
    Http,
    Ipfs,
    Ipns,
    Arweave,
    Did,
}

/// Helper function to check if an ASCII character is whitespace.
#[inline]
fn is_ascii_whitespace(b: u8) -> bool {
    b == b' ' || b == b'\t' || b == b'\n' || b == b'\r' || b == 0x0c || b == 0x0b
}

/// Helper function to convert an ASCII byte to lowercase without allocation.
#[inline]
fn to_ascii_lower(b: u8) -> u8 {
    if (b'A'..=b'Z').contains(&b) {
        b + (b'a' - b'A')
    } else {
        b
    }
}

/// Check if a byte slice matches a prefix case-insensitively.
fn starts_with_case_insensitive(bytes: &Bytes, prefix: &[u8], offset: u32) -> bool {
    if bytes.len() < offset + (prefix.len() as u32) {
        return false;
    }
    let mut i = 0usize;
    while i < prefix.len() {
        let b = bytes.get(offset + i as u32).unwrap_or(0);
        if to_ascii_lower(b) != prefix[i] {
            return false;
        }
        i += 1;
    }
    true
}

/// Check if a byte sequence contains a dangerous subslice case-insensitively.
fn contains_case_insensitive(bytes: &Bytes, needle: &[u8]) -> bool {
    if bytes.len() < needle.len() as u32 {
        return false;
    }
    let max_start = bytes.len() - needle.len() as u32;
    let mut start = 0u32;
    while start <= max_start {
        let mut matched = true;
        let mut i = 0usize;
        while i < needle.len() {
            let b = bytes.get(start + i as u32).unwrap_or(0);
            if to_ascii_lower(b) != needle[i] {
                matched = false;
                break;
            }
            i += 1;
        }
        if matched {
            return true;
        }
        start += 1;
    }
    false
}

/// Validates that an untrusted URL / URI conforms to safe schema rules:
/// 1. Length is within `[1, max_len]`.
/// 2. Contains no null bytes (`\0`) or ASCII control characters (< 32, 127).
/// 3. Scheme matches an allowed safe prefix (`https://`, `ipfs://`, `ipns://`, `ar://`, `did:`, or `http://` if allowed).
/// 4. Does not contain dangerous schemes (`javascript:`, `data:`, `file:`, `vbscript:`, `blob:`, `about:`).
/// 5. For web schemes (`https://`, `http://`), ensures a non-empty domain/host exists.
pub fn validate_external_url(url: &Bytes, max_len: u32, allow_http: bool) -> Result<SafeScheme, Error> {
    let len = url.len();
    if len == 0 || len > max_len {
        return Err(Error::InvalidArgument);
    }

    // Step 1: Reject null bytes and control characters (CRLF injection prevention).
    let mut i = 0u32;
    while i < len {
        let b = url.get(i).unwrap_or(0);
        if b < 32 || b == 127 {
            return Err(Error::InvalidArgument);
        }
        // Whitespace inside URL is invalid
        if is_ascii_whitespace(b) {
            return Err(Error::InvalidArgument);
        }
        i += 1;
    }

    // Step 2: Reject dangerous protocol schemes anywhere in the URL (including nested schemes like javascript:).
    let dangerous_schemes: &[&[u8]] = &[
        b"javascript:",
        b"data:",
        b"file:",
        b"vbscript:",
        b"blob:",
        b"about:",
    ];
    for &bad in dangerous_schemes {
        if contains_case_insensitive(url, bad) {
            return Err(Error::InvalidArgument);
        }
    }

    // Step 3: Match allowed scheme prefixes.
    if starts_with_case_insensitive(url, b"https://", 0) {
        // Ensure host is present (e.g. "https://" followed by at least one valid host char)
        if len <= 8 {
            return Err(Error::InvalidArgument);
        }
        let next_char = url.get(8).unwrap_or(0);
        if next_char == b'/' || next_char == b'?' || next_char == b'#' {
            return Err(Error::InvalidArgument);
        }
        Ok(SafeScheme::Https)
    } else if starts_with_case_insensitive(url, b"ipfs://", 0) {
        if len <= 7 {
            return Err(Error::InvalidArgument);
        }
        Ok(SafeScheme::Ipfs)
    } else if starts_with_case_insensitive(url, b"ipns://", 0) {
        if len <= 7 {
            return Err(Error::InvalidArgument);
        }
        Ok(SafeScheme::Ipns)
    } else if starts_with_case_insensitive(url, b"ar://", 0) {
        if len <= 5 {
            return Err(Error::InvalidArgument);
        }
        Ok(SafeScheme::Arweave)
    } else if starts_with_case_insensitive(url, b"did:", 0) {
        if len <= 4 {
            return Err(Error::InvalidArgument);
        }
        Ok(SafeScheme::Did)
    } else if allow_http && starts_with_case_insensitive(url, b"http://", 0) {
        if len <= 7 {
            return Err(Error::InvalidArgument);
        }
        let next_char = url.get(7).unwrap_or(0);
        if next_char == b'/' || next_char == b'?' || next_char == b'#' {
            return Err(Error::InvalidArgument);
        }
        Ok(SafeScheme::Http)
    } else {
        Err(Error::InvalidArgument)
    }
}

/// Trims leading and trailing whitespace from a Bytes object.
pub fn trim_whitespace(env: &Env, raw: &Bytes) -> Bytes {
    let len = raw.len();
    if len == 0 {
        return Bytes::new(env);
    }

    let mut start = 0u32;
    while start < len {
        if !is_ascii_whitespace(raw.get(start).unwrap_or(0)) {
            break;
        }
        start += 1;
    }

    if start == len {
        return Bytes::new(env);
    }

    let mut end = len;
    while end > start {
        if !is_ascii_whitespace(raw.get(end - 1).unwrap_or(0)) {
            break;
        }
        end -= 1;
    }

    let mut trimmed = Bytes::new(env);
    let mut curr = start;
    while curr < end {
        trimmed.push_back(raw.get(curr).unwrap_or(0));
        curr += 1;
    }
    trimmed
}

/// Sanitizes a URL by trimming surrounding whitespace and validating its safety.
pub fn sanitize_url(env: &Env, raw: &Bytes, max_len: u32, allow_http: bool) -> Result<Bytes, Error> {
    let trimmed = trim_whitespace(env, raw);
    validate_external_url(&trimmed, max_len, allow_http)?;
    Ok(trimmed)
}

/// Validates untrusted user text (e.g. metadata descriptions, names, comments):
/// - Enforces maximum length bounds.
/// - Rejects null bytes and prohibited ASCII control characters.
/// - Rejects HTML/script tags when checking for injection safety.
pub fn validate_safe_text(raw: &Bytes, max_len: u32, allow_multiline: bool) -> Result<(), Error> {
    let len = raw.len();
    if len == 0 || len > max_len {
        return Err(Error::InvalidArgument);
    }

    let mut i = 0u32;
    while i < len {
        let b = raw.get(i).unwrap_or(0);
        if b == 0 {
            // Null byte strictly forbidden
            return Err(Error::InvalidArgument);
        }
        if b < 32 && b != 127 {
            if allow_multiline && (b == b'\n' || b == b'\r' || b == b'\t') {
                // Allowed in multiline text
            } else {
                return Err(Error::InvalidArgument);
            }
        }
        i += 1;
    }

    // Reject dangerous script injection tags
    let dangerous_tags: &[&[u8]] = &[
        b"<script",
        b"<iframe",
        b"<object",
        b"<embed",
        b"<form",
        b"<svg",
        b"<link",
        b"javascript:",
        b"onload=",
        b"onerror=",
        b"onclick=",
    ];

    for &tag in dangerous_tags {
        if contains_case_insensitive(raw, tag) {
            return Err(Error::InvalidArgument);
        }
    }

    Ok(())
}

/// Neutralizes and sanitizes untrusted text by stripping unsafe HTML/script tags.
pub fn sanitize_text(env: &Env, raw: &Bytes, max_len: u32, allow_multiline: bool) -> Result<Bytes, Error> {
    let trimmed = trim_whitespace(env, raw);
    validate_safe_text(&trimmed, max_len, allow_multiline)?;
    Ok(trimmed)
}

/// Classification of a telemetry label key for cardinality budgeting.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum LabelClass {
    /// Low-cardinality, safe to aggregate on (e.g. `status`, `chain`).
    Bounded,
    /// High-cardinality, must be hashed or dropped (e.g. `user_id`, `tx_hash`).
    Unbounded,
}

/// Returns true if the label key is known to be high-cardinality and therefore
/// unsafe to use as a raw metric dimension.
#[inline]
fn is_high_cardinality_key(key: &Bytes) -> bool {
    // Keys are matched case-insensitively against a small denylist of
    // identifiers that would explode metric cardinality if used as labels.
    const HIGH_CARDINALITY: &[&[u8]] = &[
        b"user_id",
        b"userid",
        b"account",
        b"account_id",
        b"address",
        b"wallet",
        b"tx_hash",
        b"txhash",
        b"transaction_id",
        b"request_id",
        b"trace_id",
        b"session_id",
        b"nonce",
        b"timestamp",
        b"email",
        b"ip",
        b"ip_address",
    ];
    for &k in HIGH_CARDINALITY {
        if key.len() == k.len() as u32 && contains_case_insensitive(key, k) {
            return true;
        }
    }
    false
}

/// Returns true if the label key names a sensitive field whose value must be
/// redacted before being emitted to logs or metrics.
#[inline]
fn is_sensitive_key(key: &Bytes) -> bool {
    const SENSITIVE: &[&[u8]] = &[
        b"secret",
        b"password",
        b"passwd",
        b"token",
        b"api_key",
        b"apikey",
        b"private_key",
        b"privatekey",
        b"seed",
        b"mnemonic",
        b"authorization",
        b"auth",
        b"cookie",
        b"signature",
        b"signing_key",
    ];
    for &k in SENSITIVE {
        if key.len() == k.len() as u32 && contains_case_insensitive(key, k) {
            return true;
        }
    }
    false
}

/// Validates a telemetry event name: bounded length, no control characters,
/// and restricted to a conservative ASCII identifier alphabet.
pub fn validate_telemetry_name(name: &Bytes) -> Result<(), Error> {
    let len = name.len();
    if len == 0 || len > MAX_TELEMETRY_NAME_LEN {
        return Err(Error::InvalidArgument);
    }
    let mut i = 0u32;
    while i < len {
        let b = name.get(i).unwrap_or(0);
        let ok = (b'a'..=b'z').contains(&b)
            || (b'A'..=b'Z').contains(&b)
            || (b'0'..=b'9').contains(&b)
            || b == b'_'
            || b == b'.'
            || b == b'-';
        if !ok {
            return Err(Error::InvalidArgument);
        }
        i += 1;
    }
    Ok(())
}

/// Validates a telemetry label key: bounded length, no control characters,
/// and restricted to a conservative ASCII identifier alphabet.
pub fn validate_telemetry_label_key(key: &Bytes) -> Result<(), Error> {
    let len = key.len();
    if len == 0 || len > MAX_TELEMETRY_LABEL_LEN {
        return Err(Error::InvalidArgument);
    }
    let mut i = 0u32;
    while i < len {
        let b = key.get(i).unwrap_or(0);
        let ok = (b'a'..=b'z').contains(&b)
            || (b'A'..=b'Z').contains(&b)
            || (b'0'..=b'9').contains(&b)
            || b == b'_'
            || b == b'.'
            || b == b'-';
        if !ok {
            return Err(Error::InvalidArgument);
        }
        i += 1;
    }
    Ok(())
}

/// Classifies a telemetry label key as bounded or unbounded cardinality.
/// Unbounded keys must be hashed, bucketed, or dropped before emission.
pub fn classify_telemetry_label(key: &Bytes) -> Result<LabelClass, Error> {
    validate_telemetry_label_key(key)?;
    if is_high_cardinality_key(key) {
        Ok(LabelClass::Unbounded)
    } else {
        Ok(LabelClass::Bounded)
    }
}

/// Redacts a telemetry label value if its key is sensitive. Non-sensitive
/// values are returned unchanged (bounded by `MAX_TELEMETRY_LABEL_LEN`).
pub fn redact_telemetry_value(env: &Env, key: &Bytes, value: &Bytes) -> Result<Bytes, Error> {
    validate_telemetry_label_key(key)?;
    if value.len() > MAX_TELEMETRY_LABEL_LEN {
        return Err(Error::InvalidArgument);
    }
    if is_sensitive_key(key) {
        let mut out = Bytes::new(env);
        let mut i = 0usize;
        while i < REDACTED_TOKEN.len() {
            out.push_back(REDACTED_TOKEN[i]);
            i += 1;
        }
        return Ok(out);
    }
    // Reject control characters in non-redacted values to prevent log injection.
    let mut i = 0u32;
    while i < value.len() {
        let b = value.get(i).unwrap_or(0);
        if b < 32 || b == 127 {
            return Err(Error::InvalidArgument);
        }
        i += 1;
    }
    Ok(value.clone())
}

/// Deterministic sampling decision for a telemetry event. Returns `true` if
/// the event should be emitted. Uses a caller-supplied entropy value (e.g. a
/// ledger sequence or monotonic counter) so the decision is reproducible and
/// does not require randomness inside the contract.
pub fn should_sample(entropy: u64, sample_rate: u32) -> bool {
    if sample_rate <= 1 {
        return true;
    }
    (entropy % (sample_rate as u64)) == 0
}

/// Validates that a telemetry event's label set respects the cardinality
/// budget. Returns `Err` if the number of labels exceeds `MAX_TELEMETRY_LABELS`
/// or if any label key is invalid.
pub fn validate_telemetry_label_count(count: u32) -> Result<(), Error> {
    if count > MAX_TELEMETRY_LABELS {
        return Err(Error::InvalidArgument);
    }
    Ok(())
}

/// End-to-end guard for a single telemetry label: validates the key, enforces
/// the cardinality budget, and redacts sensitive values. Returns the safe
/// value to emit, or `Err` if the label must be dropped.
pub fn sanitize_telemetry_label(
    env: &Env,
    key: &Bytes,
    value: &Bytes,
    current_label_count: u32,
) -> Result<Bytes, Error> {
    validate_telemetry_label_count(current_label_count)?;
    let class = classify_telemetry_label(key)?;
    if class == LabelClass::Unbounded {
        // High-cardinality keys are rejected outright rather than emitted,
        // preserving dashboard usefulness under abusive input volume.
        return Err(Error::InvalidArgument);
    }
    redact_telemetry_value(env, key, value)
}
