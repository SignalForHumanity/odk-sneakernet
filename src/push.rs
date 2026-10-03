//! Uploading instances over the OpenRosa Form Submission API.
//!
//! ODK Central accepts submissions at `<server>/submission`, where `<server>`
//! is the URL Collect is configured with, for example
//! `https://central.example.org/v1/key/<token>/projects/3` for an App User.
//! A submission is a multipart POST with the XML in `xml_submission_file`
//! and each media file in its own part. Large submissions are split across
//! several POSTs carrying the identical XML, as Collect does.

use crate::instance::Instance;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Used when the server does not send `X-OpenRosa-Accept-Content-Length`.
pub const DEFAULT_MAX_BYTES: u64 = 10 * 1024 * 1024;

/// Generous upper bound for the multipart headers of one part.
pub const PART_OVERHEAD: u64 = 256;

/// What happened to one submission.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// Server accepted it (201/200/202).
    Created,
    /// Server already holds a different submission with this instanceID.
    Conflict(String),
    /// Server rejected it; other submissions can still go through.
    Rejected(u16, String),
    /// A local file of this submission could not be read; nothing was sent
    /// for it (or only earlier parts of a split upload).
    Unreadable(String),
}

/// Errors that make further uploads pointless.
#[derive(Debug)]
pub enum Fatal {
    Network(String),
    Auth(u16, String),
    BadUrl(u16, String),
}

impl std::fmt::Display for Fatal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Fatal::Network(e) => write!(f, "network error: {e}"),
            Fatal::Auth(c, m) => write!(f, "server refused the credentials (HTTP {c}): {m}"),
            Fatal::BadUrl(c, m) => write!(
                f,
                "server has no submission endpoint at this URL (HTTP {c}): {m}. Use the server URL configured in Collect."
            ),
        }
    }
}

pub struct Client {
    agent: ureq::Agent,
    submission_url: String,
    auth: Option<String>,
    pub max_bytes: u64,
}

impl Client {
    pub fn new(server: &str, user: Option<&str>, password: Option<&str>) -> Client {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            // No overall deadline: a large batch over a slow field link can
            // take longer than any fixed limit. Bound only the waits.
            .timeout_connect(Some(Duration::from_secs(60)))
            .timeout_recv_response(Some(Duration::from_secs(600)))
            .build()
            .into();
        let auth = user.map(|u| {
            format!(
                "Basic {}",
                base64(format!("{u}:{}", password.unwrap_or("")).as_bytes())
            )
        });
        Client {
            agent,
            submission_url: format!("{}/submission", server.trim_end_matches('/')),
            auth,
            max_bytes: DEFAULT_MAX_BYTES,
        }
    }

    /// HEAD request recommended by OpenRosa. Checks the URL and credentials
    /// and reads the server's size limit.
    pub fn preflight(&mut self) -> Result<(), Fatal> {
        let mut req = self
            .agent
            .head(&self.submission_url)
            .header("X-OpenRosa-Version", "1.0");
        if let Some(a) = &self.auth {
            req = req.header("Authorization", a);
        }
        let resp = req.call().map_err(|e| Fatal::Network(e.to_string()))?;
        let code = resp.status().as_u16();
        if code == 401 || code == 403 {
            return Err(Fatal::Auth(code, String::new()));
        }
        if code == 404 {
            return Err(Fatal::BadUrl(code, String::new()));
        }
        if let Some(n) = resp
            .headers()
            .get("x-openrosa-accept-content-length")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.trim().parse::<u64>().ok())
            && n > 0
        {
            self.max_bytes = n;
        }
        Ok(())
    }

    /// Upload one instance, splitting attachments across requests if needed.
    pub fn send(&self, inst: &Instance) -> Result<Outcome, Fatal> {
        let xml = match fs::read(&inst.xml_path) {
            Ok(x) => x,
            Err(e) => {
                return Ok(Outcome::Unreadable(format!(
                    "cannot read {}: {e}",
                    inst.xml_path.display()
                )));
            }
        };
        let mut files = Vec::new();
        for p in &inst.attachments {
            let size = fs::metadata(p).map(|m| m.len()).unwrap_or(0);
            files.push((p.clone(), size));
        }
        let batches = plan_batches(xml.len() as u64, &files, self.max_bytes);
        let total = batches.len();
        for (i, batch) in batches.iter().enumerate() {
            let incomplete = i + 1 < total;
            let (ctype, body) = match build_multipart(&xml, batch, incomplete, inst.encrypted) {
                Ok(x) => x,
                Err(e) => return Ok(Outcome::Unreadable(format!("cannot read attachment: {e}"))),
            };
            let mut req = self
                .agent
                .post(&self.submission_url)
                .header("X-OpenRosa-Version", "1.0")
                .header("Content-Type", &ctype);
            if let Some(a) = &self.auth {
                req = req.header("Authorization", a);
            }
            let mut resp = req
                .send(&body[..])
                .map_err(|e| Fatal::Network(e.to_string()))?;
            let code = resp.status().as_u16();
            let text = resp.body_mut().read_to_string().unwrap_or_default();
            let message = openrosa_message(&text);
            match code {
                200..=202 => continue,
                401 => return Err(Fatal::Auth(code, message)),
                // Central answers 403 for a form this user may not submit to;
                // other forms can still go through.
                409 => return Ok(Outcome::Conflict(message)),
                _ => return Ok(Outcome::Rejected(code, message)),
            }
        }
        Ok(Outcome::Created)
    }
}

