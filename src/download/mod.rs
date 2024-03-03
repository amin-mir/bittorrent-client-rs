use futures::sink::SinkExt;
use futures::stream::StreamExt;
use sha1::{Digest, Sha1};
use tokio::net::TcpStream;
use tokio::time::{self, Duration};
use tokio_util::codec::Framed;

use crate::bencode::Info;
use crate::tracker::Peers;

mod handshake;
pub use handshake::{Handshake, HandshakeCodec};

mod peer_msg;
pub use peer_msg::{Message, MessageCodec};

const BLOCK_SIZE: u32 = 1 << 14;

pub struct File {
    pub id: String,
    pub torrent_info: Info,
    pub peers: Peers,
}

impl File {
    pub async fn download(&mut self) -> anyhow::Result<Vec<u8>> {
        let stream = TcpStream::connect((self.peers[0].ip, self.peers[0].port)).await?;
        let mut handshakes = Framed::new(stream, HandshakeCodec);

        let peer_handshake = self.handshake(&mut handshakes).await?;
        println!("Peer ID: {}", hex::encode(peer_handshake.peer_id));

        let mut message_stream = handshakes.map_codec(|_c| MessageCodec);

        let bitfield = self.get_bitfield(&mut message_stream).await?;
        bitfield.iter().for_each(|b| println!("bitfield: {:b}", b));

        message_stream.send(Message::Interested).await?;

        let unchoke = message_stream.next().await.expect("receive unchoke")?;
        let Message::Unchoke = unchoke else {
            anyhow::bail!("expected unchoke, received {:?}", unchoke);
        };

        let mut file_bytes = Vec::with_capacity(self.torrent_info.total_length() as usize);
        for i in 0..self.torrent_info.num_pieces() {
            let mut piece = Piece {
                message_stream: &mut message_stream,
                piece_hash: self.torrent_info.pieces[i as usize],
                piece_length: self.torrent_info.get_piece_length(i),
                piece_index: i,
            };
            let mut piece_bytes = piece.download().await?;
            file_bytes.append(&mut piece_bytes);
        }
        Ok(file_bytes)
    }

    async fn handshake(
        &mut self,
        handshake_stream: &mut Framed<TcpStream, HandshakeCodec>,
    ) -> anyhow::Result<Handshake> {
        let peer_id_bytes = self.id.as_bytes().try_into()?;
        let handshake = Handshake::new(self.torrent_info.hash(), peer_id_bytes);

        handshake_stream.send(handshake).await?;
        let peer_handshake = handshake_stream.next().await.expect("receive handshake")?;
        Ok(peer_handshake)
    }

    async fn get_bitfield(
        &self,
        message_stream: &mut Framed<TcpStream, MessageCodec>,
    ) -> anyhow::Result<Vec<u8>> {
        let bitfield = message_stream.next().await.expect("receive bitfield")?;

        let Message::BitField(bitfield) = bitfield else {
            anyhow::bail!("expected bitfield, received {:?}", bitfield);
        };

        Ok(bitfield)
    }
}

struct Piece<'a> {
    message_stream: &'a mut Framed<TcpStream, MessageCodec>,
    piece_hash: [u8; 20],
    piece_length: u32,
    piece_index: u32,
}

impl Piece<'_> {
    pub async fn download(&mut self) -> anyhow::Result<Vec<u8>> {
        let mut piece_bytes = Vec::with_capacity(self.piece_length as usize);
        let mut downloaded = 0;
        while downloaded < self.piece_length {
            let mut block_len = self.piece_length - downloaded;
            if block_len > BLOCK_SIZE {
                block_len = BLOCK_SIZE;
            }

            let req = Message::Request {
                index: self.piece_index,
                begin: downloaded,
                length: block_len,
            };
            self.message_stream.send(req).await?;

            let piece = loop {
                tokio::select! {
                    Some(piece) = self.message_stream.next() => {
                        break piece?;
                    }
                    _ = time::sleep(Duration::from_secs(1)) => {
                        println!("timeout");
                    }
                }
            };

            let Message::Piece {
                index,
                begin,
                mut block,
            } = piece
            else {
                anyhow::bail!("expected piece, received {:?}", piece);
            };

            anyhow::ensure!(index == self.piece_index, "piece index should match");
            anyhow::ensure!(begin == downloaded, "block index should match");
            piece_bytes.append(&mut block);

            downloaded += block_len;
        }

        if !validate_piece(&piece_bytes, self.piece_hash) {
            anyhow::bail!("piece validation failed");
        }

        Ok(piece_bytes)
    }
}

fn validate_piece(piece: &[u8], hash: [u8; 20]) -> bool {
    let mut hasher = Sha1::new();
    hasher.update(piece);
    let piece_hash: [u8; 20] = hasher.finalize().into();
    hash == piece_hash
}
