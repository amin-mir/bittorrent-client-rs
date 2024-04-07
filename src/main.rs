use std::net::SocketAddr;

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use rand::{distributions::Alphanumeric, Rng};

mod bencode;
use bencode::{Decoder, DownloadType, Torrent};

mod download;

mod tracker;
use tracker::UdpTracker;

mod file;

#[derive(Debug, Parser)]
struct Args {
    #[arg(long, default_value_t = Decoder::Nom)]
    decoder: Decoder,
    #[command(subcommand)]
    commands: Commands,
}

#[derive(Debug, Subcommand)]
enum Commands {
    Decode {
        value: String,
    },
    Download {
        #[arg(short, long)]
        output: String,
        torrent_file: String,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let decoder = &args.decoder;

    match args.commands {
        Commands::Decode { value } => {
            let (remaining, decoded_value) = decoder.decode(value.as_bytes())?;
            debug_assert!(remaining.is_empty());
            println!("{:?}", decoded_value);
            Ok(())
        }
        Commands::Download {
            output,
            torrent_file,
        } => {
            let torrent = parse_meta_info(torrent_file)?;
            let dl_type = torrent.info.download_type.clone();
            println!("{:?}", torrent.announce_list);
            println!(
                "output = {output}, expected length: {}",
                torrent.info.total_length()
            );

            let info_hash = torrent.info.hash();
            let self_id = gen_peer_id();
            let total_length = torrent.info.total_length();

            let peers = pick_tracker(
                torrent.announce_list,
                info_hash,
                self_id.clone(),
                total_length,
            )
            .await?;

            let file = download::File::new(self_id, torrent.info, peers);
            let file_bytes = file.download().await?;

            println!("downloaded file with len={}", file_bytes.len());

            file::write(output, dl_type, file_bytes).await?;

            Ok(())
        }
    }
}

async fn pick_tracker(
    announce_list: Vec<Vec<String>>,
    info_hash: [u8; 20],
    self_id: String,
    total_length: u64,
) -> anyhow::Result<Vec<SocketAddr>> {
    for url in announce_list {
        let url = url.get(0).unwrap();
        let mut tracker = match UdpTracker::connect(url.clone()).await {
            Err(e) => {
                println!("Error connecting to tracker {}: {}", url, e);
                continue;
            }
            Ok(t) => t,
        };

        let tracker_announce = tracker.announce(
            &info_hash,
            self_id.as_bytes().try_into()?,
            0,
            total_length,
            0,
        );

        let (peers, seeders) = match tracker_announce.await {
            Err(e) => {
                println!("Error announcing to tracker {}: {}", url, e);
                continue;
            }
            Ok(resp) => resp,
        };

        if seeders >= 5 {
            println!("picked tracker: {}", url);
            return Ok(peers);
        }
        continue;
    }
    anyhow::bail!("did not found a quality tracker");
}

pub fn gen_peer_id() -> String {
    rand::thread_rng()
        .sample_iter(&Alphanumeric)
        .take(20)
        .map(char::from)
        .collect()
}

fn parse_meta_info(torrent_file: String) -> Result<Torrent> {
    let meta_bytes = std::fs::read(torrent_file).context("expecting a valid torrent file path")?;
    serde_bencode::from_bytes(&meta_bytes).map_err(|e| e.into())
}