/// Group attachments into requests that stay under `max` bytes (the XML is
/// repeated in every request). Always returns at least one batch. A file
/// larger than the limit goes alone and the server decides.
pub fn plan_batches(xml_len: u64, files: &[(PathBuf, u64)], max: u64) -> Vec<Vec<PathBuf>> {
    let mut batches: Vec<Vec<PathBuf>> = vec![Vec::new()];
    // XML part plus the closing boundary / `*isIncomplete*` part.
    let base = xml_len + 2 * PART_OVERHEAD;
    let mut used = base;
    for (path, size) in files {
        let size = size + PART_OVERHEAD;
        let current = batches.last_mut().expect("non-empty");
        if !current.is_empty() && used + size > max {
            batches.push(vec![path.clone()]);
            used = base + size;
        } else {
            current.push(path.clone());
            used += size;
        }
    }
    batches
}

fn mime_for(name: &str, encrypted: bool) -> &'static str {
    if encrypted {
        return "application/octet-stream";
    }
    let ext = name
        .rsplit_once('.')
        .map(|(_, e)| e.to_ascii_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "jpg" | "jpeg" => "image/jpeg",
        "png" => "image/png",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "mp4" => "video/mp4",
        "3gp" | "3gpp" => "video/3gpp",
        "m4a" => "audio/mp4",
        "amr" => "audio/amr",
        "wav" => "audio/wav",
        "mp3" => "audio/mpeg",
        "ogg" | "opus" => "audio/ogg",
        "csv" => "text/csv",
        "pdf" => "application/pdf",
        "txt" => "text/plain",
        _ => "application/octet-stream",
    }
}

