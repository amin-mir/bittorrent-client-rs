use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::sink::SinkExt;
use futures::stream::StreamExt;
use sha1::{Digest, Sha1};
use tokio::net::TcpStream;
use tokio::sync::Mutex;
use tokio::time;
use tokio_util::codec::Framed;

use crate::bencode::Info;
use crate::tracker;

mod handshake;
pub use handshake::{Handshake, HandshakeCodec};

mod peer_msg;
pub use peer_msg::{Message, MessageCodec};

mod bitfield;
use bitfield::Bitfield;

const BLOCK_SIZE: u32 = 1 << 14;
const MAX_INFLIGHT_PER_PEER: u8 = 5;
const MAX_SEEDS: usize = 5;
const KEEP_ALIVE_INTERVAL: Duration = Duration::from_secs(20);

pub struct File {
    pub id: String,
    pub torrent_info: Arc<Info>,
    pub peers: Vec<SocketAddr>,
    bytes: Vec<u8>,
}

impl File {
    pub fn new(id: String, torrent_info: Info, peers: Vec<SocketAddr>) -> Self {
        let torrent_info = Arc::new(torrent_info);
        let bytes = vec![0; torrent_info.total_length() as usize];
        Self {
            id,
            torrent_info,
            peers,
            bytes,
        }
    }

    async fn pick_seeds(
        &mut self,
        own_bitfield: Arc<Mutex<Bitfield>>,
        downloaded_pieces_tx: kanal::AsyncSender<DownloaderMsg>,
    ) -> anyhow::Result<()> {
        let total_pieces = self.torrent_info.num_pieces() as usize;
        let id = self.id.as_bytes().try_into()?;
        let info_hash = self.torrent_info.hash();
        let mut num_seeds = 0;

        for addr in &self.peers[..] {
            let sleep = time::sleep(Duration::from_secs(5));
            tokio::pin!(sleep);

            let handshake_res = tokio::select! {
                _ = &mut sleep => {
                    // println!("Peer {} handshake timeout", addr);
                    continue;
                }
                // SocketAddr is Copy, that's why we can do *addr. it won't move out.
                handshake_res = check_is_seed(*addr, id, info_hash, total_pieces) => handshake_res,
            };

            match handshake_res {
                Ok(stream) => {
                    num_seeds += 1;
                    let own_bitfield = Arc::clone(&own_bitfield);
                    let mut downloader = SeedDownloader::new(
                        Arc::clone(&self.torrent_info),
                        own_bitfield,
                        stream,
                        downloaded_pieces_tx.clone(),
                    );
                    tokio::spawn(async move {
                        downloader.download().await;
                    });
                    if num_seeds == MAX_SEEDS {
                        break;
                    }
                }
                Err(e) => {
                    println!("seed check for peer({}) failed: {}", addr, e);
                    continue;
                }
            }
        }
        Ok(())
    }

    pub async fn download(mut self) -> anyhow::Result<Vec<u8>> {
        println!(
            "num peers: {}, total length: {}, total pieces: {}, piece length: {}",
            self.peers.len(),
            self.torrent_info.total_length(),
            self.torrent_info.num_pieces(),
            self.torrent_info.piece_length
        );

        let own_bitfield = Bitfield::zero(self.torrent_info.num_pieces());
        let own_bitfield = Arc::new(Mutex::new(own_bitfield));
        let (downloaded_pieces_tx, downloaded_pieces_rx) = kanal::bounded_async::<DownloaderMsg>(5);

        self.pick_seeds(Arc::clone(&own_bitfield), downloaded_pieces_tx)
            .await?;

        // TODO: read from the downloaded_pieces_rx
        loop {
            match downloaded_pieces_rx.recv().await? {
                DownloaderMsg::Piece {
                    index,
                    length,
                    bytes,
                } => {
                    let begin_idx: usize = index as usize * self.torrent_info.piece_length as usize;
                    let end_idx: usize = begin_idx + length as usize;
                    self.bytes.splice(begin_idx..end_idx, bytes);
                }
                DownloaderMsg::Disconnect(addr) => println!("Seed {} disconnected", addr),
            }

            let own_bitfield = own_bitfield.lock().await;
            // println!("remaining pieces: {}", own_bitfield.remaining());
            if own_bitfield.remaining() == 0 {
                break;
            }
        }

        Ok(self.bytes)
    }
}

