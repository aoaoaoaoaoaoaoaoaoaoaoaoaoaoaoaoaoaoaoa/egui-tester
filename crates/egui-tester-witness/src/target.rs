use std::fmt::{self, Display, Formatter, Write as _};

use serde::{Deserialize, Deserializer, Serialize, Serializer, de};

use crate::{Error, Result};

/// Canonical serialized identity of one semantic application Target.
///
/// A name has an owning kebab-case namespace, a dotted semantic path, and an
/// optional slash-delimited tuple of encoded runtime identities:
/// `owner.semantic.path[/identity...]`. Semantic material always precedes the
/// first slash. Identity components admit ASCII unreserved characters and
/// canonical percent escapes; [`encode_identity`] spells raw text that way.
/// Deserialization admits only canonical names, so a reader holds the same law
/// as a publisher.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
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

    /// The semantic material before the first slash, owner and dotted path
    /// together: what every name of one class shares, whatever identities
    /// follow.
    #[must_use]
    pub fn semantic(&self) -> &str {
        self.0
            .split_once('/')
            .map_or(self.as_str(), |(semantic, _)| semantic)
    }

    /// Release the canonical wire spelling.
    #[must_use]
    pub fn into_string(self) -> String {
        self.0
    }
}

impl PartialEq<str> for TargetName {
    fn eq(&self, other: &str) -> bool {
        self.as_str() == other
    }
}

impl PartialEq<&str> for TargetName {
    fn eq(&self, other: &&str) -> bool {
        self.as_str() == *other
    }
}

impl Serialize for TargetName {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for TargetName {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        Self::new(String::deserialize(deserializer)?).map_err(de::Error::custom)
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

/// Spell one raw runtime identity as a canonical identity component: unreserved
/// ASCII stays literal and every other byte becomes an uppercase percent escape,
/// exactly what [`TargetName`] admits after a slash. An empty identity has no
/// spelling, because no name admits an empty component.
pub fn encode_identity(raw: &str) -> Option<impl Display + '_> {
    struct Encoded<'a>(&'a str);

    impl Display for Encoded<'_> {
        fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
            const HEX: &[u8; 16] = b"0123456789ABCDEF";
            for byte in self.0.bytes() {
                if unreserved(byte) {
                    formatter.write_char(char::from(byte))?;
                } else {
                    formatter.write_char('%')?;
                    formatter.write_char(char::from(HEX[usize::from(byte >> 4)]))?;
                    formatter.write_char(char::from(HEX[usize::from(byte & 0x0f)]))?;
                }
            }
            Ok(())
        }
    }

    (!raw.is_empty()).then_some(Encoded(raw))
}

/// RFC 3986 unreserved ASCII: the bytes an identity spells literally.
const fn unreserved(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~')
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
    for identity in parts {
        if identity.is_empty() {
            return Err(fault(name, "runtime identity is empty"));
        }
        if !canonical_identity(identity) {
            return Err(fault(name, "runtime identity is not canonically encoded"));
        }
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

fn canonical_identity(identity: &str) -> bool {
    let bytes = identity.as_bytes();
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
            if unreserved(decoded) {
                return false;
            }
            index += 3;
        } else if unreserved(byte) {
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
    use super::{TargetName, encode_identity};

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

    #[test]
    fn every_encoded_identity_is_admitted() {
        for character in (0..0x0800)
            .chain([0xFFFD, 0x1_F600])
            .filter_map(char::from_u32)
        {
            let raw = format!("{character}a{character}");
            let identity = encode_identity(&raw).expect("a nonempty identity has a spelling");
            let name = format!("owner.path/{identity}");
            assert!(TargetName::new(name).is_ok(), "refused encoded {raw:?}");
        }
    }
}
