use std::net::{Ipv4Addr, SocketAddr};
use std::time::{Duration, Instant};

use bytes::{Buf, BufMut, BytesMut};
use tokio::net::UdpSocket;
use url::Url;

const PROTOCOL_ID: u64 = 0x41727101980;
const ACTION_CONNECT: u32 = 0;
const ACTION_ANNOUNCE: u32 = 1;
const ACTION_ERROR: u32 = 3;

const RETRY_TIMEOUT: [u64; 3] = [15, 45, 105];

pub struct UdpTracker {
    socket: UdpSocket,
    buf: BytesMut,
    key: u32,
    conn_id: u64,
    conn_id_ts: Instant,
    // we cannot announce again before next_announce_ts.
    next_announce: Instant,
}

impl UdpTracker {
    pub async fn connect(url: String) -> anyhow::Result<Self> {
        let key = rand::random();

        // letting the os choose the port for us.
        let socket = UdpSocket::bind("0.0.0.0:0").await?;
        let parsed_url = Url::parse(&url)?;
        // Extract hostname and port
        let host = match parsed_url.host_str() {
            None => anyhow::bail!("url missing host"),
            Some(host) => host,
        };
        let port = match parsed_url.port() {
            None => anyhow::bail!("url missing port"),
            Some(port) => port,
        };
        let addr = format!("{}:{}", host, port);
        socket.connect(&addr).await?;
        println!("Connected to tracker");

        let buf = BytesMut::with_capacity(512);
        let mut tracker = Self {
            socket,
            buf,
            key,
            conn_id: 0,
            conn_id_ts: Instant::now(),
            next_announce: Instant::now(),
        };

        // When calling get_conn_id for the first time, we won't
        // retry if we don't get a response in time because it already
        // means that this tracker is low quality.
        tokio::time::timeout(Duration::from_secs(5), tracker.get_conn_id()).await??;
        // match  {
        //     Ok(res) => match res {
        //         Ok(_) => return Ok(()),
        //         Err(e) => println!("get connection id error: {}", e),
        //     },
        //     Err(_) => println!("get connection id timeout"),
        // }
        // tracker.get_conn_id().await?;
        Ok(tracker)
    }

    async fn retry_get_conn_id(&mut self) -> anyhow::Result<()> {
        for timeout in RETRY_TIMEOUT {
            match tokio::time::timeout(Duration::from_secs(timeout), self.get_conn_id()).await {
                Ok(res) => match res {
                    Ok(_) => return Ok(()),
                    Err(e) => println!("get connection id error: {}", e),
                },
                Err(_) => println!("get connection id timeout"),
            }
        }
        anyhow::bail!("failed to get connection id");
    }

    async fn get_conn_id(&mut self) -> anyhow::Result<()> {
        // Clear the buffer before any operation.
        self.buf.clear();
        self.buf.put_u64(PROTOCOL_ID);
        self.buf.put_u32(ACTION_CONNECT);
        let tx_id: u32 = rand::random();
        self.buf.put_u32(tx_id);

        let n = self.socket.send(&self.buf).await?;
        anyhow::ensure!(self.buf.remaining() == n);

        self.buf.clear();
        self.socket.recv_buf(&mut self.buf).await?;

        anyhow::ensure!(16 == self.buf.remaining());
        let action = self.buf.get_u32();
        anyhow::ensure!(tx_id == self.buf.get_u32());
        self.validate_action(ACTION_CONNECT, action)?;
        self.conn_id = self.buf.get_u64();
        self.conn_id_ts = Instant::now();
        Ok(())
    }

    pub async fn announce(
        &mut self,
        info_hash: &[u8; 20],
        peer_id: &[u8; 20],
        downloaded: u64,
        left: u64,
        uploaded: u64,
    ) -> anyhow::Result<(Vec<SocketAddr>, u32)> {
        // TODO: refactor this part into a separate retry function.
        for timeout in RETRY_TIMEOUT {
            let announce_fut = self.send_announce(info_hash, peer_id, downloaded, left, uploaded);
            match tokio::time::timeout(Duration::from_secs(timeout), announce_fut).await {
                Ok(res) => match res {
                    Ok(resp) => return Ok(resp),
                    Err(e) => println!("announce error: {}", e),
                },
                Err(_) => println!("announce timeout"),
            }
        }
        anyhow::bail!("failed to get connection id");
    }

    async fn send_announce(
        &mut self,
        info_hash: &[u8; 20],
        peer_id: &[u8; 20],
        downloaded: u64,
        left: u64,
        uploaded: u64,
    ) -> anyhow::Result<(Vec<SocketAddr>, u32)> {
        if Instant::now() < self.next_announce {
            anyhow::bail!("announce interval has not passed yet");
        }

        if self.conn_id_ts.elapsed() > Duration::from_secs(60) {
            self.retry_get_conn_id().await?;
        }

        let tx_id: u32 = rand::random();
        let port = self.socket.peer_addr().unwrap().port();

        self.buf.clear();
        self.buf.put_u64(self.conn_id);
        self.buf.put_u32(ACTION_ANNOUNCE);
        self.buf.put_u32(tx_id);
        self.buf.put(&info_hash[..]);
        self.buf.put(&peer_id[..]);
        self.buf.put_u64(downloaded);
        self.buf.put_u64(left);
        self.buf.put_u64(uploaded);
        self.buf.put_u32(0);
        self.buf.put_u32(0);
        self.buf.put_u32(self.key);
        self.buf.put_i32(-1);
        self.buf.put_u16(port);

        let n = self.socket.send(&self.buf).await?;
        anyhow::ensure!(98 == n);

        self.buf.clear();
        self.socket.recv_buf(&mut self.buf).await?;

        anyhow::ensure!(self.buf.remaining() >= 20);
        let action = self.buf.get_u32();
        anyhow::ensure!(tx_id == self.buf.get_u32());
        self.validate_action(ACTION_ANNOUNCE, action)?;

        let interval = Duration::from_secs(self.buf.get_u32() as u64);
        self.next_announce = Instant::now() + interval;

        let leechers = self.buf.get_u32();
        println!("num leechers: {}", leechers);

        let seeders = self.buf.get_u32();
        println!("num seeders: {}", seeders);

        anyhow::ensure!(
            self.buf.remaining() % 6 == 0,
            "ip/port bytes should be divisible by 6"
        );

        let num_peers = self.buf.remaining() / 6;
        let mut peers = Vec::with_capacity(num_peers);

        for _ in 0..num_peers {
            let ip = Ipv4Addr::from(self.buf.get_u32());
            let port = self.buf.get_u16();
            peers.push(SocketAddr::from((ip, port)));
        }
        Ok((peers, seeders))
    }

    fn validate_action(&mut self, expected: u32, action: u32) -> anyhow::Result<()> {
        if action == expected {
            return Ok(());
        }

        if action == ACTION_ERROR {
            let mut msg = vec![0; self.buf.remaining()];
            self.buf.copy_to_slice(&mut msg[..]);
            anyhow::bail!(
                "tracker returned error: {}",
                std::str::from_utf8(&msg).unwrap()
            );
        }

        anyhow::bail!("expected action: {}, received: {}", expected, action);
    }
}
