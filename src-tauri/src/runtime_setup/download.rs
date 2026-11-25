//! Resumable, checksum-verified downloads for the speech setup.
//!
//! A file downloads to `<dest>.part`. An interrupted download (cancel, network drop, quit, crash)
//! leaves the `.part` behind and the next attempt asks the server for the rest with an HTTP `Range`.
//! Only a file whose size and SHA-256 match the pins is renamed to `dest`, so `dest` is never partial.

use anyhow::{Context, Result, anyhow, bail};
use sha2::{Digest, Sha256};
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

const CHUNK: usize = 256 * 1024;

/// Raised when the user pauses setup; never shown as an error.
#[derive(Debug, thiserror::Error)]
#[error("setup was paused")]
pub struct Cancelled;

pub fn client() -> Result<reqwest::blocking::Client> {
    reqwest::blocking::Client::builder()
        // Generic agent: nothing about the user or the machine.
        .user_agent(concat!("Wren/", env!("CARGO_PKG_VERSION"), " speech-setup"))
        .connect_timeout(Duration::from_secs(30))
        // Per read: a stalled connection fails within a minute instead of hanging forever.
        .timeout(Duration::from_secs(60))
        .build()
        .context("cannot set up the network client")
}

pub fn part_path(dest: &Path) -> PathBuf {
    let mut name = dest.file_name().map(|n| n.to_os_string()).unwrap_or_default();
    name.push(".part");
    dest.with_file_name(name)
}

/// SHA-256 of a file, lowercase hex.
pub fn sha256_file(path: &Path, cancel: &AtomicBool) -> Result<String> {
    let mut f = File::open(path).with_context(|| format!("cannot open {}", path.display()))?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; CHUNK];
    loop {
        if cancel.load(Ordering::SeqCst) {
            return Err(Cancelled.into());
        }
        let n = f.read(&mut buf).with_context(|| format!("cannot read {}", path.display()))?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(format!("{:x}", h.finalize()))
}

/// True when `path` exists with exactly this size and hash.
pub fn file_matches(path: &Path, size: u64, sha256: &str, cancel: &AtomicBool) -> Result<bool> {
    match std::fs::metadata(path) {
        Ok(m) if m.is_file() && m.len() == size => Ok(sha256_file(path, cancel)?.eq_ignore_ascii_case(sha256)),
        _ => Ok(false),
    }
}

/// Download `url` to `dest`, resuming a `.part` left by an earlier try. `progress(bytes_on_disk)`
/// is called as data arrives (starting with what an earlier try already fetched).
pub fn fetch(
    client: &reqwest::blocking::Client,
    url: &str,
    dest: &Path,
    size: u64,
    sha256: &str,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(u64),
) -> Result<()> {
    if let Some(dir) = dest.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
    }
    if file_matches(dest, size, sha256, cancel)? {
        progress(size);
        return Ok(());
    }
    let part = part_path(dest);
    let mut have = std::fs::metadata(&part).map(|m| m.len()).unwrap_or(0);
    if have > size {
        have = 0; // not ours, or the pins changed: start again
    }
    if have < size {
        progress(have);
        let mut req = client.get(url);
        if have > 0 {
            req = req.header(reqwest::header::RANGE, format!("bytes={have}-"));
        }
        let mut resp = req.send().map_err(|e| anyhow!(network_words(&e))).with_context(|| format!("cannot reach {}", host_of(url)))?;
        let status = resp.status();
        let mut file = if have > 0 && status == reqwest::StatusCode::PARTIAL_CONTENT {
            OpenOptions::new().append(true).open(&part).with_context(|| format!("cannot open {}", part.display()))?
        } else if status.is_success() {
            // The server sent the whole file (no resume support or first try): start from zero.
            have = 0;
            progress(0);
            File::create(&part).with_context(|| format!("cannot create {}", part.display()))?
        } else if status == reqwest::StatusCode::RANGE_NOT_SATISFIABLE {
            let _ = std::fs::remove_file(&part);
            bail!("the download could not continue where it stopped; try again to start it afresh");
        } else {
            bail!("{} answered {} for {}", host_of(url), status, file_label(dest));
        };
        let mut buf = vec![0u8; CHUNK];
        loop {
            if cancel.load(Ordering::SeqCst) {
                let _ = file.flush();
                return Err(Cancelled.into());
            }
            let n = match resp.read(&mut buf) {
                Ok(n) => n,
                Err(e) => {
                    let _ = file.flush();
                    return Err(anyhow!(
                        "the connection dropped while downloading {} ({e}); try again to continue where it stopped",
                        file_label(dest)
                    ));
                }
            };
            if n == 0 {
                break;
            }
            if have + n as u64 > size {
                drop(file);
                let _ = std::fs::remove_file(&part);
                bail!("{} sent more data than expected for {}", host_of(url), file_label(dest));
            }
            file.write_all(&buf[..n]).with_context(|| format!("cannot write {}", part.display()))?;
            have += n as u64;
            progress(have);
        }
        file.sync_all().ok();
        if have < size {
            bail!("the download of {} stopped early; try again to continue it", file_label(dest));
        }
    }
    let got = sha256_file(&part, cancel)?;
    if !got.eq_ignore_ascii_case(sha256) {
        let _ = std::fs::remove_file(&part);
        bail!("{} did not match its checksum, so it was discarded; try again to download it afresh", file_label(dest));
    }
    if dest.exists() {
        std::fs::remove_file(dest).with_context(|| format!("cannot replace {}", dest.display()))?;
    }
    std::fs::rename(&part, dest).with_context(|| format!("cannot move {} into place", file_label(dest)))?;
    progress(size);
    Ok(())
}

