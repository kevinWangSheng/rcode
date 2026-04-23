//! HTTP-hook header preparation.
//!
//! Mirrors the TS helpers in `src/utils/hooks/execHttpHook.ts`:
//! - `${NAME}` / `$NAME` env-var interpolation in header values
//!   (and header names as a defence-in-depth extra — TS only interpolates
//!   values, but we lock down both against configs that someone may build
//!   programmatically).
//! - CR / LF / NUL sanitisation to stop header-injection (CRLF-injection)
//!   attacks via crafted env-var values.
//!
//! Divergence from TS:
//! - TS *strips* `\r`, `\n`, and `\0` silently and replaces unknown env
//!   vars with an empty string. We **reject** both with a typed error —
//!   the parity roadmap flagged this as a security fix, and silently
//!   dropping bytes hides a mis-configured header from the user. Callers
//!   that want the TS behaviour can `.unwrap_or_default()` the result
//!   of `sanitize_header_value`, but the default is fail-loud.
//! - TS intersects with a caller-provided `allowedEnvVars` allowlist;
//!   we model this too via the `env_allowlist` arg. Passing `None`
//!   means "any env var is allowed" — matches the roadmap spec.
//!   Passing `Some(&[])` blocks all interpolation.
//!
//! These helpers are pure: the actual HTTP POST still lives outside this
//! crate (see follow-up batch — `cc-hooks` does not yet ship an HTTP
//! client). Exposing them here lets the eventual client re-use exactly
//! the same sanitisation path as the existing tests lock in.

use std::collections::HashMap;

/// Error returned when a header name or value cannot be prepared safely.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum HeaderPrepError {
    /// Header contained a CR, LF, or NUL byte after interpolation —
    /// would allow HTTP-header injection.
    #[error("header {field} contains control byte 0x{byte:02x} (CR/LF/NUL not allowed)")]
    ControlByte {
        /// "name" or "value".
        field: &'static str,
        byte: u8,
    },
    /// Referenced `${FOO}` / `$FOO` but `FOO` is unset in the provided env.
    #[error("env var ${name} referenced in header {field} is unset")]
    UnsetEnvVar { field: &'static str, name: String },
    /// Referenced `${FOO}` but `FOO` is not in the allowlist.
    #[error("env var ${name} referenced in header {field} is not in the allowlist")]
    DisallowedEnvVar { field: &'static str, name: String },
}

/// Reject if the string contains any CR, LF, or NUL byte.
///
/// Unlike TS's `sanitizeHeaderValue` which silently strips, we fail
/// loud — a control byte in a header value is always a bug or an attack.
fn reject_control_bytes(field: &'static str, s: &str) -> Result<(), HeaderPrepError> {
    for b in s.bytes() {
        if b == b'\r' || b == b'\n' || b == 0 {
            return Err(HeaderPrepError::ControlByte { field, byte: b });
        }
    }
    Ok(())
}

/// Expand `${NAME}` and `$NAME` occurrences in `s` using `env`.
///
/// * `allowlist = None` → any env var may be referenced.
/// * `allowlist = Some(&set)` → only names in `set` may be referenced.
/// * Unset (or disallowed) names → error — we match the roadmap spec
///   (unset-var = error) rather than TS's silent empty-string.
fn interpolate(
    field: &'static str,
    s: &str,
    env: &HashMap<String, String>,
    allowlist: Option<&[String]>,
) -> Result<String, HeaderPrepError> {
    let bytes = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'$' {
            // Safe because we're stepping by valid utf8 boundary — $ is ASCII so
            // any non-$ byte can be copied and the next iteration will either
            // land on another ASCII byte or the start of a multibyte sequence.
            out.push(bytes[i] as char);
            i += 1;
            continue;
        }

        // Either `${NAME}` or `$NAME`.
        let (name, consumed) = if i + 1 < bytes.len() && bytes[i + 1] == b'{' {
            // `${NAME}` — find closing `}`.
            let rest = &bytes[i + 2..];
            let Some(close) = rest.iter().position(|&b| b == b'}') else {
                // Unterminated `${` — treat literally.
                out.push('$');
                i += 1;
                continue;
            };
            let name_bytes = &rest[..close];
            let Some(n) = take_identifier(name_bytes) else {
                // `${}` or `${1}` — not a valid identifier; keep literal.
                out.push('$');
                i += 1;
                continue;
            };
            if n.len() != name_bytes.len() {
                // e.g. `${FOO-bar}` — not a pure identifier. Keep literal.
                out.push('$');
                i += 1;
                continue;
            }
            (n, 2 + close + 1) // `${` + name + `}`
        } else {
            // `$NAME` — greedy identifier.
            let rest = &bytes[i + 1..];
            let Some(n) = take_identifier(rest) else {
                // `$` not followed by identifier char — keep literal.
                out.push('$');
                i += 1;
                continue;
            };
            let len = n.len();
            (n, 1 + len)
        };

        if let Some(list) = allowlist {
            if !list.iter().any(|x| x == &name) {
                return Err(HeaderPrepError::DisallowedEnvVar { field, name });
            }
        }
        let Some(val) = env.get(&name) else {
            return Err(HeaderPrepError::UnsetEnvVar { field, name });
        };
        out.push_str(val);
        i += consumed;
    }
    Ok(out)
}

/// Take a leading ASCII-identifier run: `[A-Za-z_][A-Za-z0-9_]*`.
/// Returns None if the first byte isn't a valid start char.
fn take_identifier(bytes: &[u8]) -> Option<String> {
    let first = *bytes.first()?;
    if !(first.is_ascii_alphabetic() || first == b'_') {
        return None;
    }
    let end = bytes
        .iter()
        .position(|b| !(b.is_ascii_alphanumeric() || *b == b'_'))
        .unwrap_or(bytes.len());
    Some(String::from_utf8_lossy(&bytes[..end]).into_owned())
}

