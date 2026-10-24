//! Bounded pre-TLS routing. No application bytes are decrypted here.
use std::io;
use tokio::io::{AsyncRead, AsyncReadExt};

pub const AGENT_ALPN: &[u8] = b"codeg-relay-agent/1";
pub const LIMIT: usize = 16 * 1024;

#[derive(Debug, PartialEq, Eq)]
pub enum Kind {
    Agent,
    Client,
    Health,
}

pub async fn classify<S: AsyncRead + Unpin>(stream: &mut S) -> io::Result<(Kind, Vec<u8>)> {
    tokio::time::timeout(std::time::Duration::from_secs(10), read_hello(stream))
        .await
        .map_err(|_| io::Error::from(io::ErrorKind::TimedOut))?
}

fn invalid() -> io::Error {
    io::ErrorKind::InvalidData.into()
}

async fn read_hello<S: AsyncRead + Unpin>(stream: &mut S) -> io::Result<(Kind, Vec<u8>)> {
    let first = stream.read_u8().await?;
    let mut wire = vec![first];
    if first == b'G' || first == b'H' {
        let expected = if first == b'G' {
            b"GET ".as_slice()
        } else {
            b"HEAD ".as_slice()
        };
        wire.resize(expected.len(), 0);
        stream.read_exact(&mut wire[1..]).await?;
        return if wire == expected {
            Ok((Kind::Health, wire))
        } else {
            Err(invalid())
        };
    }
    if first != 0x16 {
        return Err(invalid());
    }
    let mut hello = Vec::new();
    loop {
        let start = wire.len() - 1;
        if start + 5 > LIMIT {
            return Err(invalid());
        }
        wire.resize(start + 5, 0);
        stream.read_exact(&mut wire[start + 1..]).await?;
        if wire[start] != 0x16 || wire[start + 1] != 3 || !(1..=4).contains(&wire[start + 2]) {
            return Err(invalid());
        }
        let len = u16::from_be_bytes([wire[start + 3], wire[start + 4]]) as usize;
        if len == 0 || wire.len() + len > LIMIT {
            return Err(invalid());
        }
        let body = wire.len();
        wire.resize(body + len, 0);
        stream.read_exact(&mut wire[body..]).await?;
        hello.extend_from_slice(&wire[body..]);
        if hello.first() != Some(&1) {
            return Err(invalid());
        }
        if hello.len() >= 4 {
            let len = u32::from_be_bytes([0, hello[1], hello[2], hello[3]]) as usize;
            if len + 4 > LIMIT {
                return Err(invalid());
            }
            if hello.len() >= len + 4 {
                return Ok((parse_alpn(&hello[4..4 + len])?, wire));
            }
        }
        if wire.len() == LIMIT {
            return Err(invalid());
        }
        wire.push(stream.read_u8().await?);
    }
}

struct Cursor<'a>(&'a [u8]);
impl<'a> Cursor<'a> {
    fn take(&mut self, len: usize) -> io::Result<&'a [u8]> {
        if len > self.0.len() {
            return Err(invalid());
        }
        let (value, rest) = self.0.split_at(len);
        self.0 = rest;
        Ok(value)
    }
    fn u8(&mut self) -> io::Result<usize> {
        Ok(self.take(1)?[0] as usize)
    }
    fn u16(&mut self) -> io::Result<usize> {
        let value = self.take(2)?;
        Ok(u16::from_be_bytes([value[0], value[1]]) as usize)
    }
}

