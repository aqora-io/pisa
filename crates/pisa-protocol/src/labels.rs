//! Labels and idempotency keys on snapshot entries, and entry ids: the rules
//! the router and the workers share.

use std::collections::BTreeMap;

/// The most labels an entry can have.
pub const MAX_LABELS: usize = 64;

/// The longest label key, in bytes.
const MAX_KEY: usize = 128;

/// The longest label value, in bytes.
const MAX_VALUE: usize = 512;

/// The longest idempotency key, in bytes.
const MAX_IDEMPOTENCY_KEY: usize = 256;

/// Checks an entry's labels: at most [`MAX_LABELS`] of them, each key 1–128
/// bytes of `[A-Za-z0-9._/-]` (so `key:value` splits on the first `:`), and
/// each value at most 512 bytes. The error says which rule a label breaks.
pub fn check_labels(labels: &BTreeMap<String, String>) -> Result<(), String> {
    if labels.len() > MAX_LABELS {
        return Err(format!(
            "{} labels; an entry can have at most {MAX_LABELS}",
            labels.len()
        ));
    }
    for (key, value) in labels {
        let allowed = |b: u8| b.is_ascii_alphanumeric() || b"._/-".contains(&b);
        if key.is_empty() || key.len() > MAX_KEY || !key.bytes().all(allowed) {
            return Err(format!(
                "label key {key:?} must be 1 to {MAX_KEY} bytes of letters, digits, '.', '_', '/' and '-'"
            ));
        }
        if value.len() > MAX_VALUE {
            return Err(format!(
                "the value of label {key} is longer than {MAX_VALUE} bytes"
            ));
        }
    }
    Ok(())
}

/// Checks an idempotency key: 1–256 bytes of `[A-Za-z0-9._:/-]`. The error says
/// which rule it breaks.
pub fn check_key(key: &str) -> Result<(), String> {
    let allowed = |b: u8| b.is_ascii_alphanumeric() || b"._:/-".contains(&b);
    if key.is_empty() || key.len() > MAX_IDEMPOTENCY_KEY || !key.bytes().all(allowed) {
        return Err(format!(
            "idempotency key {key:?} must be 1 to {MAX_IDEMPOTENCY_KEY} bytes of letters, digits, '.', '_', ':', '/' and '-'"
        ));
    }
    Ok(())
}

/// Whether `s` is an entry id: 32 lowercase hex digits.
pub fn is_entry_id(s: &str) -> bool {
    s.len() == 32 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// The error for a path segment that isn't an entry id.
pub fn entry_id_error(s: &str) -> String {
    format!(
        "{s} is not a snapshot entry: use the `entry` that creating the snapshot returned, \
         32 lowercase hex digits. Names like PisaSnap.s… are module names, for imports"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn labels(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn labels_follow_the_rules() {
        assert_eq!(check_labels(&labels(&[("a.b/c-d_1", "x")])), Ok(()));
        assert_eq!(check_labels(&labels(&[("a", "")])), Ok(()));
        let longest = labels(&[(&"k".repeat(128), &"v".repeat(512))]);
        assert_eq!(check_labels(&longest), Ok(()));
        for bad in ["", &"k".repeat(129), "a b", "a:b"] {
            assert!(check_labels(&labels(&[(bad, "x")])).is_err(), "{bad:?}");
        }
        assert!(check_labels(&labels(&[("a", &"v".repeat(513))])).is_err());

        let many = |n: usize| (0..n).map(|i| (i.to_string(), String::new())).collect();
        assert_eq!(check_labels(&many(MAX_LABELS)), Ok(()));
        assert!(check_labels(&many(65)).is_err());
    }

    #[test]
    fn keys_follow_the_rules() {
        assert_eq!(check_key("run-1:step/2.a_b"), Ok(()));
        assert_eq!(check_key(&"k".repeat(256)), Ok(()));
        for bad in ["", &"k".repeat(257), "has space", "a=b", "é"] {
            assert!(check_key(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn entry_ids_are_32_lowercase_hex() {
        assert!(is_entry_id(&uuid::Uuid::new_v4().simple().to_string()));
        let hex = "0123456789abcdef0123456789abcdef";
        assert!(is_entry_id(hex));
        assert!(!is_entry_id(&format!("PisaSnap.s{hex}")));
        assert!(!is_entry_id(&hex.to_uppercase()));
        assert!(!is_entry_id(&hex[1..]));

        let error = entry_id_error(&format!("PisaSnap.s{hex}"));
        assert!(
            error.contains("entry") && error.contains("module"),
            "{error}"
        );
    }
}
