use std::fmt::{self, Display, Formatter};

use serde::{Deserialize, Serialize};

use crate::{Error, Result};

/// Canonical serialized identity of one semantic application Target.
///
/// A name has an owning kebab-case namespace, a dotted semantic path, and an
/// optional slash-delimited tuple of encoded runtime identities:
/// `owner.semantic.path[/identity...]`. Semantic material always precedes the
/// first slash. Identity components admit ASCII unreserved characters and
/// canonical percent escapes.
#[derive(Clone, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct TargetName(String);

impl TargetName {
    /// Admit one canonical Target wire name.
    pub fn new(name: impl Into<String>) -> Result<Self> {
        let name = name.into();
        validate(&name)?;
        Ok(Self(name))
    }

    /// Borrow the canonical wire spelling.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Release the canonical wire spelling.
    #[must_use]
    pub fn into_string(self) -> String {
        self.0
    }
}

impl AsRef<str> for TargetName {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl Display for TargetName {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl TryFrom<String> for TargetName {
    type Error = Error;

    fn try_from(name: String) -> Result<Self> {
        Self::new(name)
    }
}

impl TryFrom<&str> for TargetName {
    type Error = Error;

    fn try_from(name: &str) -> Result<Self> {
        Self::new(name)
    }
}

fn validate(name: &str) -> Result<()> {
    if name.len() > 512 {
        return Err(fault(name, "name exceeds 512 bytes"));
    }
    let mut parts = name.split('/');
    let semantic = parts.next().unwrap_or_default();
    let mut segments = semantic.split('.');
    let owner = segments.next().unwrap_or_default();
    if !valid_kebab(owner) {
        return Err(fault(name, "owner is not a kebab-case segment"));
    }
    let path = segments.collect::<Vec<_>>();
    if path.is_empty() || !path.iter().all(|segment| valid_kebab(segment)) {
        return Err(fault(
            name,
            "semantic path requires one or more kebab-case segments",
        ));
    }
    if !parts.all(valid_identity) {
        return Err(fault(
            name,
            "runtime identity is empty or not canonically encoded",
        ));
    }
    Ok(())
}

fn fault(name: &str, detail: &'static str) -> Error {
    Error::TargetName {
        name: name.to_owned(),
        detail,
    }
}

fn valid_kebab(segment: &str) -> bool {
    let bytes = segment.as_bytes();
    bytes.first().is_some_and(u8::is_ascii_lowercase)
        && bytes.last().is_some_and(u8::is_ascii_alphanumeric)
        && bytes
            .iter()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || *byte == b'-')
        && !bytes.windows(2).any(|pair| pair == b"--")
}

fn valid_identity(identity: &str) -> bool {
    let bytes = identity.as_bytes();
    if bytes.is_empty() {
        return false;
    }
    let mut index = 0;
    while index < bytes.len() {
        let byte = bytes[index];
        if byte == b'%' {
            if index + 2 >= bytes.len()
                || !canonical_hex(bytes[index + 1])
                || !canonical_hex(bytes[index + 2])
            {
                return false;
            }
            let decoded = (hex_value(bytes[index + 1]) << 4) | hex_value(bytes[index + 2]);
            if decoded.is_ascii_alphanumeric() || matches!(decoded, b'-' | b'.' | b'_' | b'~') {
                return false;
            }
            index += 3;
        } else if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            index += 1;
        } else {
            return false;
        }
    }
    true
}

fn canonical_hex(byte: u8) -> bool {
    byte.is_ascii_digit() || matches!(byte, b'A'..=b'F')
}

fn hex_value(byte: u8) -> u8 {
    match byte {
        b'0'..=b'9' => byte - b'0',
        b'A'..=b'F' => byte - b'A' + 10,
        _ => unreachable!("validated hexadecimal digit"),
    }
}

#[cfg(test)]
mod tests {
    use super::TargetName;

    #[test]
    fn canonical_names_separate_semantics_from_identity() {
        for name in [
            "eternalist.application.help",
            "hrrr.map.pin/0",
            "trailgen.editor.support.coordinates/12",
            "wrangler.card.activate/codex/0198f4f3-90cc-7a21-b846-7d632cf24b18",
            "abv.cabinet.filter.entry/work%2Fplay",
        ] {
            assert_eq!(
                TargetName::new(name).expect("canonical Target").as_str(),
                name
            );
        }
    }

    #[test]
    fn rival_dialects_are_rejected() {
        for name in [
            "application",
            "panel/forecast",
            "recess:ui",
            "comparison.reject_stream/left",
            "abv.filter.entry/work play",
            "abv.filter.entry/work%2fplay",
            "abv.filter.entry/%41",
        ] {
            assert!(TargetName::new(name).is_err(), "admitted `{name}`");
        }
    }
}
