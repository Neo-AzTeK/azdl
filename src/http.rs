//! Shared in-process HTTPS client for small JSON metadata requests.
//!
//! One pooled client replaces a `curl` process per lookup, so repeated Store and
//! appinfo requests reuse TLS connections. Large or resumable file transfers
//! keep their dedicated download paths.

use crate::Result;
use std::io::Read;
use std::sync::OnceLock;
use std::time::Duration;

const MAX_REDIRECTS: usize = 5;

fn client() -> Result<&'static reqwest::blocking::Client> {
    static CLIENT: OnceLock<std::result::Result<reqwest::blocking::Client, String>> =
        OnceLock::new();
    CLIENT
        .get_or_init(|| {
            reqwest::blocking::Client::builder()
                .https_only(true)
                .redirect(reqwest::redirect::Policy::limited(MAX_REDIRECTS))
                .user_agent(concat!("azdl/", env!("CARGO_PKG_VERSION")))
                .build()
                .map_err(|error| format!("Cannot create HTTPS client: {error}"))
        })
        .as_ref()
        .map_err(|error| error.clone().into())
}

/// Fetch an HTTPS resource, rejecting failures, non-success statuses and bodies over `max_bytes`.
///
/// Must not be called from inside an async runtime worker; use a blocking thread.
pub(crate) fn get_bytes(url: &str, timeout: Duration, max_bytes: u64) -> Result<Vec<u8>> {
    let response = client()?
        .get(url)
        .timeout(timeout)
        .send()?
        .error_for_status()?;
    if response
        .content_length()
        .is_some_and(|length| length > max_bytes)
    {
        return Err(format!("Response from {url} exceeds {max_bytes} bytes").into());
    }
    let mut body = Vec::new();
    response.take(max_bytes + 1).read_to_end(&mut body)?;
    if body.len() as u64 > max_bytes {
        return Err(format!("Response from {url} exceeds {max_bytes} bytes").into());
    }
    Ok(body)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_http_is_rejected_before_any_request() {
        let error = get_bytes("http://127.0.0.1:9/", Duration::from_secs(1), 16).unwrap_err();
        assert!(!error.to_string().is_empty());
    }
}
