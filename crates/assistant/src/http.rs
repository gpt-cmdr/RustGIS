//! Sending requests on the desktop and the command line (blocking: call it off the UI thread).

use crate::provider::{HttpRequest, HttpResponse, allowed_url, reply_limit};

/// Sends `req` and reads the response, whatever its status.
pub fn send(req: &HttpRequest) -> Result<HttpResponse, String> {
    send_timeout(req, std::time::Duration::from_secs(600))
}

/// Sends `req`, handing each piece of the response body to `on_chunk` as it arrives (for a
/// streamed reply's live text), and reads the whole response.
pub fn send_streaming(req: &HttpRequest, on_chunk: &mut dyn FnMut(&[u8])) -> Result<HttpResponse, String> {
    send_with(req, std::time::Duration::from_secs(600), Some(on_chunk))
}

/// Sends with an overall deadline, including reading the response body.
pub fn send_timeout(req: &HttpRequest, timeout: std::time::Duration) -> Result<HttpResponse, String> {
    send_with(req, timeout, None)
}

fn send_with(req: &HttpRequest, timeout: std::time::Duration, mut on_chunk: Option<&mut dyn FnMut(&[u8])>) -> Result<HttpResponse, String> {
    // The address checked is the one used.
    let url = req.url.trim();
    if !allowed_url(url) {
        return Err(format!("{url}: only HTTPS addresses, or plain HTTP on this computer or network, are allowed"));
    }
    let agent: ureq::Agent = ureq::Agent::config_builder()
        // Models can think for minutes on a hard request.
        .timeout_global(Some(timeout))
        .http_status_as_error(false)
        // An API key is never sent on to another address.
        .max_redirects(0)
        .build()
        .into();
    let ua = concat!("RustGIS/", env!("CARGO_PKG_VERSION"));
    let result = match (req.method, &req.body) {
        ("POST", body) => {
            let mut b = agent.post(url).header("User-Agent", ua);
            for (k, v) in &req.headers {
                b = b.header(k.as_str(), v.as_str());
            }
            b.send(body.as_deref().unwrap_or(""))
        }
        _ => {
            let mut b = agent.get(url).header("User-Agent", ua);
            for (k, v) in &req.headers {
                b = b.header(k.as_str(), v.as_str());
            }
            b.call()
        }
    };
    let mut resp = result.map_err(|e| format!("couldn't reach {}: {e}", host(url)))?;
    let status = resp.status().as_u16();
    let headers =
        resp.headers().iter().filter_map(|(k, v)| Some((k.as_str().to_ascii_lowercase(), v.to_str().ok()?.to_string()))).collect();
    let limit = reply_limit(req);
    let mut reader = resp.body_mut().with_config().limit(limit as u64).reader();
    let mut body = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        let n = std::io::Read::read(&mut reader, &mut chunk).map_err(|e| format!("reading the reply: {e}"))?;
        let Some(piece) = chunk.get(..n).filter(|p| !p.is_empty()) else { break };
        if let Some(f) = on_chunk.as_mut() {
            f(piece);
        }
        body.extend_from_slice(piece);
    }
    Ok(HttpResponse { status, body, headers })
}

/// The scheme and host of a URL (errors never repeat a path that might carry a key).
fn host(url: &str) -> String {
    let (scheme, rest) = url.split_once("://").unwrap_or(("", url));
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    // Never the user name or password.
    format!("{scheme}://{}", authority.rsplit('@').next().unwrap_or(""))
}
