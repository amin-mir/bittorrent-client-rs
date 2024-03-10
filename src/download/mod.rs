use std::net::SocketAddr;

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

mod bitfield;
use bitfield::Bitfield;

const BLOCK_SIZE: u32 = 1 << 14;

pub struct File {
    pub id: String,
    pub torrent_info: Info,
    pub peers: Peers,
    state: Option<PeerState>,
    bytes: Vec<u8>,
}

struct PeerState {
    /// Address of peer.
    addr: SocketAddr,
    /// Whether peer choked us or not.
    choked: bool,
    bitfield: Bitfield,
    /// If there's a piece download from this peer in progress,
    /// this field will contain its piece index.
    pending_piece_dl: Option<PieceDownload>,
}

struct PieceDownload {
    piece_hash: [u8; 20],
    piece_length: u32,
    piece_index: u32,
    block_begin: u32,
    block_len: u32,
    piece_bytes: Vec<u8>,
}

impl File {
    pub fn new(id: String, torrent_info: Info, peers: Peers) -> Self {
        let file_len = torrent_info.total_length() as usize;
        Self {
            id,
            torrent_info,
            peers,
            state: None,
            bytes: vec![0; file_len],
        }
    }

    pub async fn download(mut self) -> anyhow::Result<Vec<u8>> {
        println!(
            "num peers: {}, piece length: {}",
            self.peers.len(),
            self.torrent_info.piece_length
        );
        let addr: SocketAddr = (self.peers[0].ip, self.peers[0].port).into();
        let stream = TcpStream::connect(addr).await?;
        let mut handshakes = Framed::new(stream, HandshakeCodec);

        let peer_handshake = self.handshake(&mut handshakes).await?;
        println!("Peer ID: {}", hex::encode(peer_handshake.peer_id));

        let mut message_stream = handshakes.map_codec(|_c| MessageCodec);

        let bitfield;

        let msg = message_stream.next().await.expect("first peer msg");
        let total_pieces =
            (self.torrent_info.total_length() + self.torrent_info.piece_length as u64 - 1)
                / self.torrent_info.piece_length as u64;
        if let Ok(Message::Bitfield(bytes)) = msg {
            if !bitfield::is_seed(total_pieces as usize, &bytes) {
                // Skip this peer and connect to the next one.
            }

            bitfield = Bitfield::zero(total_pieces as u32);
            println!(
                "total torrent pieces: {}, bitfield={bitfield}",
                total_pieces
            );
            self.state = Some(PeerState {
                addr,
                choked: true,
                bitfield,
                pending_piece_dl: None,
            });
        } else {
            self.handle_msg(msg?, &mut message_stream).await?;
        }

        // TODO: choose whether to send interested or not based bitfield.
        message_stream.send(Message::Interested).await?;
        println!("interested sent");

        loop {
            if self
                .state
                .as_mut()
                .unwrap()
                .bitfield
                .seek_next_piece()
                .is_none()
            {
                break;
            }

            let msg = message_stream.next().await;

            let Some(msg) = msg else {
                anyhow::bail!("peer stream ended");
            };
            self.handle_msg(msg?, &mut message_stream).await?;
        }
        Ok(self.bytes)
    }