fn file_label(dest: &Path) -> String {
    dest.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| dest.display().to_string())
}

fn host_of(url: &str) -> String {
    url.split("://").nth(1).and_then(|r| r.split('/').next()).unwrap_or(url).to_string()
}

fn network_words(e: &reqwest::Error) -> String {
    if e.is_connect() {
        "no internet connection (or the server could not be reached)".into()
    } else if e.is_timeout() {
        "the connection timed out".into()
    } else {
        format!("network error: {e}")
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::io::BufRead;
    use std::net::TcpListener;
    use std::sync::Arc;
    use std::sync::atomic::AtomicUsize;

    /// A tiny HTTP/1.1 server for tests: serves `body` with `Range` support. `cut_after` closes the
    /// connection after that many body bytes on the first request (a dropped download).
    pub(crate) struct TestServer {
        pub url: String,
        pub requests: Arc<AtomicUsize>,
        pub ranges: Arc<parking_lot::Mutex<Vec<Option<u64>>>>,
    }

    pub(crate) fn serve(files: Vec<(String, Vec<u8>)>, cut_after: Option<usize>, support_range: bool) -> TestServer {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let requests = Arc::new(AtomicUsize::new(0));
        let ranges = Arc::new(parking_lot::Mutex::new(Vec::new()));
        let (req2, ranges2) = (requests.clone(), ranges.clone());
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let nth = req2.fetch_add(1, Ordering::SeqCst);
                let mut reader = std::io::BufReader::new(stream.try_clone().unwrap());
                let mut first = String::new();
                if reader.read_line(&mut first).is_err() {
                    continue;
                }
                let path = first.split_whitespace().nth(1).unwrap_or("/").to_string();
                let mut range: Option<u64> = None;
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                        break;
                    }
                    let lower = line.to_ascii_lowercase();
                    if let Some(v) = lower.strip_prefix("range: bytes=") {
                        range = v.trim().trim_end_matches('-').parse().ok();
                    }
                }
                ranges2.lock().push(range);
                let Some((_, body)) = files.iter().find(|(p, _)| *p == path) else {
                    let _ = stream.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
                    continue;
                };
                let (status, slice) = match range {
                    Some(start) if support_range => ("206 Partial Content", &body[start as usize..]),
                    _ => ("200 OK", &body[..]),
                };
                let head = format!("HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", slice.len());
                let _ = stream.write_all(head.as_bytes());
                let send = match cut_after {
                    Some(n) if nth == 0 => &slice[..n.min(slice.len())],
                    _ => slice,
                };
                let _ = stream.write_all(send);
                let _ = stream.flush();
            }
        });
        TestServer { url: format!("http://{addr}"), requests, ranges }
    }

    pub(crate) fn sha_hex(data: &[u8]) -> String {
        format!("{:x}", Sha256::digest(data))
    }

    fn temp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("wren-dl-{name}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn body(n: usize) -> Vec<u8> {
        (0..n).map(|i| (i * 7 % 251) as u8).collect()
    }

    #[test]
    fn downloads_verifies_and_skips_a_finished_file() {
        let data = body(700_000);
        let srv = serve(vec![("/f.bin".into(), data.clone())], None, true);
        let dir = temp("ok");
        let dest = dir.join("f.bin");
        let never = AtomicBool::new(false);
        let mut last = 0;
        fetch(&client().unwrap(), &format!("{}/f.bin", srv.url), &dest, data.len() as u64, &sha_hex(&data), &never, &mut |b| last = b).unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), data);
        assert_eq!(last, data.len() as u64);
        assert!(!part_path(&dest).exists());
        // Second call: already there and verified, no request.
        fetch(&client().unwrap(), &format!("{}/f.bin", srv.url), &dest, data.len() as u64, &sha_hex(&data), &never, &mut |_| {}).unwrap();
        assert_eq!(srv.requests.load(Ordering::SeqCst), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_dropped_download_resumes_with_a_range_request() {
        let data = body(900_000);
        let srv = serve(vec![("/m.bin".into(), data.clone())], Some(300_000), true);
        let dir = temp("resume");
        let dest = dir.join("m.bin");
        let never = AtomicBool::new(false);
        let url = format!("{}/m.bin", srv.url);
        let err = fetch(&client().unwrap(), &url, &dest, data.len() as u64, &sha_hex(&data), &never, &mut |_| {}).unwrap_err();
        assert!(format!("{err:#}").contains("try again"), "{err:#}");
        let kept = std::fs::metadata(part_path(&dest)).unwrap().len();
        assert!(kept > 0 && kept <= 300_000, "partial kept: {kept}");
        assert!(!dest.exists(), "never a partial file under the real name");
        let mut first_progress = None;
        fetch(&client().unwrap(), &url, &dest, data.len() as u64, &sha_hex(&data), &never, &mut |b| {
            first_progress.get_or_insert(b);
        })
        .unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), data);
        assert_eq!(srv.ranges.lock().last().copied().flatten(), Some(kept), "asked only for the rest");
        assert_eq!(first_progress, Some(kept), "progress starts from what was already there");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_server_without_range_support_restarts_cleanly() {
        let data = body(400_000);
        let srv = serve(vec![("/m.bin".into(), data.clone())], None, false);
        let dir = temp("norange");
        let dest = dir.join("m.bin");
        std::fs::write(part_path(&dest), &data[..1000]).unwrap();
        fetch(&client().unwrap(), &format!("{}/m.bin", srv.url), &dest, data.len() as u64, &sha_hex(&data), &AtomicBool::new(false), &mut |_| {}).unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), data);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_wrong_checksum_is_discarded_never_installed() {
        let data = body(10_000);
        let srv = serve(vec![("/m.bin".into(), data.clone())], None, true);
        let dir = temp("badsum");
        let dest = dir.join("m.bin");
        let err = fetch(&client().unwrap(), &format!("{}/m.bin", srv.url), &dest, data.len() as u64, &"0".repeat(64), &AtomicBool::new(false), &mut |_| {})
            .unwrap_err();
        assert!(format!("{err:#}").contains("checksum"), "{err:#}");
        assert!(!dest.exists() && !part_path(&dest).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn pausing_keeps_the_partial_file() {
        let data = body(2_000_000);
        let srv = serve(vec![("/m.bin".into(), data.clone())], None, true);
        let dir = temp("pause");
        let dest = dir.join("m.bin");
        let cancel = AtomicBool::new(false);
        let err = fetch(&client().unwrap(), &format!("{}/m.bin", srv.url), &dest, data.len() as u64, &sha_hex(&data), &cancel, &mut |b| {
            if b > 0 {
                cancel.store(true, Ordering::SeqCst);
            }
        })
        .unwrap_err();
        assert!(err.downcast_ref::<Cancelled>().is_some(), "{err:#}");
        assert!(part_path(&dest).exists() && !dest.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn http_errors_and_missing_servers_are_explained() {
        let srv = serve(vec![], None, true);
        let dir = temp("404");
        let err = fetch(&client().unwrap(), &format!("{}/nope", srv.url), &dir.join("x"), 5, &"0".repeat(64), &AtomicBool::new(false), &mut |_| {})
            .unwrap_err();
        assert!(format!("{err:#}").contains("404"), "{err:#}");
        // Nothing listens on port 9 (discard) on 127.0.0.1 in CI or here.
        let err = fetch(&client().unwrap(), "http://127.0.0.1:9/x", &dir.join("y"), 5, &"0".repeat(64), &AtomicBool::new(false), &mut |_| {})
            .unwrap_err();
        assert!(format!("{err:#}").contains("cannot reach 127.0.0.1:9"), "{err:#}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
