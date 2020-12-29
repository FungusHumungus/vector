use crate::lookup::*;
use std::{fs, io::Read, path::Path};
use tracing::trace;

const SUFFICIENTLY_COMPLEX: &str =
    r#"regular."quoted"."quoted but spaces"."quoted.but.periods".lookup[0].nested_lookup[0][0]"#;
lazy_static::lazy_static! {
    static ref SUFFICIENTLY_DECOMPOSED: [SegmentBuf; 9] = [
        SegmentBuf::from(r#"regular"#.to_string()),
        SegmentBuf::from(r#""quoted""#.to_string()),
        SegmentBuf::from(r#""quoted but spaces""#.to_string()),
        SegmentBuf::from(r#""quoted.but.periods""#.to_string()),
        SegmentBuf::from(r#"lookup"#.to_string()),
        SegmentBuf::from(0),
        SegmentBuf::from(r#"nested_lookup"#.to_string()),
        SegmentBuf::from(0),
        SegmentBuf::from(0),
    ];
}

#[test_env_log::test]
fn zero_len_not_allowed() {
    let input = "";
    let maybe_lookup = LookupBuf::from_str(input);
    assert!(maybe_lookup.is_err());
}

#[test_env_log::test]
fn we_dont_parse_plain_strings_in_from() {
    let input = "some_key.still_the_same_key.this.is.going.in.via.from.and.should.not.get.parsed";
    let lookup = LookupBuf::from(input);
    assert_eq!(lookup[0], SegmentBuf::from(String::from(input)));
    assert_eq!(lookup.to_string(), input);
}

#[test_env_log::test]
fn simple() {
    let input = "some_key";
    let lookup = LookupBuf::from_str(input).unwrap();
    assert_eq!(lookup[0], SegmentBuf::from(String::from("some_key")));
    assert_eq!(lookup.to_string(), input);
}

#[test_env_log::test]
fn quoted() {
    let input = "\"start\".\"after\"";
    let lookup = LookupBuf::from_str(input).unwrap();
    assert_eq!(lookup[0], SegmentBuf::from(String::from("\"start\"")));
    assert_eq!(lookup[1], SegmentBuf::from(String::from("\"after\"")));
    assert_eq!(lookup.to_string(), input);
}

#[test_env_log::test]
fn coalesced() {
    let input = "plain.(option_one | option_two)";
    let lookup = LookupBuf::from_str(input).unwrap();
    assert_eq!(lookup[0], SegmentBuf::from("plain".to_string()));
    assert_eq!(
        lookup[1],
        SegmentBuf::from(vec![
            vec![SegmentBuf::from("option_one".to_string())],
            vec![SegmentBuf::from("option_two".to_string())],
        ])
    );
}

#[test_env_log::test]
fn coalesced_nesting() {
    let input = "plain.(option_one.inner | option_two.other_inner)";
    let lookup = LookupBuf::from_str(input).unwrap();
    assert_eq!(lookup[0], SegmentBuf::from("plain".to_string()));
    assert_eq!(
        lookup[1],
        SegmentBuf::from(vec![
            vec![
                SegmentBuf::from("option_one".to_string()),
                SegmentBuf::from("inner".to_string())
            ],
            vec![
                SegmentBuf::from("option_two".to_string()),
                SegmentBuf::from("other_inner".to_string())
            ],
        ]),
    );
}

#[test_env_log::test]
fn push() {
    let input = "some_key";
    let mut lookup = LookupBuf::from_str(input).unwrap();
    lookup.push_back(SegmentBuf::from(String::from(input)));
    assert_eq!(lookup[0], SegmentBuf::from(String::from("some_key")));
    assert_eq!(lookup[1], SegmentBuf::from(String::from("some_key")));
}

#[test_env_log::test]
fn pop() {
    let input = "some_key";
    let mut lookup = LookupBuf::from_str(input).unwrap();
    let out = lookup.pop_back();
    assert_eq!(out, Some(SegmentBuf::from(String::from("some_key"))));
}

#[test_env_log::test]
fn array() {
    let input = "foo[0]";
    let lookup = LookupBuf::from_str(input).unwrap();
    assert_eq!(lookup[0], SegmentBuf::from(String::from("foo")));
    assert_eq!(lookup[1], SegmentBuf::from(0));
    assert_eq!(lookup.to_string(), input);
}

#[test_env_log::test]
fn via_parse() {
    let input = "foo[0]";
    let lookup = input.parse::<LookupBuf>().unwrap();
    assert_eq!(lookup[0], SegmentBuf::from(String::from("foo")));
    assert_eq!(lookup[1], SegmentBuf::from(0));
    assert_eq!(lookup.to_string(), input);
}

#[test_env_log::test]
fn to_string() {
    let input = SUFFICIENTLY_COMPLEX;
    let lookup = LookupBuf::from_str(input).unwrap();
    assert_eq!(lookup.to_string(), input);
}

#[test_env_log::test]
fn impl_index_usize() {
    let lookup = LookupBuf::from_str(SUFFICIENTLY_COMPLEX).unwrap();

    for i in 0..SUFFICIENTLY_DECOMPOSED.len() {
        assert_eq!(lookup[i], SUFFICIENTLY_DECOMPOSED[i])
    }
}

#[test_env_log::test]
fn impl_index_mut_index_mut() {
    let mut lookup = LookupBuf::from_str(SUFFICIENTLY_COMPLEX).unwrap();

    for i in 0..SUFFICIENTLY_DECOMPOSED.len() {
        let x = &mut lookup[i]; // Make sure we force a mutable borrow!
        assert_eq!(*x, SUFFICIENTLY_DECOMPOSED[i])
    }
}

#[test_env_log::test]
fn iter() {
    let lookup = LookupBuf::from_str(SUFFICIENTLY_COMPLEX).unwrap();

    let mut iter = lookup.iter();
    for (index, expected) in SUFFICIENTLY_DECOMPOSED.iter().enumerate() {
        let parsed = iter
            .next()
            .unwrap_or_else(|| panic!("Expected at index {}: {:?}, got None.", index, expected));
        assert_eq!(expected, parsed, "Failed at {}", index);
    }
}

#[test_env_log::test]
fn into_iter() {
    let lookup = LookupBuf::from_str(SUFFICIENTLY_COMPLEX).unwrap();
    let mut iter = lookup.into_iter();
    for (index, expected) in SUFFICIENTLY_DECOMPOSED.iter().cloned().enumerate() {
        let parsed = iter
            .next()
            .unwrap_or_else(|| panic!("Expected at index {}: {:?}, got None.", index, expected));
        assert_eq!(expected, parsed, "Failed at {}", index);
    }
}

fn parse_artifact(path: impl AsRef<Path>) -> std::io::Result<String> {
    let mut test_file = match fs::File::open(path) {
        Ok(file) => file,
        Err(e) => return Err(e),
    };

    let mut buf = Vec::new();
    test_file.read_to_end(&mut buf)?;
    let string = String::from_utf8(buf).unwrap();
    Ok(string.trim_end().to_owned())
}

// This test iterates over the `tests/data/fixtures/lookup` folder and ensures the lookup parsed,
// then turned into a string again is the same.
#[test_env_log::test]
fn lookup_to_string_and_serialize() {
    const FIXTURE_ROOT: &str = "tests/fixtures/lookup";

    trace!(?FIXTURE_ROOT, "Opening.");
    std::fs::read_dir(FIXTURE_ROOT)
        .unwrap()
        .for_each(|fixture_file| match fixture_file {
            Ok(fixture_file) => {
                let path = fixture_file.path();
                tracing::trace!(?path, "Opening.");
                let buf = parse_artifact(&path).unwrap();
                let buf_serialized =
                    serde_json::to_string(&serde_json::to_value(&buf).unwrap()).unwrap();
                let lookup = LookupBuf::from_str(&buf).unwrap();
                tracing::trace!(?path, ?lookup, ?buf, "Asserting equal.");
                assert_eq!(lookup.to_string(), buf);
                // Ensure serialization doesn't clobber.
                let serialized = serde_json::to_string(&lookup.to_string()).unwrap();
                assert_eq!(serialized, buf_serialized);
                // Ensure deserializing doesn't clobber.
                let deserialized = serde_json::from_str(&serialized).unwrap();
                assert_eq!(lookup, deserialized);
            }
            _ => panic!("This test should never read Err'ing test fixtures."),
        });
}

#[quickcheck]
fn path_parses(path: LookupBuf) -> bool {
    let orig = path.to_string();
    let result = match LookupBuf::from_str(&orig) {
        Err(err) => {
            println!("Err: {:?}", err);
            false
        }
        Ok(path) => {
            let new = path.to_string();
            if orig == new {
                true
            } else {
                println!("Parsed: {:?}", path);
                println!("New: {}", new);
                false
            }
        }
    };

    if !result {
        println!("Failed: {}", orig);
    }

    result
}
