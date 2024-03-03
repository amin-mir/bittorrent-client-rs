use anyhow::{anyhow, bail, Result};
use std::collections::BTreeMap;
use std::str;

use super::Value;

// TODO:
// Watch video on serde.
// Investigate serde_bencode
// Implement torrent file decoding using serde_bencode and support all the spec.
pub fn decode(mut bytes: &[u8]) -> Result<(&[u8], Value)> {
    match bytes.get(0).ok_or(anyhow!("input is empty"))? {
        ch if ch.is_ascii_digit() => {
            let Some(colon_pos) = bytes.iter().position(|&b| b == b':') else {
                bail!("string size is missing ':'");
            };
            let len: usize = str::from_utf8(&bytes[..colon_pos])?.parse()?;

            bytes = &bytes[colon_pos + 1..];
            let str = String::from_utf8_lossy(&bytes[..len]);
            Ok((&bytes[len..], Value::String(str.to_string())))
        }
        b'i' => {
            let Some(end_pos) = bytes.iter().position(|&b| b == b'e') else {
                bail!("missing 'e' when parsing integer");
            };
            let num: i64 = str::from_utf8(&bytes[1..end_pos])?.parse()?;
            Ok((&bytes[end_pos + 1..], Value::Int(num)))
        }
        b'l' => {
            let mut list = Vec::new();
            let mut elem: Value;

            // Get rid of the 'l' at the beginning.
            bytes = &bytes[1..];
            loop {
                (bytes, elem) = decode(&bytes[0..])?;
                list.push(elem);
                if let Some(&b'e') = bytes.get(0) {
                    return Ok((&bytes[1..], Value::List(list)));
                }
            }
        }
        b'd' => {
            let mut map = BTreeMap::new();

            // Get rid of the 'd' at the beginning.
            bytes = &bytes[1..];
            loop {
                let (key, val);

                (bytes, key) = decode(bytes)?;
                let Value::String(key) = key else {
                    panic!("dictionary keys must be string");
                };

                (bytes, val) = decode(bytes)?;
                map.insert(key, val);

                if let Some(&b'e') = bytes.get(0) {
                    return Ok((&bytes[1..], Value::Map(map)));
                }
            }
        }
        _ => todo!(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_string() {
        let bencoded = b"5:hellounparsed";

        let (remaining, val) = decode(bencoded).unwrap();
        assert_eq!(str::from_utf8(remaining).unwrap(), "unparsed");
        assert_eq!(val, Value::String("hello".to_string()));
    }

    #[test]
    fn decode_int() {
        let bencoded = b"i-326eunparsed";

        let (remaining, val) = decode(bencoded).unwrap();
        assert_eq!(str::from_utf8(remaining).unwrap(), "unparsed");
        assert_eq!(val, Value::Int(-326));
    }

    #[test]
    fn decode_list() {
        let bencoded = b"l5:helloi52eeunparsed";

        let (remaining, val) = decode(bencoded).unwrap();
        assert_eq!(str::from_utf8(remaining).unwrap(), "unparsed");
        assert_eq!(
            val,
            Value::List(vec![Value::String("hello".to_string()), Value::Int(52)])
        );
    }

    #[test]
    fn decode_dictionary() {
        let bencoded = b"d5:applei-23e3:foo3:bar5:helloi52eeunparsed";
        let (remaining, val) = decode(bencoded).unwrap();
        assert_eq!(str::from_utf8(remaining).unwrap(), "unparsed");

        assert_eq!(
            val,
            Value::Map(BTreeMap::from([
                ("apple".to_string(), Value::Int(-23)),
                ("foo".to_string(), Value::String("bar".to_string())),
                ("hello".to_string(), Value::Int(52)),
            ]))
        );
    }
}