async fn check_is_seed(
    addr: SocketAddr,
    id: [u8; 20],
    info_hash: [u8; 20],
    total_pieces: usize,
) -> anyhow::Result<Framed<TcpStream, MessageCodec>> {
    let stream = TcpStream::connect(addr).await?;
    let mut handshake_stream = Framed::new(stream, HandshakeCodec);

    let peer_handshake = handshake(id, info_hash, &mut handshake_stream).await?;
    println!(
        "Handshake done with peer ID: {}",
        hex::encode(peer_handshake.peer_id)
    );

    let mut message_stream = handshake_stream.map_codec(|_c| MessageCodec);

    let Some(msg) = message_stream.next().await else {
        anyhow::bail!("stream ended before first msg");
    };
    let msg = msg?;
    match msg {
        Message::Bitfield(bytes) => {
            if !bitfield::is_seed(total_pieces, &bytes) {
                anyhow::bail!("peer ({}) is not seed", addr);
            }
            Ok(message_stream)
        }
        m => anyhow::bail!("first message was not bitfield: {:?}", m),
    }
}

async fn handshake(
    id: [u8; 20],
    info_hash: [u8; 20],
    handshake_stream: &mut Framed<TcpStream, HandshakeCodec>,
) -> anyhow::Result<Handshake> {
    let handshake = Handshake::new(info_hash, id);

    handshake_stream.send(handshake).await?;
    match handshake_stream.next().await {
        None => Err(anyhow::anyhow!("peer stream ended during handshake")),
        Some(peer_handshake) => peer_handshake.map_err(Into::into),
    }
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

struct SeedDownloader {
    addr: SocketAddr,
    choked_us: bool,
    torrent_info: Arc<Info>,
    own_bitfield: Arc<Mutex<Bitfield>>,
    stream: Framed<TcpStream, MessageCodec>,
    downloaded_pieces_tx: kanal::AsyncSender<DownloaderMsg>,
    /// Map of all the pieces which are currently being downloaded
    /// from seeds keyed by piece index.
    pending_pieces: HashMap<u32, PieceDownload>,
    /// This set contains all the outgoing block requests.
    /// The main purpose is validating that we get the blocks
    /// that we previously sent request for.
    pending_blocks: HashSet<BlockDownload>,
}

enum DownloaderMsg {
    Disconnect(SocketAddr),
    Piece {
        index: u32,
        length: u32,
        bytes: Vec<u8>,
    },
}

impl SeedDownloader {
    fn new(
        torrent_info: Arc<Info>,
        own_bitfield: Arc<Mutex<Bitfield>>,
        stream: Framed<TcpStream, MessageCodec>,
        downloaded_pieces_tx: kanal::AsyncSender<DownloaderMsg>,
    ) -> Self {
        Self {
            addr: stream.get_ref().peer_addr().unwrap(),
            choked_us: true,
            torrent_info,
            own_bitfield,
            stream,
            downloaded_pieces_tx,
            pending_pieces: HashMap::new(),
            pending_blocks: HashSet::new(),
        }
    }

    async fn download(&mut self) {
        println!("picked a seed: {}", self.addr);
        let mut inflight: u8 = 0;

        loop {
            {
                let own_bitfield = self.own_bitfield.lock().await;
                if own_bitfield.remaining() == 0 {
                    break;
                }
            }

            let keepalive_timeout = time::sleep(KEEP_ALIVE_INTERVAL);
            tokio::pin!(keepalive_timeout);

            let msg_res = tokio::select! {
                _ = &mut keepalive_timeout => {
                    println!("Sending keepalive to peer {}", self.addr);
                    if let Err(e) = self.stream.send(Message::KeepAlive).await {
                        println!("Error sending keepalive to peer {}: {}", self.addr, e);
                    }
                    continue;
                }
                msg_res = self.stream.next() => msg_res,
            };

            let msg = match msg_res {
                None => {
                    // Peer stream ended. Report to the receiver and exit.
                    if let Err(e) = self
                        .downloaded_pieces_tx
                        .send(DownloaderMsg::Disconnect(self.addr))
                        .await
                    {
                        println!("Error announcing peer {} disconnect: {}", self.addr, e);
                    }
                    break;
                }
                Some(msg) => msg,
            };

            match msg {
                Err(e) => println!("error reading message from seed {}: {}", self.addr, e),
                Ok(msg) => {
                    if let Err(e) = self.handle_msg(&mut inflight, msg).await {
                        println!("Error happened while handling seed message: {e}");
                    }
                }
            }
        }

        println!("loop ended for peer {}", self.addr);
    }

    // Return true if there are no more pieces to download.
    async fn handle_msg(&mut self, inflight: &mut u8, msg: Message) -> anyhow::Result<()> {
        match msg {
            Message::KeepAlive => {
                println!("Received keep alive for seed {}", self.addr);
                Ok(())
            }
            Message::Choke => {
                if self.choked_us {
                    println!("peer {} sent duplicate choke", self.addr);
                }
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
                self.choked_us = true;
                for (_, dl) in self.pending_pieces.iter_mut() {
                    // By setting requested to 0, piece bytes will be overwritten.
                    // We prefer to do this to re-use the already allocated Vector.
                    // Also remaining_blocks needs to reset otherwise we will finish
                    // the download prematurely.
                    dl.requested = 0;
                    dl.remaining_blocks = ((dl.piece_length + BLOCK_SIZE - 1) / BLOCK_SIZE) as u16;
                }
                self.pending_blocks.clear();

                println!("peer {} choked us", self.addr);
                Ok(())
            }
            Message::Unchoke => {
                if !self.choked_us {
                    println!("peer {} sent duplicate unchoke", self.addr);
                    return Ok(());
                }
                self.choked_us = false;
                println!("peer {} unchoked us", self.addr);

                self.request_pieces(inflight).await?;
                Ok(())
            }
            Message::Bitfield(_) => {
                anyhow::bail!("peer {} sent bitfield out of order", self.addr)
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

                let Some(piece_dl) = self.pending_pieces.get_mut(&index) else {
                    anyhow::bail!("received block for unrequested piece: {}", index);
                };
                piece_dl.remaining_blocks -= 1;

                *inflight -= 1;

                // println!(
                //     "Received block [{}-{})/{} for piece ({}), remaining blocks: {}",
                //     begin,
                //     begin + block.len() as u32,
                //     piece_dl.piece_length,
                //     index,
                //     piece_dl.remaining_blocks,
                // );

                piece_dl
                    .bytes
                    .splice(begin as usize..begin as usize + block.len(), block);

                // If have all blocks for a piece, we write it to file bytes.
                if piece_dl.remaining_blocks == 0 {
                    let mut own_bitfield = self.own_bitfield.lock().await;
                    own_bitfield.set(index);
                    // We know for sure there's a value for this piece index.
                    let piece_dl = self.pending_pieces.remove(&index).unwrap();
                    anyhow::ensure!(
                        validate_piece(&piece_dl.bytes, piece_dl.piece_hash),
                        "piece validation failed"
                    );

                    self.downloaded_pieces_tx
                        .send(DownloaderMsg::Piece {
                            index,
                            length: piece_dl.piece_length,
                            bytes: piece_dl.bytes,
                        })
                        .await?;
                }

                self.request_pieces(inflight).await?;
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

    async fn request_pieces(&mut self, inflight: &mut u8) -> anyhow::Result<()> {
        if self.choked_us {
            println!("Cannot request more pieces as we are choked.");
            return Ok(());
        }

        for (piece_index, piece_dl) in self.pending_pieces.iter_mut() {
            // requested should never go above piece_length.
            debug_assert!(piece_dl.requested <= piece_dl.piece_length);
            debug_assert!(piece_dl.remaining_blocks > 0);
            if piece_dl.requested == piece_dl.piece_length {
                // println!(
                //     "({}) all blocks already requested for piece: {}",
                //     self.addr, piece_index
                // );
                continue;
            }

            // Requests as many blocks as possible for the given piece.
            request_blocks(
                self.addr,
                inflight,
                *piece_index,
                piece_dl,
                &mut self.stream,
                &mut self.pending_blocks,
            )
            .await?;
        }

        // If there are no more blocks for the pending requests,
        // go for the next piece.
        if *inflight < MAX_INFLIGHT_PER_PEER {
            let piece_index = {
                let mut own_bitfield = self.own_bitfield.lock().await;
                let Some(piece_index) = own_bitfield.pick_piece() else {
                    println!("We have requested all pieces.");
                    return Ok(());
                };
                piece_index
            };

            // Every time we make a request for the first block of a new piece,
            // we insert an entry in pending_pieces. So if we see a repetitive
            // piece index, it means that there are requests for all the blocks
            // of all the *remaining* pieces.
            if self.pending_pieces.contains_key(&piece_index) {
                println!("There are pending requests for all blocks/pieces.");
                return Ok(());
            }

            let piece_length = self.torrent_info.get_piece_length(piece_index);
            let num_blocks = (piece_length + BLOCK_SIZE - 1) / BLOCK_SIZE;

            let mut piece_dl = PieceDownload {
                piece_hash: self.torrent_info.pieces[piece_index as usize],
                piece_length,
                requested: 0,
                remaining_blocks: num_blocks as u16,
                bytes: vec![0; piece_length as usize],
            };

            request_blocks(
                self.addr,
                inflight,
                piece_index,
                &mut piece_dl,
                &mut self.stream,
                &mut self.pending_blocks,
            )
            .await?;
            assert!(self.pending_pieces.insert(piece_index, piece_dl).is_none());

            let mut dl_report = format!("========== Peer {} ==========\n", self.addr);
            dl_report.push_str(&format!(
                "picked next piece: {}, blocks: {}\n",
                piece_index, num_blocks,
            ));
            dl_report.push_str("Pending pieces:\n");
            for (k, v) in &self.pending_pieces {
                dl_report.push_str(&format!(
                    "piece index: {k}, requested: {}, num remaining blocks: {}\n",
                    v.requested, v.remaining_blocks
                ));
            }
            dl_report.push_str("Pending blocks\n");
            for b in &self.pending_blocks {
                dl_report.push_str(&format!("{:?}\n", b));
            }
            // println!("{dl_report}");
            println!(
                "{} => picked next piece: {}, blocks: {}",
                self.addr, piece_index, num_blocks,
            );
        }

        Ok(())
    }
}

async fn request_blocks(
    addr: SocketAddr,
    inflight: &mut u8,
    piece_index: u32,
    piece_dl: &mut PieceDownload,
    message_stream: &mut Framed<TcpStream, MessageCodec>,
    pending_blocks: &mut HashSet<BlockDownload>,
) -> anyhow::Result<()> {
    while piece_dl.requested < piece_dl.piece_length {
        // println!(
        //     "({}) requesting next block from piece: {}, {}/{}",
        //     addr, piece_index, piece_dl.requested, piece_dl.piece_length,
        // );

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
            pending_blocks.insert(block_dl),
            "block download already requested"
        );
        piece_dl.requested += block_len;

        *inflight += 1;
        if *inflight == MAX_INFLIGHT_PER_PEER {
            break;
        }
    }
    Ok(())
}

fn validate_piece(piece: &[u8], hash: [u8; 20]) -> bool {
    let mut hasher = Sha1::new();
    hasher.update(piece);
    let piece_hash: [u8; 20] = hasher.finalize().into();
    hash == piece_hash
}