fn parse_alpn(bytes: &[u8]) -> io::Result<Kind> {
    let mut c = Cursor(bytes);
    let version = c.take(2)?;
    if version[0] != 3 || !(1..=4).contains(&version[1]) {
        return Err(invalid());
    }
    c.take(32)?;
    let len = c.u8()?;
    if len > 32 {
        return Err(invalid());
    }
    c.take(len)?;
    let len = c.u16()?;
    if len == 0 || len % 2 != 0 {
        return Err(invalid());
    }
    c.take(len)?;
    let len = c.u8()?;
    if len == 0 {
        return Err(invalid());
    }
    c.take(len)?;
    if c.0.is_empty() {
        return Ok(Kind::Client);
    }
    let len = c.u16()?;
    let mut extensions = Cursor(c.take(len)?);
    if !c.0.is_empty() {
        return Err(invalid());
    }
    let mut seen = std::collections::HashSet::new();
    let mut agent = false;
    while !extensions.0.is_empty() {
        let tag = extensions.u16()?;
        if !seen.insert(tag) {
            return Err(invalid());
        }
        let len = extensions.u16()?;
        let value = extensions.take(len)?;
        if tag == 16 {
            let mut alpn = Cursor(value);
            let len = alpn.u16()?;
            if len == 0 || len != alpn.0.len() {
                return Err(invalid());
            }
            while !alpn.0.is_empty() {
                let len = alpn.u8()?;
                if len == 0 {
                    return Err(invalid());
                }
                agent |= alpn.take(len)? == AGENT_ALPN;
            }
        }
    }
    Ok(if agent { Kind::Agent } else { Kind::Client })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncWriteExt;

    fn hello(protocols: &[&[u8]], split: usize) -> Vec<u8> {
        let mut body = vec![3, 3];
        body.extend([0; 32]);
        body.extend([0, 0, 2, 0x13, 1, 1, 0]);
        let mut extensions = Vec::new();
        if !protocols.is_empty() {
            let names: Vec<u8> = protocols
                .iter()
                .flat_map(|p| {
                    let mut v = vec![p.len() as u8];
                    v.extend_from_slice(p);
                    v
                })
                .collect();
            extensions.extend([0, 16]);
            extensions.extend(((names.len() + 2) as u16).to_be_bytes());
            extensions.extend((names.len() as u16).to_be_bytes());
            extensions.extend(names);
        }
        body.extend((extensions.len() as u16).to_be_bytes());
        body.extend(extensions);
        let mut handshake = vec![1];
        handshake.extend_from_slice(&(body.len() as u32).to_be_bytes()[1..]);
        handshake.extend(body);
        let mut wire = Vec::new();
        for part in handshake.chunks(split) {
            wire.extend([0x16, 3, 1]);
            wire.extend((part.len() as u16).to_be_bytes());
            wire.extend(part);
        }
        wire
    }

    #[tokio::test]
    async fn routes_agent_client_and_absent_alpn() {
        for (protocols, kind) in [
            (vec![AGENT_ALPN, b"http/1.1".as_slice()], Kind::Agent),
            (vec![b"h2".as_slice(), b"http/1.1".as_slice()], Kind::Client),
            (vec![], Kind::Client),
        ] {
            let wire = hello(&protocols, 4096);
            assert_eq!(classify(&mut wire.as_slice()).await.unwrap(), (kind, wire));
        }
    }

    #[tokio::test]
    async fn reassembles_multiple_records_and_replays_exact_wire() {
        for split in [1, 2, 3, 7, 32] {
            let wire = hello(&[AGENT_ALPN], split);
            assert_eq!(
                classify(&mut wire.as_slice()).await.unwrap(),
                (Kind::Agent, wire)
            );
        }
    }

    #[tokio::test]
    async fn health_and_garbage() {
        for wire in [b"GET /health".as_slice(), b"HEAD /health".as_slice()] {
            assert_eq!(classify(&mut &wire[..]).await.unwrap().0, Kind::Health);
        }
        for wire in [
            b"POST /".as_slice(),
            b"nonsense".as_slice(),
            &[0x16, 3, 0, 0, 1, 1],
        ] {
            assert!(classify(&mut &wire[..]).await.is_err());
        }
    }

    #[tokio::test]
    async fn rejects_malformed_truncated_and_oversized() {
        let valid = hello(&[AGENT_ALPN], 4096);
        for n in 0..valid.len() {
            assert!(classify(&mut &valid[..n]).await.is_err(), "prefix {n}");
        }
        let mut malformed = valid.clone();
        malformed[5] = 2; // ServerHello, not ClientHello.
        assert!(classify(&mut malformed.as_slice()).await.is_err());
        let mut malformed = valid;
        let last = malformed.len() - AGENT_ALPN.len() - 1;
        malformed[last] = 255; // ALPN name exceeds extension.
        assert!(classify(&mut malformed.as_slice()).await.is_err());
        assert!(classify(&mut &[0x16, 3, 1, 0xff, 0xff][..]).await.is_err());
    }

    #[tokio::test(start_paused = true)]
    async fn slow_partial_client_has_one_ten_second_budget() {
        let (mut writer, mut reader) = tokio::io::duplex(64);
        writer.write_all(&[0x16, 3]).await.unwrap();
        let start = tokio::time::Instant::now();
        assert_eq!(
            classify(&mut reader).await.unwrap_err().kind(),
            io::ErrorKind::TimedOut
        );
        assert_eq!(start.elapsed(), std::time::Duration::from_secs(10));
    }
}
