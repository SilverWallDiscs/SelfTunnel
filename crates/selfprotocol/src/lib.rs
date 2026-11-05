use std::io;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

const MAGIC: &[u8; 8] = b"SELFN001";
const MAX_TOKEN_LEN: usize = 4096;
const MIN_TOKEN_LEN: usize = 16;
const MAX_DATA_LEN: usize = 64 * 1024;
const MAX_FRAME_LEN: usize = MAX_DATA_LEN + 5;

#[derive(Debug)]
pub enum Frame {
    Open { id: u32 },
    OpenOk { id: u32 },
    OpenErr { id: u32, message: String },
    Data { id: u32, bytes: Vec<u8> },
    Close { id: u32 },
    Ping,
    Pong,
}

pub async fn send_auth<W>(writer: &mut W, token: &str) -> io::Result<()>
where
    W: AsyncWrite + Unpin,
{
    let token = token.as_bytes();
    if !(MIN_TOKEN_LEN..=MAX_TOKEN_LEN).contains(&token.len()) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "access token must contain between 16 and 4096 bytes",
        ));
    }

    writer.write_all(MAGIC).await?;
    writer.write_all(&(token.len() as u16).to_be_bytes()).await?;
    writer.write_all(token).await?;
    writer.flush().await
}

pub async fn read_auth<R>(reader: &mut R) -> io::Result<Vec<u8>>
where
    R: AsyncRead + Unpin,
{
    let mut magic = [0_u8; 8];
    reader.read_exact(&mut magic).await?;
    if &magic != MAGIC {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "invalid SelfTunnel handshake"));
    }

    let mut len = [0_u8; 2];
    reader.read_exact(&mut len).await?;
    let len = u16::from_be_bytes(len) as usize;
    if !(MIN_TOKEN_LEN..=MAX_TOKEN_LEN).contains(&len) {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "invalid token length"));
    }

    let mut token = vec![0_u8; len];
    reader.read_exact(&mut token).await?;
    Ok(token)
}

pub async fn send_auth_result<W>(writer: &mut W, accepted: bool) -> io::Result<()>
where
    W: AsyncWrite + Unpin,
{
    writer.write_all(&[u8::from(accepted)]).await?;
    writer.flush().await
}

pub async fn read_auth_result<R>(reader: &mut R) -> io::Result<bool>
where
    R: AsyncRead + Unpin,
{
    let mut result = [0_u8; 1];
    reader.read_exact(&mut result).await?;
    match result[0] {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(io::Error::new(io::ErrorKind::InvalidData, "invalid authentication response")),
    }
}

pub async fn write_frame<W>(writer: &mut W, frame: &Frame) -> io::Result<()>
where
    W: AsyncWrite + Unpin,
{
    let (kind, id, payload): (u8, u32, &[u8]) = match frame {
        Frame::Open { id } => (1, *id, &[]),
        Frame::OpenOk { id } => (2, *id, &[]),
        Frame::OpenErr { id, message } => (3, *id, message.as_bytes()),
        Frame::Data { id, bytes } => (4, *id, bytes),
        Frame::Close { id } => (5, *id, &[]),
        Frame::Ping => (6, 0, &[]),
        Frame::Pong => (7, 0, &[]),
    };

    if payload.len() > MAX_DATA_LEN {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "frame payload exceeds 64 KiB"));
    }

    let frame_len = 5 + payload.len();
    writer.write_all(&(frame_len as u32).to_be_bytes()).await?;
    writer.write_all(&[kind]).await?;
    writer.write_all(&id.to_be_bytes()).await?;
    writer.write_all(payload).await?;
    writer.flush().await
}

pub async fn read_frame<R>(reader: &mut R) -> io::Result<Frame>
where
    R: AsyncRead + Unpin,
{
    let mut len = [0_u8; 4];
    reader.read_exact(&mut len).await?;
    let len = u32::from_be_bytes(len) as usize;
    if !(5..=MAX_FRAME_LEN).contains(&len) {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "invalid frame length"));
    }

    let mut body = vec![0_u8; len];
    reader.read_exact(&mut body).await?;
    let kind = body[0];
    let id = u32::from_be_bytes([body[1], body[2], body[3], body[4]]);
    let payload = &body[5..];

    match kind {
        1 if payload.is_empty() => Ok(Frame::Open { id }),
        2 if payload.is_empty() => Ok(Frame::OpenOk { id }),
        3 => Ok(Frame::OpenErr {
            id,
            message: String::from_utf8_lossy(payload).into_owned(),
        }),
        4 => Ok(Frame::Data {
            id,
            bytes: payload.to_vec(),
        }),
        5 if payload.is_empty() => Ok(Frame::Close { id }),
        6 if id == 0 && payload.is_empty() => Ok(Frame::Ping),
        7 if id == 0 && payload.is_empty() => Ok(Frame::Pong),
        _ => Err(io::Error::new(io::ErrorKind::InvalidData, "unknown or malformed frame")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::duplex;

    #[tokio::test]
    async fn frame_round_trip() {
        let (mut left, mut right) = duplex(128);
        let outgoing = Frame::Data {
            id: 42,
            bytes: b"minecraft packet".to_vec(),
        };
        let write = tokio::spawn(async move {
            write_frame(&mut left, &outgoing).await.unwrap();
        });
        let received = read_frame(&mut right).await.unwrap();
        write.await.unwrap();

        match received {
            Frame::Data { id, bytes } => {
                assert_eq!(id, 42);
                assert_eq!(bytes, b"minecraft packet");
            }
            other => panic!("unexpected frame: {other:?}"),
        }
    }

    #[tokio::test]
    async fn authentication_round_trip() {
        let (mut left, mut right) = duplex(128);
        let client = tokio::spawn(async move {
            send_auth(&mut left, "0123456789abcdef0123456789abcdef").await.unwrap();
            assert!(read_auth_result(&mut left).await.unwrap());
        });

        let token = read_auth(&mut right).await.unwrap();
        assert_eq!(token, b"0123456789abcdef0123456789abcdef");
        send_auth_result(&mut right, true).await.unwrap();
        client.await.unwrap();
    }
}
