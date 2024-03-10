use std::io::{self, ErrorKind};

use bytes::{Buf, BufMut, BytesMut};
use tokio_util::codec::{Decoder, Encoder};

#[derive(Clone, Debug, PartialEq)]
pub enum Message {
    KeepAlive,
    Choke,
    Unchoke,
    Interested,
    NotInterested,
    Have {
        piece_index: u32,
    },

    /// Bitfield is only ever sent as the first message,
    /// right after handshake.
    Bitfield(Vec<u8>),
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
    Cancel {
        index: u32,
        begin: u32,
        length: u32,
    },
    // TODO: PORT for DHT trackers.
}

#[repr(u8)]
enum MessageType {
    Choke,
    Unchoke,
    Interested,
    NotInterested,
    Have,
    Bitfield,
    Request,
    Piece,
    Cancel,
}

impl TryFrom<u8> for MessageType {
    type Error = io::Error;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(MessageType::Choke),
            1 => Ok(MessageType::Unchoke),
            2 => Ok(MessageType::Interested),
            3 => Ok(MessageType::NotInterested),
            4 => Ok(MessageType::Have),
            5 => Ok(MessageType::Bitfield),
            6 => Ok(MessageType::Request),
            7 => Ok(MessageType::Piece),
            8 => Ok(MessageType::Cancel),
            _ => Err(io::Error::from(ErrorKind::InvalidData)),
        }
    }
}

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

        let msg_type = MessageType::try_from(src.get_u8())?;

        let msg = match msg_type {
            MessageType::Choke => Message::Choke,
            MessageType::Unchoke => Message::Unchoke,
            MessageType::Interested => Message::Interested,
            MessageType::NotInterested => Message::NotInterested,
            MessageType::Have => {
                let piece_index = src.get_u32();
                Message::Have { piece_index }
            }
            MessageType::Bitfield => {
                // msg_len is the total message length including the 1 byte
                // message type. That's why we allocate msg_len - 1.
                let mut bitfield = vec![0; msg_len - 1];
                src.copy_to_slice(&mut bitfield);
                Message::Bitfield(bitfield)
            }
            MessageType::Request => {
                let index = src.get_u32();
                let begin = src.get_u32();
                let length = src.get_u32();
                Message::Request {
                    index,
                    begin,
                    length,
                }
            }
            MessageType::Piece => {
                // msg_len includes message type, index and begin.
                // which will require 1 + 4 + 4 bytes.
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
            MessageType::Cancel => {
                let index = src.get_u32();
                let begin = src.get_u32();
                let length = src.get_u32();
                Message::Cancel {
                    index,
                    begin,
                    length,
                }
            }
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
            Message::Choke => {
                dst.put_u32(1);
                dst.put_u8(MessageType::Choke as u8);
            }
            Message::Unchoke => {
                dst.put_u32(1);
                dst.put_u8(MessageType::Unchoke as u8);
            }
            Message::Interested => {
                dst.put_u32(1);
                dst.put_u8(MessageType::Interested as u8);
            }
            Message::NotInterested => {
                dst.put_u32(1);
                dst.put_u8(MessageType::NotInterested as u8);
            }
            Message::Have { piece_index } => {
                // len(u32) + type(u8) + piece_index(u32)
                // 4 + 1 + 4 = 9
                dst.put_u32(5);
                dst.put_u8(MessageType::Have as u8);
                dst.put_u32(piece_index);
            }
            Message::Bitfield(bitfield) => {
                // len(u32) + type(u8) + bitfield(Vec<u8>)
                // 4 + 1 + bitfield.len()
                dst.reserve(bitfield.len() + 5);

                dst.put_u32(bitfield.len() as u32 + 1);
                dst.put_u8(MessageType::Bitfield as u8);
                dst.put(&bitfield[..]);
            }
            Message::Request {
                index,
                begin,
                length,
            } => {
                // len(u32) + type(u8) + index(u32) + begin(u32) + length(u32)
                // 4 + 1 + 4 + 4 + 4 = 17
                dst.reserve(17);

                dst.put_u32(13);
                dst.put_u8(MessageType::Request as u8);
                dst.put_u32(index);
                dst.put_u32(begin);
                dst.put_u32(length);
            }
            Message::Piece {
                index,
                begin,
                block,
            } => {
                // len(u32) + type(u8) + index(u32) + begin(u32) + block(Vec<u8>)
                // 4 + 1 + 4 + 4 + block.len()
                dst.reserve(13 + block.len());

                // 4 bytes dedicated to msg_len itself is not included (13 - 4).
                dst.put_u32(9 + block.len() as u32);
                dst.put_u8(MessageType::Piece as u8);
                dst.put_u32(index);
                dst.put_u32(begin);
                dst.put(&block[..]);
            }
            Message::Cancel {
                index,
                begin,
                length,
            } => {
                // len(u32) + type(u8) + index(u32) + begin(u32) + length(u32)
                // 4 + 1 + 4 + 4 + 4 = 17
                dst.reserve(17);

                dst.put_u32(13);
                dst.put_u8(MessageType::Cancel as u8);
                dst.put_u32(index);
                dst.put_u32(begin);
                dst.put_u32(length);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use bytes::BytesMut;
    use tokio_util::codec::{Decoder, Encoder};

    use super::{Message, MessageCodec};

    macro_rules! codec_tests {
        ($($name:ident: $msg_sent:expr);+) => {
            $(

                #[test]
                fn $name() {
                    let mut codec = MessageCodec;

                    let mut buf = BytesMut::new();
                    codec.encode($msg_sent.clone(), &mut buf).unwrap();

                    let msg_received = codec.decode(&mut buf).unwrap().unwrap();
                    assert_eq!($msg_sent, msg_received);
                }
            )+
        };
    }

    codec_tests!(
        codec_choke: Message::Choke;
        codec_unchoke: Message::Unchoke;
        codev_interested: Message::Interested;
        codec_not_interested: Message::NotInterested;
        codec_have: Message::Have { piece_index: 2 };
        codec_bit_field: Message::Bitfield(vec![1, 0, 1, 0, 1, 0, 0, 0]);
        codec_request: Message::Request { index: 3, begin: 16384, length: 32768 };
        codec_piece: Message::Piece { index: 3, begin: 16384, block: vec![1, 1, 1, 1, 0, 0, 0, 0] };
        codec_cancel: Message::Cancel { index: 3, begin: 16384, length: 100_000 }
    );
}
