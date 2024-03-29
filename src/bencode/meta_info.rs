use std::ops::Deref;

use anyhow::Result;
use serde::{
    de::{Error, Visitor},
    Deserialize, Serialize,
};
use sha1::{Digest, Sha1};

#[derive(Serialize, Deserialize, Debug, PartialEq)]
pub struct Torrent {
    pub announce: String,
    pub info: Info,
}

#[derive(Serialize, Deserialize, Debug, PartialEq)]
pub struct Info {
    pub name: String,

    #[serde(rename = "piece length")]
    pub piece_length: u32,

    pub pieces: Pieces,

    #[serde(flatten)]
    pub download_type: DownloadType,
}

impl Info {
    /// Calculate the hash of torrent info.
    pub fn hash(&self) -> [u8; 20] {
        let info_bytes = serde_bencode::to_bytes(self).expect("re-encoding info must succeed");
        let mut hasher = Sha1::new();
        hasher.update(&info_bytes);
        hasher.finalize().into()
    }

    pub fn get_piece_length(&self, piece_index: u32) -> u32 {
        let length = self.download_type.len();

        // The reason we're casting to u64 is that file length could be
        // bigger than u32 but piece length will be a u32. So we have to
        // perform our calculations with u64 here.
        let begin = piece_index as u64 * self.piece_length as u64;
        if begin + self.piece_length as u64 > length {
            return (length - begin).try_into().expect("piece_size is u32");
        }
        self.piece_length as u32
    }

    pub fn num_pieces(&self) -> u32 {
        let file_len = self.download_type.len();
        let piece_len = self.piece_length as u64;
        let num = (file_len + piece_len - 1) / piece_len;
        num as u32
    }

    pub fn total_length(&self) -> u64 {
        self.download_type.len()
    }
}

#[derive(Default, Debug, PartialEq)]
pub struct Pieces(pub Vec<[u8; 20]>);

impl Deref for Pieces {
    type Target = Vec<[u8; 20]>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

#[derive(Serialize, Deserialize, Debug, PartialEq)]
#[serde(untagged)]
pub enum DownloadType {
    SingleFile { length: u64 },
    MultiFile { files: Vec<File> },
}

impl DownloadType {
    pub fn len(&self) -> u64 {
        match self {
            DownloadType::SingleFile { length } => *length,
            DownloadType::MultiFile { files } => files.iter().fold(0, |len, f| len + f.length),
        }
    }
}

#[derive(Serialize, Deserialize, Debug, PartialEq)]
pub struct File {
    pub length: u64,
    pub path: Vec<String>,
}

// NOTE: Another alternative approach is to have a newtype struct which wraps Vec<u8>
// and implement iterator trait so that it return arrays of 20 bytes.
impl Serialize for Pieces {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let hashes = self.0.concat();
        serializer.serialize_bytes(&hashes)
    }
}

impl<'de> Deserialize<'de> for Pieces {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct PiecesVisitor;

        impl<'de> Visitor<'de> for PiecesVisitor {
            type Value = Pieces;

            fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
                write!(formatter, "byte array whose length is a multiple of 20")
            }

            fn visit_bytes<E>(self, v: &[u8]) -> Result<Self::Value, E>
            where
                E: Error,
            {
                if v.len() % 20 != 0 {
                    return Err(E::custom(format!(
                        "expecting length to be a multiple of 20 but is {}",
                        v.len()
                    )));
                }

                let hashes = v
                    .chunks_exact(20)
                    .map(|hash| hash.try_into().expect("expecting a slice with 20 bytes"))
                    .collect();

                Ok(Pieces(hashes))
            }
        }
        deserializer.deserialize_bytes(PiecesVisitor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_bencode;
    use std::{fs, path::Path};

    #[test]
    fn single_file_download_type() {
        let path = Path::new("fixtures/single.torrent");
        let torrent = fs::read(path).unwrap();
        let meta: Torrent = serde_bencode::from_bytes(&torrent).unwrap();

        let hashed = [
            92, 197, 230, 82, 190, 13, 230, 242, 120, 5, 179, 4, 100, 255, 155, 0, 244, 137, 240,
            201,
        ];
        let info = Info {
            name: "sample.txt".to_string(),
            piece_length: 65536,
            pieces: Pieces(vec![hashed]),
            download_type: DownloadType::SingleFile { length: 20 },
            hash: None,
        };
        let expected = Torrent {
            announce: "udp://tracker.openbittorrent.com:80".to_string(),
            info,
        };
        assert_eq!(meta, expected);
    }

    #[test]
    fn multi_file_download_type() {
        let hashed = [
            92, 197, 230, 82, 190, 13, 230, 242, 120, 5, 179, 4, 100, 255, 155, 0, 244, 137, 240,
            201,
        ];
        let info = Info {
            name: "secret_vid.mov".to_string(),
            piece_length: 256,
            pieces: Pieces(vec![hashed]),
            download_type: DownloadType::MultiFile {
                files: vec![
                    File {
                        length: 20,
                        path: vec!["dir".to_string(), "filename1".to_string()],
                    },
                    File {
                        length: 100,
                        path: vec!["dir".to_string(), "filename2".to_string()],
                    },
                ],
            },
            hash: None,
        };

        let b = serde_bencode::to_bytes(&info).unwrap();
        let result = serde_bencode::from_bytes(&b).unwrap();
        assert_eq!(info, result);
    }
}
