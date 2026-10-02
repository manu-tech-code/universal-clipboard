//! Noise_XX over TCP. Both sides authenticate with their static keys, and every frame after
//! the handshake is encrypted and authenticated. Reading and writing use independent nonce
//! counters so the two halves can run in separate tasks.

use crate::identity::NOISE_PARAMS;
use crate::wire::Msg;
use anyhow::{Result, bail};
use snow::{Builder, StatelessTransportState};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};

const CHUNK: usize = 60_000;
const MAX_MSG: usize = 8 * 1024 * 1024;

pub struct Secure {
    pub reader: SecureReader,
    pub writer: SecureWriter,
    pub remote_static: Vec<u8>,
    pub handshake_hash: Vec<u8>,
}

pub struct SecureReader {
    r: OwnedReadHalf,
    t: Arc<StatelessTransportState>,
    nonce: u64,
    buf: Vec<u8>,
}

pub struct SecureWriter {
    w: OwnedWriteHalf,
    t: Arc<StatelessTransportState>,
    nonce: u64,
}

async fn write_frame(w: &mut OwnedWriteHalf, data: &[u8]) -> Result<()> {
    w.write_all(&(data.len() as u16).to_be_bytes()).await?;
    w.write_all(data).await?;
    Ok(())
}

async fn read_frame(r: &mut OwnedReadHalf) -> Result<Vec<u8>> {
    let mut len = [0u8; 2];
    r.read_exact(&mut len).await?;
    let mut buf = vec![0u8; u16::from_be_bytes(len) as usize];
    r.read_exact(&mut buf).await?;
    Ok(buf)
}

pub async fn handshake(stream: TcpStream, private: &[u8], initiator: bool) -> Result<Secure> {
    stream.set_nodelay(true)?;
    let builder = Builder::new(NOISE_PARAMS.parse()?).local_private_key(private)?;
    let mut hs = if initiator { builder.build_initiator()? } else { builder.build_responder()? };
    let (mut r, mut w) = stream.into_split();
    let mut buf = vec![0u8; 65535];
    while !hs.is_handshake_finished() {
        if hs.is_my_turn() {
            let n = hs.write_message(&[], &mut buf)?;
            write_frame(&mut w, &buf[..n]).await?;
        } else {
            let frame = read_frame(&mut r).await?;
            hs.read_message(&frame, &mut buf)?;
        }
    }
    let Some(remote) = hs.get_remote_static().map(|k| k.to_vec()) else { bail!("peer sent no static key") };
    let handshake_hash = hs.get_handshake_hash().to_vec();
    let t = Arc::new(hs.into_stateless_transport_mode()?);
    Ok(Secure {
        reader: SecureReader { r, t: t.clone(), nonce: 0, buf: Vec::new() },
        writer: SecureWriter { w, t, nonce: 0 },
        remote_static: remote,
        handshake_hash,
    })
}

impl SecureReader {
    /// Not cancel-safe: abort the task that owns the reader instead of wrapping this in a timeout.
    pub async fn recv(&mut self) -> Result<Msg> {
        loop {
            if self.buf.len() >= 4 {
                let len = u32::from_le_bytes(self.buf[..4].try_into().unwrap()) as usize;
                if len > MAX_MSG {
                    bail!("message too large ({len} bytes)");
                }
                if self.buf.len() >= 4 + len {
                    let body: Vec<u8> = self.buf.drain(..4 + len).skip(4).collect();
                    return Ok(postcard::from_bytes(&body)?);
                }
            }
            let frame = read_frame(&mut self.r).await?;
            let mut out = vec![0u8; frame.len()];
            let n = self.t.read_message(self.nonce, &frame, &mut out)?;
            self.nonce += 1;
            self.buf.extend_from_slice(&out[..n]);
        }
    }
}

impl SecureWriter {
    pub async fn send(&mut self, msg: &Msg) -> Result<()> {
        let body = postcard::to_stdvec(msg)?;
        let mut framed = Vec::with_capacity(body.len() + 4);
        framed.extend_from_slice(&(body.len() as u32).to_le_bytes());
        framed.extend_from_slice(&body);
        for chunk in framed.chunks(CHUNK) {
            let mut out = vec![0u8; chunk.len() + 16];
            let n = self.t.write_message(self.nonce, chunk, &mut out)?;
            self.nonce += 1;
            write_frame(&mut self.w, &out[..n]).await?;
        }
        Ok(())
    }
}

/// Six digits both screens show after the handshake. If someone sits in the middle, the two
/// handshakes differ, so the digits differ and the user declines.
pub fn sas(handshake_hash: &[u8]) -> String {
    let n = u32::from_be_bytes(handshake_hash[..4].try_into().unwrap()) % 1_000_000;
    format!("{:03} {:03}", n / 1000, n % 1000)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::TcpListener;

    #[tokio::test]
    async fn handshake_and_large_message_roundtrip() {
        let a = snow::Builder::new(NOISE_PARAMS.parse().unwrap()).generate_keypair().unwrap();
        let b = snow::Builder::new(NOISE_PARAMS.parse().unwrap()).generate_keypair().unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let bpriv = b.private.clone();
        let server = tokio::spawn(async move {
            let (s, _) = listener.accept().await.unwrap();
            handshake(s, &bpriv, false).await.unwrap()
        });
        let client = handshake(TcpStream::connect(addr).await.unwrap(), &a.private, true).await.unwrap();
        let mut server = server.await.unwrap();
        assert_eq!(client.remote_static, b.public);
        assert_eq!(server.remote_static, a.public);
        assert_eq!(sas(&client.handshake_hash), sas(&server.handshake_hash));

        let text = "x".repeat(300_000);
        let mut cw = client.writer;
        let msg = Msg::Clip(crate::wire::ClipMsg {
            id: "1".into(),
            origin: "a".into(),
            ts_ms: 1,
            item: crate::wire::ClipItem::Text(text.clone()),
        });
        cw.send(&msg).await.unwrap();
        match server.reader.recv().await.unwrap() {
            Msg::Clip(c) => assert_eq!(c.item, crate::wire::ClipItem::Text(text)),
            other => panic!("unexpected {other:?}"),
        }
    }
}
