use std::{
    collections::BTreeMap,
    str::{self, FromStr},
};

use nom::{
    branch::alt,
    bytes::complete::take,
    character::complete::{char, digit1, i64 as parse_i64},
    combinator::{map, map_res},
    multi::{fold_many0, many0},
    sequence::{delimited, pair, terminated},
    IResult,
};

use super::Value;

pub fn decode(input: &[u8]) -> IResult<&[u8], Value> {
    alt((
        map(integer, Value::Int),
        map(string, Value::String),
        map(list, Value::List),
        map(dict, Value::Map),
    ))(input)
}

fn string(input: &[u8]) -> IResult<&[u8], String> {
    fn parse_len_str(input: &[u8]) -> IResult<&[u8], &str> {
        terminated(map_res(digit1, str::from_utf8), char(':'))(input)
    }

    fn parse_len_u64(input: &[u8]) -> IResult<&[u8], u64> {
        map_res(parse_len_str, FromStr::from_str)(input)
    }

    let (input, len) = parse_len_u64(input)?;
    map(take(len), |s| String::from_utf8_lossy(s).into_owned())(input)
}

fn integer(input: &[u8]) -> IResult<&[u8], i64> {
    // TODO: wrap parse_i64 in cut.
    delimited(char('i'), parse_i64, char('e'))(input)
}

fn list(input: &[u8]) -> IResult<&[u8], Vec<Value>> {
    delimited(char('l'), many0(decode), char('e'))(input)
}

fn dict(input: &[u8]) -> IResult<&[u8], BTreeMap<String, Value>> {
    // TODO: wrap fold_many0 in cut.
    // theory: addig wrapping fold with cut is not enough.
    // I need to wrap pair with cut. fold_many will just stop when encoutering errors.
    delimited(
        char('d'),
        fold_many0(pair(string, decode), BTreeMap::new, |mut acc, kv| {
            acc.insert(kv.0, kv.1);
            acc
        }),
        char('e'),
    )(input)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_errors_parse_string() {
        let bencoded = b"5hi:hellounparsed";
        assert!(decode(bencoded).is_err());
    }

    #[test]
    fn decode_parses_string() {
        let bencoded = b"5:hellounparsed";

        let (remaining, hello) = decode(bencoded).unwrap();
        assert_eq!(hello, Value::String("hello".to_string()));
        assert_eq!("unparsed", str::from_utf8(remaining).unwrap());
    }

    #[test]
    fn decode_parses_integer() {
        let bencoded = b"i-326eunparsed";

        let (remaining, num) = decode(bencoded).unwrap();
        assert_eq!(num, Value::Int(-326));
        assert_eq!("unparsed", str::from_utf8(remaining).unwrap());
    }

    #[test]
    fn decode_errors_parse_integer() {
        let bencoded = b"i3a3eunparsed";
        assert!(decode(bencoded).is_err());
    }

    #[test]
    fn decode_parses_list() {
        let bencoded = b"l5:helloi52eeunparsed";

        let (remaining, list) = decode(bencoded).unwrap();
        assert_eq!(str::from_utf8(remaining).unwrap(), "unparsed");
        assert_eq!(
            list,
            Value::List(vec![Value::String("hello".to_string()), Value::Int(52)])
        );
    }

    #[test]
    fn decode_list_error_invalid_integer_elem() {
        let bencoded = b"l5:helloi5h2ee";
        assert!(decode(bencoded).is_err());
    }

    #[test]
    fn decode_parses_dictionary() {
        let bencoded = b"d5:applei-23e3:foo3:bar5:helloi52eeunparsed";
        let (remaining, dict) = decode(bencoded).unwrap();
        assert_eq!(str::from_utf8(remaining).unwrap(), "unparsed");

        assert_eq!(
            dict,
            Value::Map(BTreeMap::from([
                ("apple".to_string(), Value::Int(-23)),
                ("foo".to_string(), Value::String("bar".to_string())),
                ("hello".to_string(), Value::Int(52)),
            ]))
        );
    }

    #[test]
    fn decode_dictionary_error_incomplete_key_val_pair() {
        let bencoded = b"d5:applee";
        assert!(decode(bencoded).is_err());
    }
}
