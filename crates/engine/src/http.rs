//! One-shot HTTPS GET through WinHTTP. No TLS or HTTP crate: the OS stack is
//! already in every WA process and follows redirects on its own.

use std::ffi::c_void;
use windows_sys::Win32::Networking::WinHttp::{
    WinHttpCloseHandle, WinHttpConnect, WinHttpOpen, WinHttpOpenRequest,
    WinHttpQueryDataAvailable, WinHttpQueryHeaders, WinHttpReadData, WinHttpReceiveResponse,
    WinHttpSendRequest, INTERNET_DEFAULT_HTTPS_PORT, INTERNET_DEFAULT_HTTP_PORT,
    WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, WINHTTP_ACCESS_TYPE_DEFAULT_PROXY,
    WINHTTP_FLAG_SECURE, WINHTTP_QUERY_CONTENT_LENGTH, WINHTTP_QUERY_FLAG_NUMBER,
    WINHTTP_QUERY_STATUS_CODE,
};

fn wide(s: &str) -> Vec<u16> {
    let mut v: Vec<u16> = s.encode_utf16().collect();
    v.push(0);
    v
}

struct Handle(*mut c_void);
impl Drop for Handle {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe {
                WinHttpCloseHandle(self.0);
            }
        }
    }
}

fn last_error(what: &str) -> String {
    let code = unsafe { windows_sys::Win32::Foundation::GetLastError() };
    format!("{what} failed (Win32 error {code})")
}

/// Split `https://host[:port]/path` into (host, port, secure, path).
fn parse_url(url: &str) -> Result<(String, u16, bool, String), String> {
    let (scheme, rest) = url
        .split_once("://")
        .ok_or_else(|| format!("bad URL {url}"))?;
    let secure = match scheme {
        "https" => true,
        "http" => false,
        _ => return Err(format!("unsupported scheme in {url}")),
    };
    let (host_port, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    let (host, port) = match host_port.split_once(':') {
        Some((h, p)) => (h, p.parse::<u16>().map_err(|_| format!("bad port in {url}"))?),
        None => (
            host_port,
            if secure { INTERNET_DEFAULT_HTTPS_PORT } else { INTERNET_DEFAULT_HTTP_PORT },
        ),
    };
    Ok((host.to_string(), port, secure, path.to_string()))
}

/// Download `url` into memory. `progress(received, total)` runs after every
/// chunk; `total` is 0 when the server sent no Content-Length.
pub fn get(url: &str, mut progress: impl FnMut(usize, usize)) -> Result<Vec<u8>, String> {
    let (host, port, secure, path) = parse_url(url)?;
    unsafe {
        let agent = wide("WormForge/1.0");
        let mut session = WinHttpOpen(
            agent.as_ptr(),
            WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY,
            std::ptr::null(),
            std::ptr::null(),
            0,
        );
        if session.is_null() {
            session = WinHttpOpen(
                agent.as_ptr(),
                WINHTTP_ACCESS_TYPE_DEFAULT_PROXY,
                std::ptr::null(),
                std::ptr::null(),
                0,
            );
        }
        let session = Handle(session);
        if session.0.is_null() {
            return Err(last_error("WinHttpOpen"));
        }
        let whost = wide(&host);
        let connect = Handle(WinHttpConnect(session.0, whost.as_ptr(), port, 0));
        if connect.0.is_null() {
            return Err(last_error("WinHttpConnect"));
        }
        let verb = wide("GET");
        let wpath = wide(&path);
        let request = Handle(WinHttpOpenRequest(
            connect.0,
            verb.as_ptr(),
            wpath.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            std::ptr::null(),
            if secure { WINHTTP_FLAG_SECURE } else { 0 },
        ));
        if request.0.is_null() {
            return Err(last_error("WinHttpOpenRequest"));
        }
        if WinHttpSendRequest(request.0, std::ptr::null(), 0, std::ptr::null(), 0, 0, 0) == 0 {
            return Err(last_error("WinHttpSendRequest"));
        }
        if WinHttpReceiveResponse(request.0, std::ptr::null_mut()) == 0 {
            return Err(last_error("WinHttpReceiveResponse"));
        }
        let mut status: u32 = 0;
        let mut len = std::mem::size_of::<u32>() as u32;
        WinHttpQueryHeaders(
            request.0,
            WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
            std::ptr::null(),
            &mut status as *mut u32 as *mut c_void,
            &mut len,
            std::ptr::null_mut(),
        );
        if status != 200 {
            return Err(format!("HTTP {status} for {url}"));
        }
        let mut total: u32 = 0;
        let mut len = std::mem::size_of::<u32>() as u32;
        if WinHttpQueryHeaders(
            request.0,
            WINHTTP_QUERY_CONTENT_LENGTH | WINHTTP_QUERY_FLAG_NUMBER,
            std::ptr::null(),
            &mut total as *mut u32 as *mut c_void,
            &mut len,
            std::ptr::null_mut(),
        ) == 0
        {
            total = 0;
        }

        let mut out = Vec::with_capacity(total as usize);
        loop {
            let mut avail: u32 = 0;
            if WinHttpQueryDataAvailable(request.0, &mut avail) == 0 {
                return Err(last_error("WinHttpQueryDataAvailable"));
            }
            if avail == 0 {
                break;
            }
            let start = out.len();
            out.resize(start + avail as usize, 0);
            let mut read: u32 = 0;
            if WinHttpReadData(
                request.0,
                out[start..].as_mut_ptr() as *mut c_void,
                avail,
                &mut read,
            ) == 0
            {
                return Err(last_error("WinHttpReadData"));
            }
            out.truncate(start + read as usize);
            if read == 0 {
                break;
            }
            progress(out.len(), total as usize);
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::parse_url;

    #[test]
    fn url_parts() {
        let (h, p, s, path) = parse_url(
            "https://raw.githubusercontent.com/ropahektic/WormForge-engine/main/downloads/packs/index.toml",
        )
        .unwrap();
        assert_eq!(
            (h.as_str(), p, s, path.as_str()),
            (
                "raw.githubusercontent.com",
                443,
                true,
                "/ropahektic/WormForge-engine/main/downloads/packs/index.toml"
            )
        );
        let (h, p, s, path) = parse_url("http://localhost:8080").unwrap();
        assert_eq!((h.as_str(), p, s, path.as_str()), ("localhost", 8080, false, "/"));
        assert!(parse_url("ftp://x/y").is_err());
    }
}
