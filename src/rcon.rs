use anyhow::{Context, Result, bail};
use std::{
    io::{Read, Write},
    net::{TcpStream, ToSocketAddrs},
    time::Duration,
};

pub struct RconClient {
    pub host: String,
    pub port: u16,
    pub password: String,
    pub timeout: Duration,
}
impl RconClient {
    fn packet(stream: &mut TcpStream) -> Result<(i32, i32, String)> {
        let mut size = [0; 4];
        stream.read_exact(&mut size)?;
        let size = i32::from_le_bytes(size);
        if !(10..=4 * 1024 * 1024).contains(&size) {
            bail!("Invalid RCON packet length");
        }
        let mut body = vec![0; size as usize];
        stream.read_exact(&mut body)?;
        if body[body.len() - 2..] != [0, 0] {
            bail!("Invalid RCON terminator");
        }
        Ok((
            i32::from_le_bytes(body[..4].try_into()?),
            i32::from_le_bytes(body[4..8].try_into()?),
            String::from_utf8(body[8..body.len() - 2].to_vec())?,
        ))
    }
    fn send(stream: &mut TcpStream, id: i32, kind: i32, text: &str) -> Result<()> {
        let size = i32::try_from(text.len() + 10)?;
        stream.write_all(&size.to_le_bytes())?;
        stream.write_all(&id.to_le_bytes())?;
        stream.write_all(&kind.to_le_bytes())?;
        stream.write_all(text.as_bytes())?;
        stream.write_all(&[0, 0])?;
        Ok(())
    }
    pub fn command(&self, command: &str) -> Result<String> {
        let address = (self.host.as_str(), self.port)
            .to_socket_addrs()?
            .next()
            .context("RCON address unavailable")?;
        let mut stream =
            TcpStream::connect_timeout(&address, self.timeout).context("RCON connection failed")?;
        stream.set_read_timeout(Some(self.timeout))?;
        stream.set_write_timeout(Some(self.timeout))?;
        Self::send(&mut stream, 1, 3, &self.password)?;
        let mut authenticated = false;
        for _ in 0..3 {
            let (id, kind, _) = Self::packet(&mut stream)?;
            if id == -1 {
                bail!("RCON authentication failed");
            }
            if id == 1 && kind == 2 {
                authenticated = true;
                break;
            }
        }
        if !authenticated {
            bail!("Missing RCON authentication response");
        }
        Self::send(&mut stream, 2, 2, command)?;
        let (id, kind, text) = Self::packet(&mut stream)?;
        if id != 2 || kind != 0 {
            bail!("Unexpected RCON response");
        }
        Ok(text)
    }
    pub fn online(&self) -> Result<u32> {
        parse_online(&self.command("minecraft:list")?)
    }
}
pub fn parse_online(text: &str) -> Result<u32> {
    let mut clean = String::new();
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c == '§' {
            chars.next();
        } else {
            clean.push(c);
        }
    }
    let rest = clean
        .trim()
        .strip_prefix("There are ")
        .context("Unknown player count response")?;
    let (count, rest) = rest
        .split_once(" of a max of ")
        .context("Unknown player count response")?;
    let (max, _) = rest
        .split_once(" players online:")
        .context("Unknown player count response")?;
    let count: u32 = count.parse()?;
    let max: u32 = max.parse()?;
    if count > max {
        bail!("Invalid player count");
    }
    Ok(count)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fail_closed() {
        assert!(parse_online("unknown command").is_err());
        assert!(parse_online("There are 99 of a max of 20 players online:").is_err());
        assert_eq!(
            parse_online("There are 0 of a max of 20 players online:").unwrap(),
            0
        );
    }
}
