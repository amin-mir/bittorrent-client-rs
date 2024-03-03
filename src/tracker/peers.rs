use serde::{de::Visitor, Deserialize, Serialize};
use std::{net::Ipv4Addr, ops::Deref};

pub struct Peers(Vec<Peer>);

#[derive(Serialize, Deserialize)]
pub struct Peer {
    #[serde(rename = "peer id")]
    pub id: Option<String>,
    pub ip: Ipv4Addr,
    pub port: u16,
}

impl Deref for Peers {
    type Target = Vec<Peer>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<'de> Deserialize<'de> for Peers {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct PeersVisitor;

        impl<'de> Visitor<'de> for PeersVisitor {
            type Value = Peers;

            fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
                write!(
                    formatter,
                    "byte array with a length that is a multiple of 6"
                )
            }

            // BENCHMARK
            // fn visit_bytes<E>(self, v: &[u8]) -> Result<Self::Value, E>
            // where
            //     E: serde::de::Error,
            // {
            //     if v.len() % 6 != 0 {
            //         return Err(E::invalid_length(v.len(), &self));
            //     }
            //
            //     let mut rdr = Cursor::new(v);
            //     let num_peers = v.len() / 6;
            //     let mut peers = Vec::with_capacity(num_peers);
            //
            //     for _ in 0..num_peers {
            //         let ip_bits = rdr
            //             .read_u32::<BigEndian>()
            //             .map_err(|e| E::custom(format!("{e}")))?;
            //         let ip = Ipv4Addr::from(ip_bits);
            //
            //         let port = rdr
            //             .read_u16::<BigEndian>()
            //             .map_err(|e| E::custom(format!("{e}")))?;
            //         peers.push(Peer { id: None, ip, port });
            //     }
            //
            //     Ok(Peers(peers))
            // }
            fn visit_bytes<E>(self, v: &[u8]) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                if v.len() % 6 != 0 {
                    return Err(E::invalid_length(v.len(), &self));
                }

                let num_peers = v.len() / 6;
                let mut peers = Vec::with_capacity(num_peers);

                for c in v.chunks_exact(6) {
                    let ip = Ipv4Addr::new(c[0], c[1], c[2], c[3]);
                    // BENCHMARK
                    // let port = (c[4] as u16) << 8 | c[5] as u16;
                    let port = u16::from_be_bytes([c[4], c[5]]);
                    peers.push(Peer { id: None, ip, port });
                }

                Ok(Peers(peers))
            }
        }

        deserializer.deserialize_bytes(PeersVisitor)
    }
}
