//! Canonical binary transcript primitives.

use crate::ProtocolError;

pub(crate) struct Transcript {
    bytes: Vec<u8>,
}

impl Transcript {
    pub(crate) fn new(domain: &'static [u8]) -> Result<Self, ProtocolError> {
        let mut transcript = Self { bytes: Vec::new() };
        transcript.field(domain)?;
        Ok(transcript)
    }

    pub(crate) fn field(&mut self, value: &[u8]) -> Result<(), ProtocolError> {
        let length = u32::try_from(value.len())
            .map_err(|_| ProtocolError::BoundExceeded("transcript field"))?;
        self.bytes.extend_from_slice(&length.to_be_bytes());
        self.bytes.extend_from_slice(value);
        Ok(())
    }

    pub(crate) fn text(&mut self, value: &str) -> Result<(), ProtocolError> {
        self.field(value.as_bytes())
    }

    pub(crate) fn optional_text(&mut self, value: Option<&str>) -> Result<(), ProtocolError> {
        match value {
            Some(value) => self.text(value),
            None => self.field(&[]),
        }
    }

    pub(crate) fn u32(&mut self, value: u32) -> Result<(), ProtocolError> {
        self.field(&value.to_be_bytes())
    }

    pub(crate) fn optional_u32(&mut self, value: Option<u32>) -> Result<(), ProtocolError> {
        match value {
            Some(value) => self.u32(value),
            None => self.field(&[]),
        }
    }

    pub(crate) fn optional_u64(&mut self, value: Option<u64>) -> Result<(), ProtocolError> {
        match value {
            Some(value) => self.field(&value.to_be_bytes()),
            None => self.field(&[]),
        }
    }

    pub(crate) fn boolean(&mut self, value: bool) -> Result<(), ProtocolError> {
        self.field(&[u8::from(value)])
    }

    pub(crate) fn finish(self) -> Vec<u8> {
        self.bytes
    }
}
