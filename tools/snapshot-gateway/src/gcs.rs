//! The GCS binding of [`ObjectStore`]: a public unconditional read and a
//! generation-matched conditional write, over the JSON API with a
//! metadata-server access token. Pure-Rust TLS via `ureq`, no C dependency.
//!
//! The conditional write is the whole point — GCS `ifGenerationMatch` is the
//! atomic compare-and-set the no-lost-update guarantee rests on. A 412 from GCS
//! is a real concurrent writer, surfaced as [`PutError::Conflict`].

use lait_snapshot_gateway::{ObjectStore, PutError, Stored};

/// A GCS bucket the gateway writes snapshot objects into.
pub struct GcsStore {
    bucket: String,
    token: Box<dyn Fn() -> Result<String, String> + Send + Sync>,
    agent: ureq::Agent,
}

impl GcsStore {
    /// A store over `bucket`, minting write tokens from `token` (the Cloud Run
    /// metadata server in production; a fixed token in a smoke test).
    pub fn new(
        bucket: impl Into<String>,
        token: impl Fn() -> Result<String, String> + Send + Sync + 'static,
    ) -> Self {
        Self {
            bucket: bucket.into(),
            token: Box::new(token),
            agent: ureq::AgentBuilder::new()
                .timeout(std::time::Duration::from_secs(30))
                .build(),
        }
    }

    fn object_url(&self, key: &str, upload: bool) -> String {
        let object = urlencode(key);
        if upload {
            format!(
                "https://storage.googleapis.com/upload/storage/v1/b/{}/o?uploadType=media&name={object}",
                self.bucket
            )
        } else {
            format!(
                "https://storage.googleapis.com/storage/v1/b/{}/o/{object}?alt=media",
                self.bucket
            )
        }
    }

    fn metadata_url(&self, key: &str) -> String {
        format!(
            "https://storage.googleapis.com/storage/v1/b/{}/o/{}",
            self.bucket,
            urlencode(key)
        )
    }
}

impl ObjectStore for GcsStore {
    fn read(&self, key: &str) -> Result<Option<Stored>, String> {
        // The generation lives in object metadata; the bytes in a media read.
        // Two calls, but the read path is cold (a client re-reads only on
        // conflict) and the alternative — parsing a multipart response — is
        // more code for no real saving.
        let meta = self.agent.get(&self.metadata_url(key)).call();
        let generation = match meta {
            Ok(response) => {
                let json: serde_json::Value = response
                    .into_json()
                    .map_err(|e| format!("object metadata is not JSON: {e}"))?;
                json.get("generation")
                    .and_then(|g| g.as_str())
                    .and_then(|g| g.parse::<u64>().ok())
                    .ok_or_else(|| "object metadata carries no generation".to_string())?
            }
            Err(ureq::Error::Status(404, _)) => return Ok(None),
            Err(e) => return Err(format!("read object metadata: {e}")),
        };
        let bytes = match self.agent.get(&self.object_url(key, false)).call() {
            Ok(response) => {
                let mut buf = Vec::new();
                response
                    .into_reader()
                    .read_to_end(&mut buf)
                    .map_err(|e| format!("read object body: {e}"))?;
                buf
            }
            Err(ureq::Error::Status(404, _)) => return Ok(None),
            Err(e) => return Err(format!("read object body: {e}")),
        };
        Ok(Some(Stored { bytes, generation }))
    }

    fn put_if_generation(
        &self,
        key: &str,
        bytes: &[u8],
        expected_generation: u64,
    ) -> Result<u64, PutError> {
        let token = (self.token)().map_err(PutError::Store)?;
        let url = format!(
            "{}&ifGenerationMatch={expected_generation}",
            self.object_url(key, true)
        );
        let response = self
            .agent
            .post(&url)
            .set("Authorization", &format!("Bearer {token}"))
            .set("Content-Type", "application/octet-stream")
            .send_bytes(bytes);
        match response {
            Ok(response) => {
                let json: serde_json::Value = response
                    .into_json()
                    .map_err(|e| PutError::Store(format!("upload response is not JSON: {e}")))?;
                json.get("generation")
                    .and_then(|g: &serde_json::Value| g.as_str())
                    .and_then(|g| g.parse::<u64>().ok())
                    .ok_or_else(|| PutError::Store("upload response carries no generation".into()))
            }
            // 412 precondition failed: a concurrent writer moved the generation.
            // Re-read for the current one so the caller can retry against it.
            Err(ureq::Error::Status(412, _)) => {
                let current = self
                    .read(key)
                    .ok()
                    .flatten()
                    .map(|s| s.generation)
                    .unwrap_or(0);
                Err(PutError::Conflict { current })
            }
            Err(e) => Err(PutError::Store(format!("conditional upload: {e}"))),
        }
    }
}

/// The Cloud Run access token, minted from the instance metadata server. The
/// gateway's service account needs `storage.objects.create` on the bucket.
pub fn metadata_token() -> Result<String, String> {
    let json: serde_json::Value = ureq::get(
        "http://metadata.google.internal/computeMetadata/v1/instance/service-accounts/default/token",
    )
    .set("Metadata-Flavor", "Google")
    .call()
    .map_err(|e| format!("metadata token: {e}"))?
    .into_json()
    .map_err(|e| format!("metadata token is not JSON: {e}"))?;
    json.get("access_token")
        .and_then(|t| t.as_str())
        .map(|t| t.to_string())
        .ok_or_else(|| "metadata token response carries no access_token".to_string())
}

/// Percent-encode a GCS object name for a URL path segment (GCS wants the
/// slashes in an object name encoded when it is the `{object}` path parameter).
fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for byte in s.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

use std::io::Read as _;
