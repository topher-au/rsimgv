//! Hashes of strings, such as the names of the files in the app's caches.

/// The XXH3-64 hash of `text`, as 16 lowercase hex digits.
pub fn string_hash(text: &str) -> String {
    format!("{:016x}", xxhash_rust::xxh3::xxh3_64(text.as_bytes()))
}