/// Prepare a single (name, value) header pair: interpolate env vars and
/// reject CR/LF/NUL. Both fields are checked.
///
/// The header name is interpolated too so a mis-configured `$FOO` in
/// the name is caught at prep time rather than leaking as the literal
/// three-byte sequence `$FO` to the wire.
pub fn prepare_header(
    name: &str,
    value: &str,
    env: &HashMap<String, String>,
    env_allowlist: Option<&[String]>,
) -> Result<(String, String), HeaderPrepError> {
    let out_name = interpolate("name", name, env, env_allowlist)?;
    let out_value = interpolate("value", value, env, env_allowlist)?;
    reject_control_bytes("name", &out_name)?;
    reject_control_bytes("value", &out_value)?;
    Ok((out_name, out_value))
}

/// Prepare a batch of headers.
///
/// On error, the first offending header short-circuits — the caller does
/// not get a partial result (same as TS: `axios.post` is never called
/// if any interpolation failed).
pub fn prepare_headers(
    headers: &HashMap<String, String>,
    env: &HashMap<String, String>,
    env_allowlist: Option<&[String]>,
) -> Result<HashMap<String, String>, HeaderPrepError> {
    let mut out = HashMap::with_capacity(headers.len());
    // Stable iteration for deterministic errors across BTreeMap → HashMap
    // is not critical here, but tests assert on *which* error fires, so
    // order-in-test must feed in a deterministic map. We don't sort.
    for (k, v) in headers {
        let (nk, nv) = prepare_header(k, v, env, env_allowlist)?;
        out.insert(nk, nv);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn interpolate_braced_and_unbraced() {
        let e = env(&[("TOKEN", "abc"), ("USER", "alice")]);
        let got = interpolate("value", "Bearer ${TOKEN}", &e, None).unwrap();
        assert_eq!(got, "Bearer abc");
        let got = interpolate("value", "Bearer $TOKEN for $USER", &e, None).unwrap();
        assert_eq!(got, "Bearer abc for alice");
    }

    #[test]
    fn interpolate_unset_is_error() {
        let e = env(&[]);
        let err = interpolate("value", "Bearer $TOKEN", &e, None).unwrap_err();
        assert!(matches!(err, HeaderPrepError::UnsetEnvVar { ref name, .. } if name == "TOKEN"));
    }

    #[test]
    fn interpolate_disallowed_env_var() {
        let e = env(&[("SECRET", "xyz")]);
        let allow: Vec<String> = vec!["PUBLIC".into()];
        let err = interpolate("value", "Bearer $SECRET", &e, Some(&allow)).unwrap_err();
        assert!(
            matches!(err, HeaderPrepError::DisallowedEnvVar { ref name, .. } if name == "SECRET")
        );
    }

    #[test]
    fn interpolate_keeps_unterminated_dollar() {
        let e = env(&[]);
        // Trailing `$` with nothing after — kept literal.
        let got = interpolate("value", "cost $", &e, None).unwrap();
        assert_eq!(got, "cost $");
    }

    #[test]
    fn interpolate_keeps_unterminated_braces() {
        let e = env(&[]);
        // Unterminated `${FOO` — kept literal.
        let got = interpolate("value", "cost ${FOO unterminated", &e, None).unwrap();
        assert_eq!(got, "cost ${FOO unterminated");
    }

    #[test]
    fn interpolate_non_identifier_after_dollar() {
        let e = env(&[]);
        let got = interpolate("value", "price: $100", &e, None).unwrap();
        assert_eq!(got, "price: $100");
    }

    #[test]
    fn reject_cr_in_value() {
        let e = env(&[("INJ", "abc\r\nX-Evil: 1")]);
        let err = prepare_header("X-Auth", "$INJ", &e, None).unwrap_err();
        assert!(matches!(
            err,
            HeaderPrepError::ControlByte {
                field: "value",
                byte: b'\r'
            }
        ));
    }

    #[test]
    fn reject_lf_in_value() {
        let e = env(&[("INJ", "abc\nEvil: 1")]);
        let err = prepare_header("X-Auth", "$INJ", &e, None).unwrap_err();
        assert!(matches!(
            err,
            HeaderPrepError::ControlByte {
                field: "value",
                byte: b'\n'
            }
        ));
    }

    #[test]
    fn reject_nul_in_value() {
        let e = env(&[("INJ", "abc\0def")]);
        let err = prepare_header("X-Auth", "$INJ", &e, None).unwrap_err();
        assert!(matches!(
            err,
            HeaderPrepError::ControlByte {
                field: "value",
                byte: 0
            }
        ));
    }

    #[test]
    fn reject_cr_in_name() {
        let e = env(&[("INJ", "Extra\r\nName")]);
        // `$INJ: v` as the header name expands and triggers rejection.
        let err = prepare_header("X-${INJ}", "v", &e, None).unwrap_err();
        assert!(matches!(
            err,
            HeaderPrepError::ControlByte { field: "name", .. }
        ));
    }

    #[test]
    fn happy_path_mixed_headers() {
        let e = env(&[("TOKEN", "s3cret"), ("APP", "cc-rust")]);
        let mut h = HashMap::new();
        h.insert("Authorization".to_string(), "Bearer $TOKEN".to_string());
        h.insert("X-App".to_string(), "${APP}".to_string());
        h.insert("Content-Type".to_string(), "application/json".to_string());
        let got = prepare_headers(&h, &e, None).unwrap();
        assert_eq!(got.get("Authorization").unwrap(), "Bearer s3cret");
        assert_eq!(got.get("X-App").unwrap(), "cc-rust");
        assert_eq!(got.get("Content-Type").unwrap(), "application/json");
    }
}