    async fn handle_msg(
        &mut self,
        msg: Message,
        message_stream: &mut Framed<TcpStream, MessageCodec>,
    ) -> anyhow::Result<()> {
        match msg {
            Message::KeepAlive => {
                println!("received keep alive");
                Ok(())
            }
            Message::Choke => {
                // Peer choked us. We should drop the pending piece request
                // and request for the same piece again when unchoked.
                let state = self.state.as_mut().unwrap();
                state.choked = true;
                _ = state.pending_piece_dl.take();
                // TODO: Inform piece picker that we need to download the piece again.
                // When we're choked in the middle of downloading a piece, we will
                // have to download it from the beginning when unchoke happens.
                // the downloaded bytes are thrown away.
                println!("peer {} choked us", state.addr);
                Ok(())
            }
            Message::Unchoke => {
                let state = self.state.as_mut().unwrap();
                if !state.choked {
                    println!("peer sent duplicate unchoke");
                    return Ok(());
                }
                state.choked = false;
                println!("peer {} unchoked us", state.addr);

                // Start by sending a request for the next piece.
                let piece_index = match state.bitfield.pick_piece() {
                    None => anyhow::bail!("no more pieces to download from peer"),
                    Some(idx) => idx,
                };
                println!("sending a request for piece_index={piece_index} after unchoke");

                let piece_length = self.torrent_info.get_piece_length(0);
                let mut block_len = BLOCK_SIZE;
                // Handling cases where the whole piece is smaller than block.
                if piece_length < block_len {
                    block_len = piece_length;
                }

                let req = Message::Request {
                    index: piece_index,
                    begin: 0,
                    length: block_len,
                };

                message_stream.send(req).await?;

                // TODO: re-use the already allocated Vec.
                state.pending_piece_dl = Some(PieceDownload {
                    piece_hash: self.torrent_info.pieces[piece_index as usize],
                    piece_length,
                    piece_index,
                    block_begin: 0,
                    block_len,
                    piece_bytes: Vec::with_capacity(piece_length as usize),
                });
                Ok(())
            }
            Message::Bitfield(_) => {
                anyhow::bail!("bitfield was sent out of order")
            }
            Message::Have { piece_index } => {
                println!("peer has piece with index: {piece_index}");
                self.state.as_mut().unwrap().bitfield.set(piece_index);
                Ok(())
            }
            Message::Request {
                index,
                begin,
                length,
            } => {
                todo!();
            }
            Message::Interested => {
                println!("peer interested");
                Ok(())
            }
            Message::NotInterested => {
                println!("peer not interested");
                Ok(())
            }
            Message::Piece {
                index,
                begin,
                mut block,
            } => {
                println!("received piece block");
                let state = self.state.as_mut().unwrap();
                let dl = state.pending_piece_dl.as_mut().unwrap();

                anyhow::ensure!(index == dl.piece_index, "piece index should match");
                anyhow::ensure!(begin == dl.block_begin, "block index should match");
                anyhow::ensure!(dl.block_len == block.len() as u32);
                dl.piece_bytes.append(&mut block);

                // Request the next block from the peer.
                if dl.piece_bytes.len() as u32 == dl.piece_length {
                    // If we received all blocks for a piece,
                    // we write the piece to file bytes.
                    if !validate_piece(&dl.piece_bytes, dl.piece_hash) {
                        anyhow::bail!("piece validation failed");
                    }

                    let begin_idx: usize =
                        dl.piece_index as usize * self.torrent_info.piece_length as usize;
                    let end_idx: usize = begin_idx + dl.piece_length as usize;
                    self.bytes
                        .splice(begin_idx..end_idx, dl.piece_bytes.drain(..));

                    // Request block from next piece. If bitfield return None,
                    // it means that we have exhausted this peer.
                    println!("requesting block from next piece: {}", dl.piece_index);
                    let Some(piece_index) = state.bitfield.pick_piece() else {
                        anyhow::bail!("no more pieces to download from peer");
                    };

                    let mut block_len = dl.piece_length;
                    if block_len > BLOCK_SIZE {
                        block_len = BLOCK_SIZE;
                    }

                    let req = Message::Request {
                        index: piece_index,
                        begin: 0,
                        length: block_len,
                    };

                    message_stream.send(req).await?;

                    // Get the index for the next piece. dl.piece_index contains
                    // the index for the previous piece.
                    let piece_length = self.torrent_info.get_piece_length(piece_index);
                    dl.piece_hash = self.torrent_info.pieces[piece_index as usize];
                    dl.piece_length = piece_length;
                    dl.piece_index = piece_index;
                    dl.block_begin = 0;
                    dl.block_len = block_len;
                    dl.piece_bytes.clear();
                } else {
                    // Request block from same piece.
                    let downloaded = dl.piece_bytes.len() as u32;
                    println!(
                        "requesting next block from same piece: {}, begin: {}",
                        dl.piece_index, downloaded
                    );
                    let mut block_len = dl.piece_length - downloaded;
                    if block_len > BLOCK_SIZE {
                        block_len = BLOCK_SIZE;
                    }

                    let req = Message::Request {
                        index: dl.piece_index,
                        begin: downloaded,
                        length: block_len,
                    };

                    message_stream.send(req).await?;

                    dl.block_len = block_len;
                    dl.block_begin = downloaded;
                }
                Ok(())
            }
            Message::Cancel {
                index,
                begin,
                length,
            } => {
                todo!();
            }
        }
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
}

fn validate_piece(piece: &[u8], hash: [u8; 20]) -> bool {
    let mut hasher = Sha1::new();
    hasher.update(piece);
    let piece_hash: [u8; 20] = hasher.finalize().into();
    hash == piece_hash
}
