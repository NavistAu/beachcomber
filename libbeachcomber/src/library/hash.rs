//! Hashing and encoding: `hash`, `b64encode` and `b64decode`. (`urlencode` is
//! minijinja's own.)

use super::{UNDEFINED, is_absent, text};
use crate::engine::{EngineCtx, op_error};
use base64::Engine as _;
use base64::engine::{DecodePaddingMode, GeneralPurpose, GeneralPurposeConfig, general_purpose};
use minijinja::value::{Value as MjValue, ValueKind};
use minijinja::{Environment, Error};
use std::fmt::Write as _;
use std::sync::Arc;

pub(super) fn add_hash(env: &mut Environment<'_>, _: &Arc<EngineCtx>) {
    env.add_filter("hash", hash);
}

pub(super) fn add_b64encode(env: &mut Environment<'_>, _: &Arc<EngineCtx>) {
    env.add_filter("b64encode", b64encode);
}

pub(super) fn add_b64decode(env: &mut Environment<'_>, _: &Arc<EngineCtx>) {
    env.add_filter("b64decode", b64decode);
}

/// The algorithms `hash` knows.
const ALGORITHMS: &[&str] = &["md5", "sha1", "sha256", "crc32"];

fn hex(bytes: &[u8]) -> String {
    bytes.iter().fold(String::new(), |mut out, b| {
        let _ = write!(out, "{b:02x}");
        out
    })
}

fn digest(alg: &str, data: &[u8]) -> String {
    match alg {
        "md5" => hex(&<md5::Md5 as md5::Digest>::digest(data)),
        "sha1" => hex(&<sha1::Sha1 as sha1::Digest>::digest(data)),
        "sha256" => hex(&<sha2::Sha256 as sha2::Digest>::digest(data)),
        _ => format!("{:08x}", crc32fast::hash(data)),
    }
}

/// `v | hash(alg)`: the lowercase hex digest of a string's bytes (`crc32` is
/// eight hex digits). Anything else is hashed as its JSON, keys sorted. `alg`
/// is checked first.
fn hash(v: MjValue, alg: MjValue) -> Result<MjValue, Error> {
    let alg = match alg.as_str() {
        Some(a) if ALGORITHMS.contains(&a) => a,
        _ => {
            return Err(op_error(format!(
                "hash(): algorithm {alg} is not one of {}",
                ALGORITHMS.join(", ")
            )));
        }
    };
    if is_absent(&v) {
        return Ok(UNDEFINED);
    }
    let bytes = match v.kind() {
        ValueKind::String => v.as_str().unwrap_or_default().as_bytes().to_vec(),
        _ => serde_json::to_vec(
            // Through `Value`, whose objects keep their keys sorted.
            &serde_json::to_value(&v).map_err(|e| op_error(format!("hash(): {e}")))?,
        )
        .map_err(|e| op_error(format!("hash(): {e}")))?,
    };
    Ok(MjValue::from(digest(alg, &bytes)))
}

/// `s | b64encode`: standard base64 with padding.
fn b64encode(v: MjValue) -> Result<MjValue, Error> {
    if is_absent(&v) {
        return Ok(UNDEFINED);
    }
    let s = text("b64encode", &v)?;
    Ok(MjValue::from(general_purpose::STANDARD.encode(s)))
}

/// Standard base64, padding optional on the way in.
const LENIENT: GeneralPurpose = GeneralPurpose::new(
    &base64::alphabet::STANDARD,
    GeneralPurposeConfig::new().with_decode_padding_mode(DecodePaddingMode::Indifferent),
);

/// `s | b64decode`: the UTF-8 text a standard base64 string holds.
fn b64decode(v: MjValue) -> Result<MjValue, Error> {
    if is_absent(&v) {
        return Ok(UNDEFINED);
    }
    let s = text("b64decode", &v)?;
    let bytes = LENIENT
        .decode(s.trim())
        .map_err(|e| op_error(format!("b64decode(): not base64: {e}")))?;
    String::from_utf8(bytes)
        .map(MjValue::from)
        .map_err(|_| op_error("b64decode(): the bytes are not UTF-8"))
}

#[cfg(test)]
mod tests {
    use crate::library::testing::eval;

    fn ok(expr: &str) -> String {
        eval(expr).unwrap_or_else(|e| panic!("{expr}: {e}"))
    }

