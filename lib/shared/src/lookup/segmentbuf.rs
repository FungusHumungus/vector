use crate::lookup::*;
#[cfg(test)]
use quickcheck::{Arbitrary, Gen};
use std::{
    collections::VecDeque,
    fmt::{Display, Formatter},
};
use tracing::instrument;

/// `SegmentBuf`s are chunks of a `LookupBuf`.
///
/// They represent either a field or an index. A sequence of `SegmentBuf`s can become a `LookupBuf`.
///
/// This is the owned, allocated side of a `Segement` for `LookupBuf.` It owns its fields unlike `Lookup`. Think of `String` to `&str` or `PathBuf` to `Path`.
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord, Clone, Hash)]
pub enum SegmentBuf {
    Field {
        name: String,
        // This is a very lazy optimization to avoid having to scan for escapes.
        requires_quoting: bool,
    },
    Index(usize),
    // Coalesces hold multiple segment sets.
    Coalesce(
        Vec<
            // Each of these can be it's own independent lookup.
            VecDeque<Self>,
        >,
    ),
}

impl SegmentBuf {
    pub const fn field(name: String, requires_quoting: bool) -> SegmentBuf {
        SegmentBuf::Field {
            name,
            requires_quoting,
        }
    }

    pub fn is_field(&self) -> bool {
        matches!(self, SegmentBuf::Field { name: _, requires_quoting: _ })
    }

    pub const fn index(v: usize) -> SegmentBuf {
        SegmentBuf::Index(v)
    }

    pub fn is_index(&self) -> bool {
        matches!(self, SegmentBuf::Index(_))
    }

    pub const fn coalesce(v: Vec<VecDeque<Self>>) -> SegmentBuf {
        SegmentBuf::Coalesce(v)
    }

    pub fn is_coalesce(&self) -> bool {
        matches!(self, SegmentBuf::Coalesce(_))
    }

    #[instrument]
    pub fn as_segment<'a>(&'a self) -> Segment<'a> {
        match self {
            SegmentBuf::Field {
                name,
                requires_quoting,
            } => Segment::field(name.as_str(), *requires_quoting),
            SegmentBuf::Index(i) => Segment::index(*i),
            SegmentBuf::Coalesce(v) => Segment::coalesce(
                v.iter()
                    .map(|inner| {
                        inner
                            .iter()
                            .map(|v| v.as_segment())
                            .collect::<VecDeque<_>>()
                    })
                    .collect(),
            ),
        }
    }
}

impl Display for SegmentBuf {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> Result<(), std::fmt::Error> {
        match self {
            SegmentBuf::Index(i) => write!(formatter, "{}", i),
            SegmentBuf::Field {
                name,
                requires_quoting: false,
            } => write!(formatter, "{}", name),
            SegmentBuf::Field {
                name,
                requires_quoting: true,
            } => write!(formatter, "\"{}\"", name),
            SegmentBuf::Coalesce(v) => write!(
                formatter,
                "({})",
                v.iter()
                    .map(|inner| inner
                        .iter()
                        .map(|v| v.to_string())
                        .collect::<Vec<_>>()
                        .join("."))
                    .collect::<Vec<_>>()
                    .join(" | ")
            ),
        }
    }
}

impl From<String> for SegmentBuf {
    fn from(mut name: String) -> Self {
        let requires_quoting = name.starts_with('\"');
        if requires_quoting {
            // There is unfortunately not way to make an owned substring of a string.
            // So we have to take a slice and clone it.
            let len = name.len();
            name = name[1..len - 1].to_string();
        }
        Self::Field {
            name,
            requires_quoting,
        }
    }
}

impl From<&str> for SegmentBuf {
    fn from(name: &str) -> Self {
        Self::from(name.to_string())
    }
}

impl From<usize> for SegmentBuf {
    fn from(value: usize) -> Self {
        Self::index(value)
    }
}

impl From<Vec<VecDeque<SegmentBuf>>> for SegmentBuf {
    fn from(value: Vec<VecDeque<SegmentBuf>>) -> Self {
        Self::coalesce(value)
    }
}

// While testing, it can be very convienent to use the `vec![]` macro.
// This would be slow in hot release code, so we don't allow it in non-test code.
#[cfg(test)]
impl From<Vec<Vec<SegmentBuf>>> for SegmentBuf {
    fn from(value: Vec<Vec<SegmentBuf>>) -> Self {
        Self::coalesce(value.into_iter().map(|v| v.into()).collect())
    }
}

impl<'a> From<Segment<'a>> for SegmentBuf {
    fn from(value: Segment<'a>) -> Self {
        value.as_segment_buf()
    }
}

#[cfg(test)]
fn arbitrary_field<G: Gen>(g: &mut G) -> (String, bool) {
    let chars = (65u8..90).chain(97..122).map(|c| c as char);

    match i32::arbitrary(g) % 2 {
        0 => {
            // Doesn't require quoting
            let chars = chars.collect::<Vec<_>>();
            let len = u8::arbitrary(g) % 5 + 1;
            (
                (0..len)
                    .map(|_| chars[usize::arbitrary(g) % chars.len()].clone())
                    .collect::<String>(),
                false,
            )
        }
        _ => {
            // Requires quoting (we can include extra characters).
            let chars = chars.chain(vec!['@', ' ']).collect::<Vec<_>>();
            let len = u8::arbitrary(g) % 5 + 1;
            (
                (0..len)
                    .map(|_| chars[usize::arbitrary(g) % chars.len()].clone())
                    .collect::<String>(),
                true,
            )
        }
    }
}

#[cfg(test)]
impl Arbitrary for SegmentBuf {
    fn arbitrary<G: Gen>(g: &mut G) -> Self {
        match u8::arbitrary(g) % 10 {
            (0..=7) => {
                let (name, requires_quoting) = arbitrary_field(g);
                SegmentBuf::Field {
                    name,
                    requires_quoting,
                }
            }
            8 => SegmentBuf::Coalesce({
                let len = u8::arbitrary(g) % 3 + 2;
                (0..len)
                    .map(|_| {
                        let len = u8::arbitrary(g) % 2 + 1;
                        (0..len).map(|_| SegmentBuf::arbitrary(g)).collect()
                    })
                    .collect()
            }),
            _ => {
                // We'll limit the index to 5, because you can rapidly use up a lot of memory
                // with lots of higher indexes.
                SegmentBuf::Index(usize::arbitrary(g) % 5)
            }
        }
    }
}
