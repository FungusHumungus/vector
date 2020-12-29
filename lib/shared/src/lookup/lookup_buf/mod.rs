#![allow(clippy::len_without_is_empty)] // It's invalid to have a lookupbuf that is empty.

use crate::{event::*, lookup::*};
use pest::iterators::Pair;
#[cfg(test)]
use quickcheck::{Arbitrary, Gen};
use remap_lang::parser::ParserRule;
use std::{
    collections::VecDeque,
    convert::TryFrom,
    fmt::{self, Display, Formatter},
    ops::{Index, IndexMut},
    str,
    str::FromStr,
};
use toml::Value as TomlValue;
use tracing::{instrument, trace};

use indexmap::map::IndexMap;
use serde::de::{self, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

#[cfg(test)]
mod test;

/// `LookupBuf`s are pre-validated, owned event lookup paths.
///
/// These are owned, ordered sets of `Segment`s. `Segment`s represent parts of a path such as
/// `pies.banana.slices[0]`. The segments would be `["pies", "banana", "slices", 0]`. You can "walk"
/// a `LookupBuf` with an `iter()` call.
///
/// # Building
///
/// You build `LookupBuf`s from `String`s and other string-like objects with a `from()` or `try_from()`
/// call. **These do not parse the buffer.**
///
/// From there, you can `push` and `pop` onto the `LookupBuf`.
///
/// ```rust
/// use shared::lookup::LookupBuf;
/// let mut lookup = LookupBuf::from("foo");
/// lookup.push_back(1);
/// lookup.push_back("bar");
///
/// let mut lookup = LookupBuf::from("foo.bar"); // This is **not** two segments.
/// lookup.push_back(1);
/// lookup.push_back("bar");
/// ```
///
/// # Parsing
///
/// To parse buffer into a `LookupBuf`, use the `std::str::FromStr` implementation. If you're working
/// something that's not able to be a `str`, you should consult `std::str::from_utf8` and handle the
/// possible error.
///
/// ```rust
/// use shared::lookup::LookupBuf;
/// let mut lookup = LookupBuf::from_str("foo").unwrap();
/// lookup.push_back(1);
/// lookup.push_back("bar");
///
/// let mut lookup = LookupBuf::from_str("foo.bar").unwrap(); // This **is** two segments.
/// lookup.push_back(1);
/// lookup.push_back("bar");
/// ```
///
/// # Unowned Variant
///
/// There exists an unowned variant of this type appropriate for static contexts or where you only
/// have a view into a long lived string. (Say, deserialization of configs).
///
/// To shed ownership use `lookup_buf.clone_lookup()`. To gain ownership of a `lookup`, use
/// `lookup.into()`.
///
/// ```rust
/// use shared::lookup::LookupBuf;
/// let mut lookup = LookupBuf::from_str("foo.bar").unwrap();
/// let mut unowned_view = lookup.clone_lookup();
/// unowned_view.push_back(1);
/// unowned_view.push_back("bar");
/// lookup.push_back("baz"); // Does not impact the view!
/// ```
///
/// For more, investigate `Lookup`.
#[derive(Debug, PartialEq, Default, Eq, PartialOrd, Ord, Clone, Hash)]
pub struct LookupBuf {
    pub segments: VecDeque<SegmentBuf>,
}

impl<'a> TryFrom<Pair<'a, ParserRule>> for LookupBuf {
    type Error = LookupError;

    fn try_from(pair: Pair<'a, ParserRule>) -> Result<Self, Self::Error> {
        let retval = LookupBuf {
            segments: Segment::from_lookup(pair)?
                .into_iter()
                .map(Into::into)
                .collect(),
        };
        retval.is_valid()?;
        Ok(retval)
    }
}

// TODO: Added in https://github.com/timberio/vector/pull/5374, Path will eventually become Lookup.
impl TryFrom<remap_lang::Path> for LookupBuf {
    type Error = LookupError;
    fn try_from(target: remap_lang::Path) -> Result<Self, Self::Error> {
        let path_string = target.to_string();
        trace!(path = %path_string, "Converting to LookupBuf.");
        LookupBuf::from_str(&path_string)
    }
}

// TODO: Added in https://github.com/timberio/vector/pull/5374, Path will eventually become Lookup.
impl TryFrom<&remap_lang::Path> for LookupBuf {
    type Error = LookupError;
    fn try_from(target: &remap_lang::Path) -> Result<Self, Self::Error> {
        let path_string = target.to_string();
        trace!(path = %path_string, "Converting to LookupBuf.");
        LookupBuf::from_str(&path_string)
    }
}

impl<'a> TryFrom<VecDeque<SegmentBuf>> for LookupBuf {
    type Error = LookupError;

    fn try_from(segments: VecDeque<SegmentBuf>) -> Result<Self, Self::Error> {
        let retval = LookupBuf { segments };
        retval.is_valid()?;
        Ok(retval)
    }
}

