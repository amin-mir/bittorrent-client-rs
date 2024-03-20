use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;

use anyhow::Context;
use futures::sink::SinkExt;
use futures::stream::StreamExt;
use sha1::{Digest, Sha1};
use tokio::net::TcpStream;
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
const MAX_INFLIGHT: u8 = 10;

pub struct File {
    pub id: String,
    pub torrent_info: Info,
    pub peers: Peers,
    state: PeerState,
    // This bitfield doesn't belog in PeerState because it's for
    // the owned pieces. We don't need bitfield for peers because
    // we're only downloading from seeds.
    bitfield: Bitfield,
    /// This maps piece_index to PieceDownload, for all the pieces
    /// which are currently being downloaded from this peer.
    pending_pieces: HashMap<u32, PieceDownload>,
    /// This set contains all the outgoing block requests.
    /// The main purpose is validating that we get the blocks
    /// that we previously sent request for.
    pending_blocks: HashSet<BlockDownload>,
    bytes: Vec<u8>,
    // TODO: choked and unchoked peers could be implemented
    // with bitwise operations on a [u8; 5].
}

struct PeerState {
    addr: SocketAddr,
    choked: bool,
}

#[derive(Debug)]
struct PieceDownload {
    piece_hash: [u8; 20],
    piece_length: u32,
    requested: u32,
    remaining_blocks: u16,
    bytes: Vec<u8>,
}

#[derive(Debug, PartialEq, Eq, Hash)]
struct BlockDownload {
    piece_index: u32,
    block_begin: u32,
    block_len: u32,
}

impl File {
    pub fn new(id: String, torrent_info: Info, peers: Peers) -> Self {
        let state = PeerState {
            addr: (peers[0].ip, peers[0].port).into(),
            choked: true,
        };
        let bitfield = Bitfield::zero(torrent_info.num_pieces());
        let bytes = vec![0; torrent_info.total_length() as usize];
        Self {
            id,
            torrent_info,
            peers,
            state,
            bitfield,
            pending_pieces: HashMap::new(),
            pending_blocks: HashSet::new(),
            bytes,
        }
    }

    pub async fn download(mut self) -> anyhow::Result<Vec<u8>> {
        println!(
            "num peers: {}, total length: {}, total pieces: {}, piece length: {}",
            self.peers.len(),
            self.torrent_info.total_length(),
            self.torrent_info.num_pieces(),
            self.torrent_info.piece_length
        );
        println!("peer addr: {}", self.state.addr);
        println!("our bitfield: {}", self.bitfield);
        let stream = TcpStream::connect(self.state.addr).await?;
        let mut handshakes = Framed::new(stream, HandshakeCodec);

        let peer_handshake = self.handshake(&mut handshakes).await?;
        println!("Peer ID: {}", hex::encode(peer_handshake.peer_id));

        let mut message_stream = handshakes.map_codec(|_c| MessageCodec);
        let mut inflight: u8 = 0;

        let msg = message_stream.next().await.expect("first peer msg");
        let total_pieces = self.torrent_info.num_pieces() as usize;
        if let Ok(Message::Bitfield(bytes)) = msg {
            if !bitfield::is_seed(total_pieces, &bytes) {
                // Skip this peer and connect to the next one.
                println!("peer is not seed");
            }
            let peer_bitfield = Bitfield::from_vec(total_pieces, bytes);
            println!("peer bitfield: {peer_bitfield}");
        } else {
            println!("came here");
            self.handle_msg(&mut inflight, msg?, &mut message_stream)
                .await?;
        }

        // We're only downloading from seeds, so we always send Interested.
        message_stream.send(Message::Interested).await?;

        loop {
            println!("remaining pieces: {}", self.bitfield.remaining());
            if self.bitfield.remaining() == 0 {
                break;
            }

            let msg = message_stream.next().await;

            let Some(msg) = msg else {
                anyhow::bail!("peer stream ended");
            };
            self.handle_msg(&mut inflight, msg?, &mut message_stream)
                .await?;
        }
        Ok(self.bytes)
    }

