//! Incremental, content-addressed snapshots over the S3 HTTP API.
use crate::{Error, Result};
use hmac::{Hmac, Mac};
use reqwest::{Method, StatusCode, blocking::Client};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::Read,
    time::Duration,
};

const CHUNK_BYTES: usize = 64 * 1024;
const MAX_BYTES: usize = 256 * 1024 * 1024;
#[derive(Clone)]
pub struct Credentials {
    pub access_key_id: String,
    pub secret_access_key: String,
    pub session_token: Option<String>,
}
impl Credentials {
    pub fn load() -> Result<Self> {
        let key = std::env::var("AWS_ACCESS_KEY_ID").ok();
        let secret = std::env::var("AWS_SECRET_ACCESS_KEY").ok();
        match (key, secret) {
            (Some(access_key_id), Some(secret_access_key))
                if !access_key_id.is_empty() && !secret_access_key.is_empty() =>
            {
                return Ok(Self {
                    access_key_id,
                    secret_access_key,
                    session_token: std::env::var("AWS_SESSION_TOKEN").ok(),
                });
            }
            (None, None) => (),
            _ => {
                return Err(Error(
                    "set both AWS_ACCESS_KEY_ID and AWS_SECRET_ACCESS_KEY".into(),
                ));
            }
        }
        let path = std::env::var_os("AWS_SHARED_CREDENTIALS_FILE")
            .map(std::path::PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME")
                    .map(|home| std::path::PathBuf::from(home).join(".aws/credentials"))
            })
            .ok_or_else(|| Error("AWS credentials are not configured".into()))?;
        Self::from_profile(
            path,
            &std::env::var("AWS_PROFILE").unwrap_or_else(|_| "default".into()),
        )
    }
    pub fn from_profile(path: impl AsRef<std::path::Path>, profile: &str) -> Result<Self> {
        let text = std::fs::read_to_string(path).map_err(|_| {
            Error(
                "AWS credentials file unavailable; set AWS_ACCESS_KEY_ID and AWS_SECRET_ACCESS_KEY"
                    .into(),
            )
        })?;
        let mut selected = false;
        let mut values = BTreeMap::new();
        for line in text.lines().map(str::trim) {
            if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
                continue;
            }
            if line.starts_with('[') {
                selected =
                    line.strip_prefix('[').and_then(|s| s.strip_suffix(']')) == Some(profile);
                continue;
            }
            if selected {
                let (key, value) = line
                    .split_once('=')
                    .ok_or_else(|| Error("invalid AWS credentials profile".into()))?;
                if values
                    .insert(key.trim().to_lowercase(), value.trim().to_string())
                    .is_some()
                {
                    return Err(Error("duplicate AWS profile field".into()));
                }
            }
        }
        let mut field = |name: &str| {
            values
                .remove(name)
                .filter(|v| !v.is_empty())
                .ok_or_else(|| {
                    Error(format!(
                        "AWS profile lacks {name}; only static access-key profiles are supported"
                    ))
                })
        };
        let access_key_id = field("aws_access_key_id")?;
        let secret_access_key = field("aws_secret_access_key")?;
        Ok(Self {
            access_key_id,
            secret_access_key,
            session_token: values.remove("aws_session_token"),
        })
    }
}
impl std::fmt::Debug for Credentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Credentials")
            .field("access_key_id", &self.access_key_id)
            .field("secret_access_key", &"[redacted]")
            .field(
                "session_token",
                &self.session_token.as_ref().map(|_| "[redacted]"),
            )
            .finish()
    }
}
#[derive(Clone, Debug)]
pub struct S3Config {
    pub endpoint: String,
    pub region: String,
    pub bucket: String,
    pub prefix: String,
    pub credentials: Credentials,
}
#[derive(Debug, Clone, Serialize)]
pub struct Revision {
    pub id: String,
    pub bytes: u64,
    pub uploaded_chunks: usize,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    version: u32,
    revision: String,
    bytes: usize,
    chunks: Vec<String>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Pointer {
    version: u32,
    revision: String,
}
pub struct Replicator {
    config: S3Config,
    client: Client,
}
fn error(e: impl std::fmt::Display) -> Error {
    Error(e.to_string())
}
fn hash(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}
fn valid_hash(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn mac(key: &[u8], value: &[u8]) -> Vec<u8> {
    let mut h = Hmac::<Sha256>::new_from_slice(key).expect("HMAC accepts any key length");
    h.update(value);
    h.finalize().into_bytes().to_vec()
}
fn encode_path(s: &str) -> String {
    let mut result = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.~/".contains(&b) {
            result.push(b as char);
        } else {
            result.push_str(&format!("%{b:02X}"));
        }
    }
    result
}
impl Replicator {
    pub fn new(config: S3Config) -> Result<Self> {
        let endpoint = reqwest::Url::parse(&config.endpoint).map_err(error)?;
        if !matches!(endpoint.scheme(), "http" | "https")
            || !endpoint.username().is_empty()
            || endpoint.password().is_some()
            || endpoint.query().is_some()
            || endpoint.fragment().is_some()
        {
            return Err(Error(
                "endpoint must be an HTTP(S) URL without credentials, query or fragment".into(),
            ));
        }
        if config.bucket.is_empty()
            || !config
                .bucket
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b".-".contains(&b))
            || config.region.is_empty()
            || !config
                .region
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        {
            return Err(Error("invalid S3 bucket or region".into()));
        }
        if config
            .prefix
            .split('/')
            .any(|part| matches!(part, "." | ".."))
        {
            return Err(Error("S3 prefix cannot contain dot path segments".into()));
        }
        if config.credentials.access_key_id.is_empty()
            || config.credentials.secret_access_key.is_empty()
        {
            return Err(Error("S3 credentials are empty".into()));
        }
        let client = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(30))
            .connect_timeout(Duration::from_secs(5))
            .build()
            .map_err(error)?;
        Ok(Self { config, client })
    }
    fn request(
        &self,
        method: Method,
        key: &str,
        bytes: &[u8],
        condition: Option<(&str, &str)>,
    ) -> Result<reqwest::blocking::Response> {
        let c = &self.config;
        let key = if c.prefix.trim_matches('/').is_empty() {
            key.to_string()
        } else {
            format!("{}/{key}", c.prefix.trim_matches('/'))
        };
        let url = reqwest::Url::parse(&format!(
            "{}/{}/{}",
            c.endpoint.trim_end_matches('/'),
            encode_path(&c.bucket),
            encode_path(&key)
        ))
        .map_err(error)?;
        let now = chrono::Utc::now();
        let date = now.format("%Y%m%d").to_string();
        let timestamp = now.format("%Y%m%dT%H%M%SZ").to_string();
        let host = match url.port() {
            Some(port) => format!("{}:{port}", url.host_str().unwrap()),
            None => url.host_str().unwrap().to_string(),
        };
        let payload_hash = hash(bytes);
        let mut headers = BTreeMap::from([
            ("host".to_string(), host),
            ("x-amz-date".into(), timestamp.clone()),
            ("x-amz-content-sha256".into(), payload_hash.clone()),
        ]);
        if let Some(token) = &c.credentials.session_token {
            headers.insert("x-amz-security-token".into(), token.clone());
        }
        if let Some((k, v)) = condition {
            headers.insert(k.into(), v.into());
        }
        let canonical_headers = headers
            .iter()
            .map(|(k, v)| {
                format!(
                    "{k}:{}\n",
                    v.split_whitespace().collect::<Vec<_>>().join(" ")
                )
            })
            .collect::<String>();
        let signed_headers = headers.keys().cloned().collect::<Vec<_>>().join(";");
        let canonical = format!(
            "{}\n{}\n\n{}\n{}\n{}",
            method.as_str(),
            url.path(),
            canonical_headers,
            signed_headers,
            payload_hash
        );
        let scope = format!("{date}/{}/s3/aws4_request", c.region);
        let to_sign = format!(
            "AWS4-HMAC-SHA256\n{timestamp}\n{scope}\n{}",
            hash(canonical.as_bytes())
        );
        let signing = mac(
            &mac(
                &mac(
                    &mac(
                        format!("AWS4{}", c.credentials.secret_access_key).as_bytes(),
                        date.as_bytes(),
                    ),
                    c.region.as_bytes(),
                ),
                b"s3",
            ),
            b"aws4_request",
        );
        let authorization = format!(
            "AWS4-HMAC-SHA256 Credential={}/{scope}, SignedHeaders={signed_headers}, Signature={}",
            c.credentials.access_key_id,
            hex::encode(mac(&signing, to_sign.as_bytes()))
        );
        let mut request = self
            .client
            .request(method, url)
            .header("authorization", authorization)
            .body(bytes.to_vec());
        for (k, v) in headers {
            request = request.header(k, v);
        }
        request.send().map_err(error)
    }
    fn get(&self, key: &str, limit: usize) -> Result<Option<(Vec<u8>, String)>> {
        let response = self.request(Method::GET, key, &[], None)?;
        if response.status() == StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if !response.status().is_success() {
            return Err(Error(format!("S3 GET {key}: {}", response.status())));
        }
        let etag = response
            .headers()
            .get("etag")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        let mut bytes = vec![];
        response
            .take(limit as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(error)?;
        if bytes.len() > limit {
            return Err(Error("S3 object exceeds size limit".into()));
        }
        Ok(Some((bytes, etag)))
    }
    fn immutable(&self, key: &str, bytes: &[u8]) -> Result<bool> {
        let response = self.request(Method::PUT, key, bytes, Some(("if-none-match", "*")))?;
        if response.status().is_success() {
            return Ok(true);
        }
        if response.status() == StatusCode::PRECONDITION_FAILED {
            let existing = self
                .get(key, bytes.len())?
                .ok_or_else(|| Error("S3 immutable object disappeared".into()))?;
            if existing.0 != bytes {
                return Err(Error("S3 immutable object integrity mismatch".into()));
            }
            return Ok(false);
        }
        Err(Error(format!("S3 PUT {key}: {}", response.status())))
    }
    fn latest(&self) -> Result<Option<(Pointer, String)>> {
        let Some((bytes, etag)) = self.get("latest.json", 4096)? else {
            return Ok(None);
        };
        let pointer: Pointer = serde_json::from_slice(&bytes).map_err(error)?;
        if pointer.version != 1 || !valid_hash(&pointer.revision) || etag.is_empty() {
            return Err(Error("invalid S3 latest pointer".into()));
        }
        Ok(Some((pointer, etag)))
    }
    fn manifest(&self, id: &str) -> Result<Manifest> {
        if !valid_hash(id) {
            return Err(Error("invalid revision id".into()));
        }
        let (bytes, _) = self
            .get(&format!("revisions/{id}.json"), 1024 * 1024)?
            .ok_or_else(|| Error("S3 revision not found".into()))?;
        let manifest: Manifest = serde_json::from_slice(&bytes).map_err(error)?;
        if manifest.version != 1
            || manifest.revision != id
            || manifest.bytes > MAX_BYTES
            || manifest.chunks.len() != manifest.bytes.div_ceil(CHUNK_BYTES)
            || !manifest.chunks.iter().all(|s| valid_hash(s))
        {
            return Err(Error("invalid S3 revision manifest".into()));
        }
        Ok(manifest)
    }
    pub fn push(&self, bytes: &[u8]) -> Result<Revision> {
        if bytes.len() > MAX_BYTES {
            return Err(Error("snapshot exceeds 256 MiB limit".into()));
        }
        let id = hash(bytes);
        let latest = self.latest()?;
        if latest.as_ref().is_some_and(|(p, _)| p.revision == id) {
            return Ok(Revision {
                id,
                bytes: bytes.len() as u64,
                uploaded_chunks: 0,
            });
        }
        let mut known = if let Some((p, _)) = &latest {
            self.manifest(&p.revision)?
                .chunks
                .into_iter()
                .collect::<BTreeSet<_>>()
        } else {
            BTreeSet::new()
        };
        let mut chunks = vec![];
        let mut uploaded_chunks = 0;
        for chunk in bytes.chunks(CHUNK_BYTES) {
            let chunk_id = hash(chunk);
            chunks.push(chunk_id.clone());
            if known.insert(chunk_id.clone())
                && self.immutable(&format!("chunks/{chunk_id}"), chunk)?
            {
                uploaded_chunks += 1;
            }
        }
        let manifest = Manifest {
            version: 1,
            revision: id.clone(),
            bytes: bytes.len(),
            chunks,
        };
        self.immutable(
            &format!("revisions/{id}.json"),
            &serde_json::to_vec(&manifest).map_err(error)?,
        )?;
        let pointer = serde_json::to_vec(&Pointer {
            version: 1,
            revision: id.clone(),
        })
        .map_err(error)?;
        let condition = latest
            .as_ref()
            .map(|(_, etag)| ("if-match", etag.as_str()))
            .unwrap_or(("if-none-match", "*"));
        let response = self.request(Method::PUT, "latest.json", &pointer, Some(condition))?;
        if !response.status().is_success() {
            return Err(Error(format!(
                "S3 latest publication failed (possible concurrent publisher): {}",
                response.status()
            )));
        }
        Ok(Revision {
            id,
            bytes: bytes.len() as u64,
            uploaded_chunks,
        })
    }
    pub fn restore(&self, revision: Option<&str>) -> Result<Vec<u8>> {
        let id = if let Some(id) = revision {
            id.to_string()
        } else {
            self.latest()?
                .ok_or_else(|| Error("S3 database not found".into()))?
                .0
                .revision
        };
        let manifest = self.manifest(&id)?;
        let mut bytes = Vec::with_capacity(manifest.bytes);
        for id in manifest.chunks {
            let (chunk, _) = self
                .get(&format!("chunks/{id}"), CHUNK_BYTES)?
                .ok_or_else(|| Error("S3 chunk not found".into()))?;
            if hash(&chunk) != id {
                return Err(Error("S3 chunk checksum mismatch".into()));
            }
            bytes.extend(chunk);
            if bytes.len() > manifest.bytes {
                return Err(Error("S3 restored size mismatch".into()));
            }
        }
        if bytes.len() != manifest.bytes || hash(&bytes) != manifest.revision {
            return Err(Error("S3 snapshot checksum mismatch".into()));
        }
        Ok(bytes)
    }
}
