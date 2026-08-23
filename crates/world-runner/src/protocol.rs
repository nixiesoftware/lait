use std::io::{Read, Write};
use std::net::SocketAddr;

use serde::{de::DeserializeOwned, Deserialize, Serialize};

pub const PROTOCOL_VERSION: u32 = 1;
pub const MAX_FRAME_BYTES: usize = 64 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ready {
    pub protocol: u32,
    pub world: String,
    pub version: String,
    pub address: SocketAddr,
    pub token: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServiceDescriptor {
    pub world: String,
    pub implementation: [u8; 32],
    pub implementation_version: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Request {
    pub protocol: u32,
    pub token: String,
    pub id: u64,
    pub operation: Operation,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Operation {
    Ping,
    Describe,
    Stop,
    Call { operation: String, payload: Vec<u8> },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum Response {
    Complete {
        id: u64,
        outcome: Result<Reply, String>,
    },
    Callback {
        id: u64,
        callback: u64,
        operation: String,
        payload: Vec<u8>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct CallbackResponse {
    pub protocol: u32,
    pub token: String,
    pub id: u64,
    pub callback: u64,
    pub outcome: Result<Vec<u8>, String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Reply {
    Pong,
    Descriptor(ServiceDescriptor),
    Stopping,
    Call { payload: Vec<u8> },
}

pub fn encode_frame<T: Serialize>(value: &T) -> std::io::Result<Vec<u8>> {
    let payload = postcard::to_stdvec(value)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
    if payload.len() > MAX_FRAME_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "World runner frame exceeds its bound",
        ));
    }
    let len = u32::try_from(payload.len()).map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "World runner frame length is not representable",
        )
    })?;
    let mut frame = Vec::with_capacity(payload.len().saturating_add(4));
    frame.extend_from_slice(&len.to_be_bytes());
    frame.extend_from_slice(&payload);
    Ok(frame)
}

pub fn decode_frame<T: DeserializeOwned>(frame: &[u8]) -> std::io::Result<T> {
    if frame.len() > MAX_FRAME_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "World runner frame exceeds its bound",
        ));
    }
    postcard::from_bytes(frame)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))
}

pub fn write_frame<T: Serialize>(writer: &mut impl Write, value: &T) -> std::io::Result<()> {
    let frame = encode_frame(value)?;
    writer.write_all(&frame)
}

pub fn read_frame<T: DeserializeOwned>(reader: &mut impl Read) -> std::io::Result<T> {
    let mut length = [0_u8; 4];
    reader.read_exact(&mut length)?;
    let length = usize::try_from(u32::from_be_bytes(length)).map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "World runner frame length is not representable",
        )
    })?;
    if length > MAX_FRAME_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "World runner frame exceeds its bound",
        ));
    }
    let mut payload = vec![0; length];
    reader.read_exact(&mut payload)?;
    decode_frame(&payload)
}
