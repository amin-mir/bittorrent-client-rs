use std::io;

use bytes::{Buf, BufMut, BytesMut};
use tokio_util::codec::{Decoder, Encoder};

pub struct Handshake {
    pub length: u8,
    pub protocol: [u8; 19],
    pub reserved: [u8; 8],
    pub info_hash: [u8; 20],
    pub peer_id: [u8; 20],
}

impl Default for Handshake {
    fn default() -> Self {
        Self {
            length: 0,
            protocol: [0; 19],
            reserved: [0; 8],
            info_hash: [0; 20],
            peer_id: [0; 20],
        }
    }
}

impl Handshake {
    pub fn new(info_hash: [u8; 20], peer_id: [u8; 20]) -> Handshake {
        Handshake {
            length: 19,
            protocol: *b"BitTorrent protocol",
            reserved: [0; 8],
            info_hash,
            peer_id,
        }
    }
}

pub struct HandshakeCodec;

const HANDSHAKE_SIZE: usize = 68;

impl Decoder for HandshakeCodec {
    type Item = Handshake;
    type Error = io::Error;

    fn decode(&mut self, buf: &mut BytesMut) -> io::Result<Option<Handshake>> {
        if buf.len() < HANDSHAKE_SIZE {
            return Ok(None);
        }

        let mut handshake = Handshake::default();
        handshake.length = buf.get_u8();
        buf.copy_to_slice(&mut handshake.protocol);
        buf.copy_to_slice(&mut handshake.reserved);
        buf.copy_to_slice(&mut handshake.info_hash);
        buf.copy_to_slice(&mut handshake.peer_id);

        Ok(Some(handshake))
    }
}

impl Encoder<Handshake> for HandshakeCodec {
    type Error = io::Error;

    fn encode(&mut self, item: Handshake, dst: &mut BytesMut) -> io::Result<()> {
        dst.reserve(HANDSHAKE_SIZE);
        dst.put_u8(item.length);
        dst.put(&item.protocol[..]);
        dst.put_bytes(0, 8);
        dst.put(&item.info_hash[..]);
        dst.put(&item.peer_id[..]);
        Ok(())
    }
}