/// Build a multipart/form-data body. Returns (content type, body).
pub fn build_multipart(
    xml: &[u8],
    files: &[PathBuf],
    incomplete: bool,
    encrypted: bool,
) -> std::io::Result<(String, Vec<u8>)> {
    let boundary = format!(
        "odk-sneakernet-{:x}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    );
    let mut body = Vec::new();
    write!(
        body,
        "--{boundary}\r\nContent-Disposition: form-data; name=\"xml_submission_file\"; filename=\"submission.xml\"\r\nContent-Type: text/xml\r\n\r\n"
    )?;
    body.extend_from_slice(xml);
    body.extend_from_slice(b"\r\n");
    for path in files {
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("file")
            .replace('"', "");
        write!(
            body,
            "--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\"; filename=\"{name}\"\r\nContent-Type: {}\r\n\r\n",
            mime_for(&name, encrypted)
        )?;
        body.extend_from_slice(&fs::read(path)?);
        body.extend_from_slice(b"\r\n");
    }
    if incomplete {
        write!(
            body,
            "--{boundary}\r\nContent-Disposition: form-data; name=\"*isIncomplete*\"\r\n\r\nyes\r\n"
        )?;
    }
    write!(body, "--{boundary}--\r\n")?;
    Ok((format!("multipart/form-data; boundary={boundary}"), body))
}

/// Pull the human-readable `<message>` out of an OpenRosaResponse.
pub fn openrosa_message(body: &str) -> String {
    if let Ok(doc) = roxmltree::Document::parse(body)
        && let Some(m) = doc
            .descendants()
            .find(|n| n.tag_name().name() == "message")
            .and_then(|n| n.text())
    {
        return m.trim().to_string();
    }
    let t = body.trim();
    t.chars().take(300).collect()
}

/// Server URL with any App User token removed, safe to write to a log.
pub fn redact_url(url: &str) -> String {
    let url = url.trim_end_matches('/');
    let mut out = Vec::new();
    let mut parts = url.split('/').peekable();
    while let Some(p) = parts.next() {
        out.push(p.to_string());
        if (p == "key" || p == "test") && parts.peek().is_some() {
            parts.next();
            out.push("…".to_string());
        }
    }
    out.join("/")
}

/// Append-only CSV log of upload results, used to resume.
pub struct Log {
    path: PathBuf,
}

impl Log {
    pub fn new(path: &Path) -> Log {
        Log {
            path: path.to_path_buf(),
        }
    }

    /// instanceIDs already accepted by this server.
    pub fn done(&self, server: &str) -> std::collections::HashSet<String> {
        let mut set = std::collections::HashSet::new();
        let Ok(f) = fs::File::open(&self.path) else {
            return set;
        };
        for line in BufReader::new(f).lines().map_while(Result::ok).skip(1) {
            let cols = crate::csvout::parse_line(&line);
            if cols.len() >= 5 && cols[1] == server && cols[4] == "created" {
                set.insert(cols[2].clone());
            }
        }
        set
    }

    pub fn record(
        &self,
        server: &str,
        inst: &Instance,
        result: &str,
        message: &str,
    ) -> std::io::Result<()> {
        let new = !self.path.exists();
        let mut f = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        if new {
            writeln!(f, "time,server,instance_id,form_id,result,message,folder")?;
        }
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let row = [
            crate::csvout::iso_utc(now),
            server.to_string(),
            inst.instance_id.clone().unwrap_or_default(),
            inst.form_id.clone(),
            result.to_string(),
            message.to_string(),
            inst.dir.display().to_string(),
        ];
        writeln!(f, "{}", crate::csvout::join(&row))
    }
}

/// Standard base64 (RFC 4648) for the Basic auth header.
pub fn base64(input: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in input.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            T[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            T[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_vectors() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"user@x.org:p4ss"), "dXNlckB4Lm9yZzpwNHNz");
    }

    #[test]
    fn batches_respect_limit() {
        let f = |n: &str, s| (PathBuf::from(n), s);
        let files = vec![f("a", 40), f("b", 40), f("c", 40), f("d", 200)];
        let b = plan_batches(10, &files, 100 + 4 * PART_OVERHEAD);
        assert_eq!(b.len(), 3);
        assert_eq!(b[0], vec![PathBuf::from("a"), PathBuf::from("b")]);
        assert_eq!(b[1], vec![PathBuf::from("c")]);
        assert_eq!(b[2], vec![PathBuf::from("d")]);
        assert_eq!(plan_batches(10, &[], 100), vec![Vec::<PathBuf>::new()]);
    }

    #[test]
    fn batches_count_multipart_overhead() {
        // Two 45-byte files plus a 10-byte XML fit 100 bytes only if the
        // multipart headers are ignored; the real request is larger.
        let files = vec![(PathBuf::from("a"), 45), (PathBuf::from("b"), 45)];
        let b = plan_batches(10, &files, 100 + 2 * PART_OVERHEAD);
        assert_eq!(b.len(), 2);
    }

    #[test]
    fn message_extraction() {
        let body = r#"<OpenRosaResponse xmlns="http://openrosa.org/http/response" items="0"><message nature="error">A submission already exists with this ID, but with different XML.</message></OpenRosaResponse>"#;
        assert_eq!(
            openrosa_message(body),
            "A submission already exists with this ID, but with different XML."
        );
        assert_eq!(openrosa_message("  plain text "), "plain text");
    }

    #[test]
    fn redacts_tokens() {
        assert_eq!(
            redact_url("https://c.org/v1/key/AbC123/projects/3/"),
            "https://c.org/v1/key/…/projects/3"
        );
        assert_eq!(
            redact_url("https://kc.kobotoolbox.org/me"),
            "https://kc.kobotoolbox.org/me"
        );
    }

    fn inst(xml: &Path, attachments: Vec<PathBuf>) -> Instance {
        Instance {
            dir: xml.parent().unwrap().to_path_buf(),
            xml_path: xml.to_path_buf(),
            form_id: "f".into(),
            version: None,
            instance_id: Some("uuid:x".into()),
            encrypted: false,
            status: crate::instance::Status::Finalized,
            attachments,
            missing: vec![],
        }
    }

    #[test]
    fn unreadable_files_are_not_network_errors() {
        // Port 9 on loopback is never contacted: reading fails first.
        let c = Client::new("http://127.0.0.1:9", None, None);
        let dir = std::env::temp_dir().join(format!("odk-sn-unread-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let xml = dir.join("x.xml");
        let gone = dir.join("gone.xml");
        assert!(matches!(
            c.send(&inst(&gone, vec![])),
            Ok(Outcome::Unreadable(m)) if m.contains("gone.xml")
        ));
        fs::write(&xml, "<data id=\"f\"/>").unwrap();
        assert!(matches!(
            c.send(&inst(&xml, vec![dir.join("photo.jpg")])),
            Ok(Outcome::Unreadable(_))
        ));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn mime_types() {
        assert_eq!(mime_for("x.JPG", false), "image/jpeg");
        assert_eq!(mime_for("x.jpg.enc", true), "application/octet-stream");
        assert_eq!(mime_for("noext", false), "application/octet-stream");
    }
}
