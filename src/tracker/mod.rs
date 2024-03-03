use std::fmt::{self, Display};

use anyhow::bail;
use serde::Deserialize;
use url::{form_urlencoded, Url};

mod peers;
pub use peers::Peers;

pub struct Tracker {
    url: String,
}

impl Tracker {
    pub fn new(url: String) -> Self {
        Self { url }
    }
}

pub struct DiscoverRequest<'a> {
    info_hash: &'a [u8; 20],
    peer_id: &'a str,
    ip: Option<String>,
    port: u16,
    uploaded: u64,
    downloaded: u64,
    left: u64,
    event: Option<Event>,
    compact: Option<bool>,
}

impl<'a> DiscoverRequest<'a> {
    pub fn new(info_hash: &'a [u8; 20], peer_id: &'a str, left: u64) -> Self {
        Self {
            info_hash,
            peer_id,
            ip: None,
            port: 6881,
            uploaded: 0,
            downloaded: 0,
            left,
            event: Some(Event::Started),
            compact: Some(true),
        }
    }

    fn url_encode(&self) -> String {
        let mut ser = form_urlencoded::Serializer::new(String::new());

        let pairs = [
            ("peer_id", self.peer_id),
            ("port", &self.port.to_string()),
            ("uploaded", &self.uploaded.to_string()),
            ("downloaded", &self.downloaded.to_string()),
            ("left", &self.left.to_string()),
        ];
        ser.extend_pairs(pairs);

        if let Some(ip) = &self.ip {
            ser.append_pair("ip", &ip);
        }
        if let Some(event) = &self.event {
            ser.append_pair("event", &event.to_string());
        }
        if self.compact.unwrap_or(false) {
            ser.append_pair("compact", "1");
        }

        ser.finish()
    }
}

impl Tracker {
    pub async fn discover(&self, req: DiscoverRequest<'_>) -> anyhow::Result<Peers> {
        let url = self.build_url(&req)?;
        let resp = reqwest::get(url).await?.bytes().await?;
        match serde_bencode::from_bytes(&resp[..])? {
            TrackerResponse::Failure { reason } => {
                bail!("tracker returned failure response with reason: {reason}")
            }
            TrackerResponse::Success { interval: _, peers } => Ok(peers),
        }
    }

    fn build_url(&self, req: &DiscoverRequest) -> anyhow::Result<Url> {
        let mut url = Url::parse(&self.url)?;

        let query_params = format!(
            "info_hash={}&{}",
            url_encode_bytes(&req.info_hash[..])?,
            req.url_encode()
        );
        url.set_query(Some(&query_params));

        Ok(url)
    }
}

#[derive(Deserialize)]
#[serde(untagged)]
enum TrackerResponse {
    Failure {
        #[serde(rename = "failure reason")]
        reason: String,
    },
    Success {
        interval: usize,
        peers: Peers,
    },
}

#[allow(dead_code)]
pub enum Event {
    Started,
    Completed,
    Stopped,
    Empty,
}

impl Display for Event {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Event::Started => f.write_str("started"),
            Event::Completed => f.write_str("completed"),
            Event::Stopped => f.write_str("stopped"),
            Event::Empty => f.write_str("empty"),
        }
    }
}

pub fn url_encode_bytes(bytes: &[u8]) -> anyhow::Result<String> {
    let mut res = vec![0; 3 * bytes.len()];
    let mut buf = [0; 1];
    for (i, &b) in bytes.iter().enumerate() {
        let mut idx = i * 3;
        res[idx] = b'%';

        idx += 1;
        buf[0] = b;
        hex::encode_to_slice(&buf[..], &mut res[idx..idx + 2])?;
    }
    String::from_utf8(res).map_err(Into::into)
}
