use std::io;

use bytes::{Buf, BufMut, BytesMut};
use tokio_util::codec::{Decoder, Encoder};

#[derive(Debug)]
pub enum Message {
    KeepAlive,
    Unchoke,
    Interested,
    BitField(Vec<u8>),
    Request {
        index: u32,
        begin: u32,
        length: u32,
    },
    Piece {
        index: u32,
        begin: u32,
        block: Vec<u8>,
    },
}

const UNCHOKE: u8 = 1;
const INTERESTED: u8 = 2;
const BIT_FIELD: u8 = 5;
const REQUEST: u8 = 6;
const PIECE: u8 = 7;

pub struct MessageCodec;

impl Decoder for MessageCodec {
    type Item = Message;
    type Error = io::Error;

    fn decode(&mut self, src: &mut BytesMut) -> io::Result<Option<Self::Item>> {
        if src.len() < 4 {
            return Ok(None);
        }

        // Not using get_u32 on src in order to avoid advancing the internal cursor.
        let mut len_bytes = [0; 4];
        len_bytes.copy_from_slice(&src[..4]);
        let msg_len = u32::from_be_bytes(len_bytes) as usize;

        if src.len() >= 4 + msg_len {
            // We've already read the len_bytes.
            src.advance(4);

            if msg_len == 0 {
                return Ok(Some(Message::KeepAlive));
            }
        } else {
            // We don't have the full message yet so reserve more space in the buffer.
            // This is not strictly necessary, but is a good idea performance-wise.
            src.reserve(4 + msg_len - src.len());
            return Ok(None);
        }

        let msg_type = src.get_u8();

        let msg = match msg_type {
            UNCHOKE => Message::Unchoke,
            INTERESTED => Message::Interested,
            BIT_FIELD => {
                let mut bitfield = vec![0; msg_len - 1];
                src.copy_to_slice(&mut bitfield);
                Message::BitField(bitfield)
            }
            REQUEST => {
                let index = src.get_u32();
                let begin = src.get_u32();
                let length = src.get_u32();
                Message::Request {
                    index,
                    begin,
                    length,
                }
            }
            PIECE => {
                let mut block = vec![0; msg_len - 9];
                let index = src.get_u32();
                let begin = src.get_u32();
                src.copy_to_slice(&mut block[..]);
                Message::Piece {
                    index,
                    begin,
                    block,
                }
            }
            _ => todo!(),
        };

        Ok(Some(msg))
    }
}

impl Encoder<Message> for MessageCodec {
    type Error = io::Error;

    fn encode(&mut self, item: Message, dst: &mut BytesMut) -> io::Result<()> {
        match item {
            Message::KeepAlive => {
                dst.put_u32(0);
            }
            Message::Unchoke => {
                dst.put_u32(1);
                dst.put_u8(UNCHOKE);
            }
            Message::Interested => {
                dst.put_u32(1);
                dst.put_u8(INTERESTED);
            }
            Message::BitField(bitfield) => {
                // len(u32) + type(u8) + bitfield(Vec<u8>)
                dst.reserve(bitfield.len() + 5);
                dst.put_u32(bitfield.len() as u32 + 1);
                dst.put_u8(BIT_FIELD);
                dst.put(&bitfield[..]);
            }
            Message::Request {
                index,
                begin,
                length,
            } => {
                // len(u32) + type(u8) + index(u64) + begin(u64) + length(u64)
                dst.reserve(29);
                dst.put_u32(25);
                dst.put_u8(REQUEST);
                dst.put_u32(index);
                dst.put_u32(begin);
                dst.put_u32(length);
            }
            Message::Piece {
                index,
                begin,
                block,
            } => {
                // len(u32) + type(u8) + index(u64) + begin(u64) + block(Vec<u8>)
                dst.reserve(21 + block.len());
                dst.put_u32(1 + block.len() as u32);
                dst.put_u8(PIECE);
                dst.put_u32(index);
                dst.put_u32(begin);
                dst.put(&block[..]);
            }
        }
        Ok(())
    }
}