impl Display for LookupBuf {
    fn fmt(&self, f: &mut Formatter<'_>) -> Result<(), std::fmt::Error> {
        let mut peeker = self.segments.iter().peekable();
        let mut next = peeker.next();
        let mut maybe_next = peeker.peek();
        while let Some(segment) = next {
            match segment {
                SegmentBuf::Field {
                    name: _,
                    requires_quoting: _,
                } => match maybe_next {
                    Some(next) if next.is_field() || next.is_coalesce() => {
                        write!(f, r#"{}."#, segment)?
                    }
                    None | Some(_) => write!(f, "{}", segment)?,
                },
                SegmentBuf::Index(_) => match maybe_next {
                    Some(next) if next.is_field() || next.is_coalesce() => {
                        write!(f, r#"[{}]."#, segment)?
                    }
                    None | Some(_) => write!(f, "[{}]", segment)?,
                },
                SegmentBuf::Coalesce(_) => match maybe_next {
                    Some(next) if next.is_field() || next.is_coalesce() => {
                        write!(f, r#"{}."#, segment)?
                    }
                    None | Some(_) => write!(f, "{}", segment)?,
                },
            }
            next = peeker.next();
            maybe_next = peeker.peek();
        }
        Ok(())
    }
}

impl LookupBuf {
    /// Get from the internal list of segments.
    #[instrument(level = "trace")]
    pub fn get(&mut self, index: usize) -> Option<&SegmentBuf> {
        self.segments.get(index)
    }

    /// Push onto the internal list of segments.
    #[instrument(level = "trace", skip(segment))]
    pub fn push_back(&mut self, segment: impl Into<SegmentBuf>) {
        self.segments.push_back(segment.into());
    }

    #[instrument(level = "trace")]
    pub fn pop_back(&mut self) -> Option<SegmentBuf> {
        self.segments.pop_back()
    }

    #[instrument(level = "trace", skip(segment))]
    pub fn push_front(&mut self, segment: impl Into<SegmentBuf>) {
        self.segments.push_front(segment.into())
    }

    #[instrument(level = "trace")]
    pub fn pop_front(&mut self) -> Option<SegmentBuf> {
        self.segments.pop_front()
    }

    #[instrument(level = "trace")]
    pub fn iter(&self) -> std::collections::vec_deque::Iter<'_, SegmentBuf> {
        self.segments.iter()
    }

    #[instrument(level = "trace")]
    pub fn from_indexmap(
        values: IndexMap<String, TomlValue>,
    ) -> crate::Result<IndexMap<LookupBuf, Value>> {
        let mut discoveries = IndexMap::new();
        for (key, value) in values {
            Self::from_toml_table_recursive_step(
                LookupBuf::try_from(key)?,
                value,
                &mut discoveries,
            )?;
        }
        Ok(discoveries)
    }

    #[instrument(level = "trace")]
    pub fn len(&self) -> usize {
        self.segments.len()
    }

    #[instrument(level = "trace")]
    pub fn from_toml_table(value: TomlValue) -> crate::Result<IndexMap<LookupBuf, Value>> {
        let mut discoveries = IndexMap::new();
        match value {
            TomlValue::Table(map) => {
                for (key, value) in map {
                    Self::from_toml_table_recursive_step(
                        LookupBuf::try_from(key)?,
                        value,
                        &mut discoveries,
                    )?;
                }
                Ok(discoveries)
            }
            _ => Err(format!(
                "A TOML table must be passed to the `from_toml_table` function. Passed: {:?}",
                value
            )
            .into()),
        }
    }

    #[instrument(level = "trace")]
    fn from_toml_table_recursive_step(
        lookup: LookupBuf,
        value: TomlValue,
        discoveries: &mut IndexMap<LookupBuf, Value>,
    ) -> crate::Result<()> {
        match value {
            TomlValue::String(s) => discoveries.insert(lookup, Value::from(s)),
            TomlValue::Integer(i) => discoveries.insert(lookup, Value::from(i)),
            TomlValue::Float(f) => discoveries.insert(lookup, Value::from(f)),
            TomlValue::Boolean(b) => discoveries.insert(lookup, Value::from(b)),
            TomlValue::Datetime(dt) => {
                let dt = dt.to_string();
                discoveries.insert(lookup, Value::from(dt))
            }
            TomlValue::Array(vals) => {
                for (i, val) in vals.into_iter().enumerate() {
                    let key = format!("{}[{}]", lookup, i);
                    Self::from_toml_table_recursive_step(
                        LookupBuf::try_from(key)?,
                        val,
                        discoveries,
                    )?;
                }
                None
            }
            TomlValue::Table(map) => {
                for (table_key, value) in map {
                    let key = format!("{}.{}", lookup, table_key);
                    Self::from_toml_table_recursive_step(
                        LookupBuf::try_from(key)?,
                        value,
                        discoveries,
                    )?;
                }
                None
            }
        };
        Ok(())
    }

    /// Raise any errors that might stem from the lookup being invalid.
    #[instrument(level = "trace")]
    pub fn is_valid(&self) -> Result<(), LookupError> {
        Ok(())
    }

    #[instrument(level = "trace")]
    pub fn clone_lookup(&self) -> Lookup {
        Lookup::from(self)
    }

    #[instrument(level = "trace")]
    pub fn from_str(value: &str) -> Result<LookupBuf, LookupError> {
        Lookup::from_str(value).map(|l| l.into_buf())
    }

    /// Return a borrow of the SegmentBuf set.
    #[instrument(level = "trace")]
    pub fn as_segments(&self) -> &VecDeque<SegmentBuf> {
        &self.segments
    }

    /// Return the SegmentBuf set.
    #[instrument(level = "trace")]
    pub fn into_segments(self) -> VecDeque<SegmentBuf> {
        self.segments
    }

    /// Merge a lookup.
    #[instrument(level = "trace")]
    pub fn extend(&mut self, other: Self) {
        self.segments.extend(other.segments)
    }

    /// Returns `true` if `needle` is a prefix of the lookup.
    #[instrument(level = "trace")]
    pub fn starts_with(&self, needle: &LookupBuf) -> bool {
        needle.iter().zip(&self.segments).all(|(n, s)| n == s)
    }
}

impl FromStr for LookupBuf {
    type Err = LookupError;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        let lookup = Lookup::from_str(input)?;
        let lookup_buf: LookupBuf = lookup.into();
        Ok(lookup_buf)
    }
}

impl IntoIterator for LookupBuf {
    type Item = SegmentBuf;
    type IntoIter = std::collections::vec_deque::IntoIter<Self::Item>;