    #[test]
    fn each_algorithm_gives_its_known_digest() {
        for (alg, input, want) in [
            ("md5", "beachcomber", "80944584e48c324c6ffa144966c3cee7"),
            ("md5", "", "d41d8cd98f00b204e9800998ecf8427e"),
            (
                "sha1",
                "beachcomber",
                "94b787605f7e799368de16ee0503e07239b28d37",
            ),
            ("sha1", "", "da39a3ee5e6b4b0d3255bfef95601890afd80709"),
            (
                "sha256",
                "beachcomber",
                "32ad91e22a8eaba09b7e56b797aa8b6ef0ddf404b65579239a523f669b1f63df",
            ),
            (
                "sha256",
                "",
                "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
            ),
            ("crc32", "beachcomber", "48484316"),
            ("crc32", "123456789", "cbf43926"),
            ("crc32", "", "00000000"),
        ] {
            assert_eq!(
                ok(&format!("'{input}' | hash('{alg}')")),
                want,
                "{alg} {input:?}"
            );
        }
    }

    #[test]
    fn a_non_string_is_hashed_as_its_json_with_keys_sorted() {
        // sha256 of the text `{"a":1,"b":[1,2]}`.
        let want = ok(r#"'{"a":1,"b":[1,2]}' | hash('sha256')"#);
        assert_eq!(ok("{'b': [1, 2], 'a': 1} | hash('sha256')"), want);
        assert_eq!(ok("{'a': 1, 'b': [1, 2]} | hash('sha256')"), want);
        assert_eq!(ok("5 | hash('crc32')"), ok("'5' | hash('crc32')"));
        assert_eq!(ok("[1, 2] | hash('md5')"), ok("'[1,2]' | hash('md5')"));
    }

    #[test]
    fn hash_of_nothing_is_undefined_after_alg_is_checked() {
        assert_eq!(ok("(none | hash('md5')) is undefined"), "true");
        assert_eq!(ok("(nope | hash('sha1')) is undefined"), "true");
        for bad in ["'sha512'", "'SHA256'", "1", "none"] {
            for subject in ["'x'", "none"] {
                let err = eval(&format!("{subject} | hash({bad})")).unwrap_err();
                assert!(
                    err.contains("hash(): algorithm") && err.contains("md5, sha1, sha256, crc32"),
                    "{subject} {bad}: {err}"
                );
            }
        }
    }

    #[test]
    fn base64_round_trips() {
        assert_eq!(ok("'hi' | b64encode"), "aGk=");
        assert_eq!(ok("'aGk=' | b64decode"), "hi");
        assert_eq!(
            ok("'aGk' | b64decode"),
            "hi",
            "padding is optional on decode"
        );
        assert_eq!(ok("'' | b64encode"), "");
        assert_eq!(ok("'' | b64decode"), "");
        assert_eq!(ok("'héllo ✓' | b64encode | b64decode"), "héllo ✓");
        assert_eq!(ok("12 | b64encode"), "MTI=");
    }

    #[test]
    fn base64_of_nothing_is_undefined() {
        for f in ["b64encode", "b64decode"] {
            assert_eq!(ok(&format!("(none | {f}) is undefined")), "true");
            assert_eq!(ok(&format!("(nope | {f}) is undefined")), "true");
        }
    }

    #[test]
    fn base64_rejects_bad_input() {
        let err = eval("[1] | b64encode").unwrap_err();
        assert!(err.contains("b64encode(): expected text"), "{err}");
        let err = eval("'not base64!' | b64decode").unwrap_err();
        assert!(err.contains("b64decode(): not base64"), "{err}");
        // `/w==` is the single byte 0xff.
        let err = eval("'/w==' | b64decode").unwrap_err();
        assert!(
            err.contains("b64decode(): the bytes are not UTF-8"),
            "{err}"
        );
        let err = eval("{} | b64decode").unwrap_err();
        assert!(err.contains("b64decode(): expected text"), "{err}");
    }

    #[test]
    fn urlencode_is_minijinjas() {
        assert_eq!(ok("{'q': 'a b'} | urlencode"), "q=a%20b");
        assert_eq!(ok("'a b/c' | urlencode"), "a%20b/c");
        assert_eq!(ok("(none | urlencode) is undefined"), "true");
        assert_eq!(ok("(nope | urlencode) is undefined"), "true");
    }
}
