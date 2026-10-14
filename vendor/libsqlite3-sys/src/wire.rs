//! Single-byte BLOB storage tags. Codec versions belong to database metadata.
pub const VERSION: i64 = 1;
pub const BYTEA_TAG: u8 = b'b';

pub fn blob_payload(bytes: &[u8]) -> &[u8] {
    bytes.strip_prefix(&[BYTEA_TAG]).unwrap_or(bytes)
}

pub fn encode_blob(bytes: &[u8]) -> Vec<u8> {
    let mut encoded = Vec::with_capacity(bytes.len() + 1);
    encoded.push(BYTEA_TAG);
    encoded.extend_from_slice(bytes);
    encoded
}

pub fn decode_blob(mut bytes: Vec<u8>) -> Vec<u8> {
    if bytes.first() == Some(&BYTEA_TAG) {
        bytes.remove(0);
    }
    bytes
}