    async fn handle_msg(
        &mut self,
        inflight: &mut u8,
        msg: Message,
        message_stream: &mut Framed<TcpStream, MessageCodec>,
    ) -> anyhow::Result<()> {
        match msg {
            Message::KeepAlive => {
                println!("received keep alive");
                Ok(())
            }
            Message::Choke => {
                // Peer choked us. We should drop the pending piece downloads
                // and request for the same pieces again when unchoked.
                //
                // The piece has to be downloaded from the beginning, because
                // we're assuming that pieces could arrive in an arbitrary order
                // and not in the order we request them from peer so determining
                // which block to download after unchoke is not trivial. We will
                // probably need a block bitmap per pieces to resume the download.
                //
                // We don't need to inform piece picker when this happens, because
                // it will yield this piece index again eventually if we don't
                // confirm its download completed.
                self.state.choked = true;
                for (_, dl) in self.pending_pieces.iter_mut() {
                    // By setting requested to 0, piece bytes will be overwritten.
                    // We prefer to do this to re-use the already allocated Vector.
                    dl.requested = 0;
                }
                self.pending_blocks.clear();

                println!("peer {} choked us", self.state.addr);
                Ok(())
            }
            Message::Unchoke => {
                if !self.state.choked {
                    println!("peer sent duplicate unchoke");
                    return Ok(());
                }
                self.state.choked = false;
                println!("peer {} unchoked us", self.state.addr);

                self.request_pieces(inflight, message_stream).await?;
                Ok(())
            }
            Message::Bitfield(_) => {
                anyhow::bail!("bitfield was sent out of order")
            }
            Message::Have { piece_index } => {
                // TODO: handle these messages by constructing a bitfield for
                // peer and updating it with these messages. Then check if peer
                // becomes seed, we start downloading from it.
                println!("peer has piece with index: {piece_index}");
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
                block,
            } => {
                let block_dl = BlockDownload {
                    piece_index: index,
                    block_begin: begin,
                    block_len: block.len() as u32,
                };
                anyhow::ensure!(
                    self.pending_blocks.remove(&block_dl),
                    "received unrequested block"
                );

                *inflight -= 1;

                let Some(piece_dl) = self.pending_pieces.get_mut(&index) else {
                    anyhow::bail!("received block for unrequested piece: {}", index);
                };
                piece_dl.remaining_blocks -= 1;

                println!(
                    "Received block [{}-{})/{} for piece ({}), remaining blocks: {}",
                    begin,
                    begin + block.len() as u32,
                    piece_dl.piece_length,
                    index,
                    piece_dl.remaining_blocks,
                );

                piece_dl
                    .bytes
                    .splice(begin as usize..begin as usize + block.len(), block);

                // If have all blocks for a piece, we write it to file bytes.
                if piece_dl.remaining_blocks == 0 {
                    self.bitfield.set(index);
                    // We know for sure there's a value for this piece index.
                    let piece_dl = self.pending_pieces.remove(&index).unwrap();
                    anyhow::ensure!(
                        validate_piece(&piece_dl.bytes, piece_dl.piece_hash),
                        "piece validation failed"
                    );

                    let begin_idx: usize = index as usize * self.torrent_info.piece_length as usize;
                    let end_idx: usize = begin_idx + piece_dl.piece_length as usize;
                    self.bytes.splice(begin_idx..end_idx, piece_dl.bytes);
                }

                self.request_pieces(inflight, message_stream).await?;
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

    async fn request_pieces(
        &mut self,
        inflight: &mut u8,
        message_stream: &mut Framed<TcpStream, MessageCodec>,
    ) -> anyhow::Result<()> {
        if self.state.choked {
            println!("Cannot request more pieces as we are choked.");
            return Ok(());
        }

        for (piece_index, piece_dl) in self.pending_pieces.iter_mut() {
            // requested should never go above piece_length.
            debug_assert!(piece_dl.requested <= piece_dl.piece_length);
            if piece_dl.requested == piece_dl.piece_length {
                println!("all blocks already requested for piece: {}", piece_index);
                continue;
            }

            // Requests as many blocks as possible for the given piece.
            request_blocks(
                inflight,
                *piece_index,
                piece_dl,
                message_stream,
                &mut self.pending_blocks,
            )
            .await?;
        }

        // If there are no more blocks for the pending requests,
        // go for the next piece.
        if *inflight < MAX_INFLIGHT {
            // Every time we make a request for the first block of a new piece,
            // we insert an entry in pending_pieces. So if we see a repetitive
            // piece index, it means that there are requests for all the blocks
            // of all the *remaining* pieces.
            let Some(piece_index) = self.bitfield.pick_piece() else {
                println!("No more pieces to send requests for as we own all of them.");
                return Ok(());
            };
            if self.pending_pieces.contains_key(&piece_index) {
                println!("There are pending requests for all blocks/pieces.");
                return Ok(());
            }

            let piece_length = self.torrent_info.get_piece_length(piece_index);
            let num_blocks = (piece_length + BLOCK_SIZE - 1) / BLOCK_SIZE;
            println!("Picked next piece: {}, blocks: {}", piece_index, num_blocks);

            let mut piece_dl = PieceDownload {
                piece_hash: self.torrent_info.pieces[piece_index as usize],
                piece_length,
                requested: 0,
                remaining_blocks: num_blocks as u16,
                bytes: vec![0; piece_length as usize],
            };

            self.request_blocks(inflight, piece_index, &mut piece_dl, message_stream)
                .await?;
            assert!(self.pending_pieces.insert(piece_index, piece_dl).is_none());

            // println!("pending pieces");
            // for (k, v) in &self.pending_pieces {
            //     println!(
            //         "piece index: {k}, requested: {}, num remaining blocks: {}",
            //         v.requested, v.remaining_blocks
            //     );
            // }
            // println!("pending blocks");
            // for b in &self.pending_blocks {
            //     println!("{:?}", b);
            // }
        }
        Ok(())
    }

    async fn request_blocks(
        &mut self,
        inflight: &mut u8,
        piece_index: u32,
        piece_dl: &mut PieceDownload,
        message_stream: &mut Framed<TcpStream, MessageCodec>,
    ) -> anyhow::Result<()> {
        while piece_dl.requested < piece_dl.piece_length && *inflight < MAX_INFLIGHT {
            println!(
                "requesting next block from piece: {}, {}/{}",
                piece_index, piece_dl.requested, piece_dl.piece_length,
            );

            let mut block_len = piece_dl.piece_length - piece_dl.requested;
            if block_len > BLOCK_SIZE {
                block_len = BLOCK_SIZE;
            }

            let req = Message::Request {
                index: piece_index,
                begin: piece_dl.requested,
                length: block_len,
            };
            message_stream.send(req).await?;

            let block_dl = BlockDownload {
                piece_index,
                block_begin: piece_dl.requested,
                block_len,
            };
            assert!(
                self.pending_blocks.insert(block_dl),
                "block download already requested"
            );
            piece_dl.requested += block_len;

            *inflight += 1;
        }
        Ok(())
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
