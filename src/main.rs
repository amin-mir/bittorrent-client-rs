use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use rand::{distributions::Alphanumeric, Rng};

mod bencode;
use bencode::{Decoder, DownloadType, Torrent};

mod download;

mod tracker;
use tracker::Tracker;

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
            let mut torrent = parse_meta_info(torrent_file)?;
            let DownloadType::SingleFile { length } = torrent.info.download_type else {
                bail!("expected single file torrent");
            };
            println!("output = {output}, expected length: {length}");

            let t = Tracker::new(torrent.announce);

            let info_hash = torrent.info.hash();
            let self_id = gen_peer_id();
            let req = tracker::DiscoverRequest::new(&info_hash, &self_id, length);
            let peers = t.discover(req).await?;

            let mut file = download::File::new(self_id, torrent.info, peers);
            let file_bytes = file.download().await?;

            println!("downloaded file with len={}", file_bytes.len());

            tokio::fs::write(output, file_bytes).await?;

            // Implement FileWriter and Write pieces to a preallocated file.

            Ok(())
        }
    }
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
