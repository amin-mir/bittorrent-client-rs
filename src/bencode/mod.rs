use std::collections::BTreeMap;
use std::fmt;

use anyhow::{bail, Result};
use clap::ValueEnum;

mod meta_info;
pub use meta_info::*;
mod nom;
mod plain;

#[derive(Debug, PartialEq)]
pub enum Value {
    Int(i64),
    String(String),
    List(Vec<Value>),
    Map(BTreeMap<String, Value>),
}

#[derive(ValueEnum, Copy, Clone, Debug)]
pub enum Decoder {
    Nom,
    Plain,
}

impl fmt::Display for Decoder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.to_possible_value()
            .expect("no values are skipped")
            .get_name()
            .fmt(f)
    }
}

impl Decoder {
    pub fn decode<'a>(&self, input: &'a [u8]) -> Result<(&'a [u8], Value)> {
        match &self {
            Decoder::Nom => match nom::decode(input) {
                Ok(res) => Ok(res),
                Err(err) => bail!("decode error using nom {err}"),
            },
            Decoder::Plain => plain::decode(input),
        }
    }
}