    fn into_iter(self) -> Self::IntoIter {
        self.segments.into_iter()
    }
}

impl From<String> for LookupBuf {
    fn from(input: String) -> Self {
        let mut segments = VecDeque::with_capacity(1);
        segments.push_back(SegmentBuf::from(input));
        LookupBuf { segments }
        // We know this must be at least one segment.
    }
}

impl From<SegmentBuf> for LookupBuf {
    fn from(input: SegmentBuf) -> Self {
        let mut segments = VecDeque::with_capacity(1);
        segments.push_back(input);
        LookupBuf { segments }
        // We know this must be at least one segment.
    }
}

impl From<usize> for LookupBuf {
    fn from(input: usize) -> Self {
        let mut segments = VecDeque::with_capacity(1);
        segments.push_back(SegmentBuf::index(input));
        LookupBuf { segments }
        // We know this must be at least one segment.
    }
}

impl From<&str> for LookupBuf {
    fn from(input: &str) -> Self {
        let mut segments = VecDeque::with_capacity(1);
        segments.push_back(SegmentBuf::from(input.to_owned()));
        LookupBuf { segments }
        // We know this must be at least one segment.
    }
}

impl Index<usize> for LookupBuf {
    type Output = SegmentBuf;

    fn index(&self, index: usize) -> &Self::Output {
        self.segments.index(index)
    }
}

impl IndexMut<usize> for LookupBuf {
    fn index_mut(&mut self, index: usize) -> &mut Self::Output {
        self.segments.index_mut(index)
    }
}

impl Serialize for LookupBuf {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&*ToString::to_string(self))
    }
}

impl<'de> Deserialize<'de> for LookupBuf {
    fn deserialize<D>(deserializer: D) -> Result<LookupBuf, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_string(LookupBufVisitor)
    }
}

struct LookupBufVisitor;

impl<'de> Visitor<'de> for LookupBufVisitor {
    type Value = LookupBuf;

    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        formatter.write_str("Expected valid Lookup path.")
    }

    fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        LookupBuf::from_str(value).map_err(de::Error::custom)
    }

    fn visit_string<E>(self, value: String) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        LookupBuf::from_str(&value).map_err(de::Error::custom)
    }
}

impl<'a> From<Lookup<'a>> for LookupBuf {
    fn from(v: Lookup<'a>) -> Self {
        let segments = v
            .segments
            .into_iter()
            .map(|f| f.as_segment_buf())
            .collect::<VecDeque<_>>();
        let retval: Result<LookupBuf, LookupError> = LookupBuf::try_from(segments);
        retval.expect(
            "A LookupBuf with 0 length was turned into a Lookup. Since a LookupBuf with 0 \
                  length is an invariant, any action on it is too.",
        )
    }
}

#[cfg(test)]
impl Arbitrary for LookupBuf {
    fn arbitrary<G: Gen>(g: &mut G) -> Self {
        LookupBuf {
            segments: {
                // Ensure we can't get an empty list.
                let len = u8::arbitrary(g) + 1;
                (0..len).map(|_| SegmentBuf::arbitrary(g)).collect()
            },
        }
    }

    fn shrink(&self) -> Box<dyn Iterator<Item = Self>> {
        Box::new(
            self.segments
                .shrink()
                .filter(|segments| segments.len() > 0)
                .map(|segments| LookupBuf { segments }),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_indexed_coalesce_from_string() {
        let parsed = LookupBuf::from_str("(a | b)[2]").unwrap();
        assert_eq!("(a | b)[2]", parsed.to_string());
    }
}
