use std::io::Read;

use anyhow::{anyhow, Context, Result};

pub struct HttpResponse {
    pub status: u16,
    pub body: Vec<u8>,
    pub content_type: Option<String>,
    pub content_length: Option<u64>,
    pub next_challenge: Option<String>,
}

pub struct Transport {
    agent: ureq::Agent,
    origin: String,
}

impl Transport {
    pub fn new(agent: ureq::Agent, origin: String) -> Self {
        Self { agent, origin }
    }

    /// The pinned origin every request goes to, for composing the one URL
    /// this receiver ever hands anything else: a ticketed playlist.
    pub fn origin(&self) -> &str {
        &self.origin
    }

    pub fn get(
        &self,
        path: &str,
        headers: &[(String, String)],
        maximum_bytes: usize,
    ) -> Result<HttpResponse> {
        let request = self.apply_headers(self.agent.get(&self.url(path)), headers);
        let response = match request.call() {
            Ok(response) | Err(ureq::Error::Status(_, response)) => response,
            Err(error) => return Err(anyhow!(error)).context("send display GET request"),
        };
        Self::read_response(response, maximum_bytes)
    }

    pub fn post(
        &self,
        path: &str,
        headers: &[(String, String)],
        body: &[u8],
        maximum_bytes: usize,
    ) -> Result<HttpResponse> {
        let request = self.apply_headers(self.agent.post(&self.url(path)), headers);
        let response = match request
            .set("Content-Type", "application/json")
            .send_bytes(body)
        {
            Ok(response) | Err(ureq::Error::Status(_, response)) => response,
            Err(error) => return Err(anyhow!(error)).context("send display POST request"),
        };
        Self::read_response(response, maximum_bytes)
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.origin)
    }

    fn apply_headers<'a>(
        &self,
        mut request: ureq::Request,
        headers: &'a [(String, String)],
    ) -> ureq::Request {
        for (name, value) in headers {
            request = request.set(name, value);
        }
        request
    }

    fn read_response(response: ureq::Response, maximum_bytes: usize) -> Result<HttpResponse> {
        let status = response.status();
        let content_type = response.header("Content-Type").map(str::to_owned);
        let content_length = response
            .header("Content-Length")
            .map(str::parse::<u64>)
            .transpose()
            .context("parse display Content-Length")?;
        let next_challenge = response
            .header(display_protocol::auth::HEADER_NEXT_CHALLENGE)
            .map(str::to_owned);
        if content_length.is_some_and(|length| {
            usize::try_from(length).map_or(true, |length| length > maximum_bytes)
        }) {
            return Err(anyhow!("display response exceeds its declared byte bound"));
        }
        let read_limit = maximum_bytes
            .checked_add(1)
            .ok_or_else(|| anyhow!("display response byte bound overflow"))?;
        let read_limit =
            u64::try_from(read_limit).context("convert display response byte bound")?;
        let mut body = Vec::new();
        response
            .into_reader()
            .take(read_limit)
            .read_to_end(&mut body)
            .context("read bounded display response")?;
        if body.len() > maximum_bytes {
            return Err(anyhow!("display response exceeds its byte bound"));
        }
        Ok(HttpResponse {
            status,
            body,
            content_type,
            content_length,
            next_challenge,
        })
    }
}
