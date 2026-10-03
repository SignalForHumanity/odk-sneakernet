//! Finding and reading ODK Collect instance folders copied off a device.
//!
//! Collect stores each filled form in its own folder under `instances/`:
//! `instances/<formid>_<date>/<formid>_<date>.xml` plus any media files.
//! Encrypted forms instead hold `submission.xml` (a manifest) and `*.enc`
//! files. The draft/finalized status is not in the folder at all; it lives in
//! `metadata/instances.db` next to the `instances` folder.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

/// Status of an instance as recorded by Collect.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Status {
    /// Finalized and not yet sent (`complete`, `submissionFailed`).
    Finalized,
    /// Saved as a draft (`incomplete`, `valid`, `invalid`, `newEdit`).
    Draft,
    /// Collect already sent it to a server (`submitted`).
    Sent,
    /// No `instances.db` was copied, or the instance is not listed in it.
    Unknown,
}

impl Status {
    pub fn from_collect(s: &str) -> Status {
        match s {
            "complete" | "submissionFailed" => Status::Finalized,
            "incomplete" | "valid" | "invalid" | "newEdit" => Status::Draft,
            "submitted" => Status::Sent,
            _ => Status::Unknown,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Status::Finalized => "finalized",
            Status::Draft => "draft",
            Status::Sent => "sent",
            Status::Unknown => "unknown",
        }
    }
}

/// One submission found on disk.
#[derive(Debug, Clone)]
pub struct Instance {
    /// Folder holding the submission.
    pub dir: PathBuf,
    /// The submission XML file (the manifest for encrypted forms).
    pub xml_path: PathBuf,
    pub form_id: String,
    pub version: Option<String>,
    pub instance_id: Option<String>,
    pub encrypted: bool,
    pub status: Status,
    /// Other files in the folder, sent as attachments.
    pub attachments: Vec<PathBuf>,
    /// File names the XML refers to that are not in the folder.
    pub missing: Vec<String>,
}

/// A folder that could not be read as an instance.
#[derive(Debug, Clone)]
pub struct Problem {
    pub path: PathBuf,
    pub message: String,
}

/// Everything found under the given paths.
#[derive(Debug, Default)]
pub struct Inventory {
    pub instances: Vec<Instance>,
    pub problems: Vec<Problem>,
    /// `instances.db` files that were read.
    pub databases: Vec<PathBuf>,
}

/// File extensions Collect produces for media and audit answers.
const MEDIA_EXTENSIONS: &[&str] = &[
    "jpg", "jpeg", "png", "gif", "webp", "bmp", "heic", "mp4", "3gp", "3gpp", "mov", "webm", "mkv",
    "m4a", "amr", "aac", "wav", "mp3", "ogg", "opus", "csv", "pdf", "txt", "enc",
];

/// Parsed contents of a submission XML.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Parsed {
    pub form_id: String,
    pub version: Option<String>,
    pub instance_id: Option<String>,
    pub encrypted: bool,
    /// File names referenced by the submission.
    pub files: Vec<String>,
}

/// Read the form id, version, instanceID and referenced files from submission XML.
pub fn parse_submission(xml: &str) -> Result<Parsed, String> {
    let doc = roxmltree::Document::parse(xml).map_err(|e| format!("invalid XML: {e}"))?;
    let root = doc.root_element();
    let form_id = root
        .attribute("id")
        .or_else(|| root.attribute("xmlns"))
        .ok_or("root element has no id attribute")?
        .to_string();
    let version = root.attribute("version").map(str::to_string);
    let encrypted = root.attribute("encrypted") == Some("yes");
    let mut instance_id = None;
    for node in root.descendants().filter(|n| n.is_element()) {
        if node.tag_name().name() == "instanceID"
            && node.parent_element().map(|p| p.tag_name().name()) == Some("meta")
        {
            instance_id = node
                .text()
                .map(str::trim)
                .filter(|t| !t.is_empty())
                .map(str::to_string);
            break;
        }
    }
    let mut files = Vec::new();
    for node in root.descendants().filter(|n| n.is_element()) {
        if node.children().any(|c| c.is_element()) {
            continue;
        }
        let name = node.tag_name().name();
        if name == "encryptedXmlFile" {
            continue;
        }
        let Some(text) = node.text().map(str::trim) else {
            continue;
        };
        let in_media = node.parent_element().map(|p| p.tag_name().name()) == Some("media");
        if (encrypted && in_media && name == "file") || (!encrypted && looks_like_file(text)) {
            files.push(text.to_string());
        }
    }
    if encrypted {
        // The encrypted submission body is a required attachment too.
        if let Some(n) = root
            .descendants()
            .find(|n| n.tag_name().name() == "encryptedXmlFile")
            .and_then(|n| n.text())
        {
            files.push(n.trim().to_string());
        }
    }
    Ok(Parsed {
        form_id,
        version,
        instance_id,
        encrypted,
        files,
    })
}

fn looks_like_file(text: &str) -> bool {
    if text.is_empty() || text.len() > 200 || text.contains(['/', '\\', ' ', ':']) {
        return false;
    }
    match text.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => {
            let ext = ext.to_ascii_lowercase();
            MEDIA_EXTENSIONS.contains(&ext.as_str())
        }
        _ => false,
    }
}

/// Map from instance folder name to Collect status, read from `instances.db`.
pub fn read_status_db(db: &Path) -> Result<HashMap<String, Status>, String> {
    // Collect uses WAL mode, so a copied database may come with a -wal file.
    // Work on a private copy so SQLite can replay it without touching the
    // original (which may be on a read-only USB drive).
    let tmp = std::env::temp_dir().join(format!(
        "odk-sneakernet-{}-{}",
        std::process::id(),
        unique_suffix()
    ));
    fs::create_dir_all(&tmp).map_err(|e| e.to_string())?;
    let copy = tmp.join("instances.db");
    let result = (|| {
        fs::copy(db, &copy).map_err(|e| format!("cannot copy {}: {e}", db.display()))?;
        for suffix in ["-wal", "-journal"] {
            let side = PathBuf::from(format!("{}{suffix}", db.display()));
            if side.exists() {
                fs::copy(&side, tmp.join(format!("instances.db{suffix}")))
                    .map_err(|e| e.to_string())?;
            }
        }
        let conn = rusqlite::Connection::open(&copy).map_err(|e| e.to_string())?;
        let mut stmt = conn
            .prepare("SELECT instanceFilePath, status FROM instances")
            .map_err(|e| format!("{}: not a Collect instances database ({e})", db.display()))?;
        let rows = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, Option<String>>(0)?,
                    r.get::<_, Option<String>>(1)?,
                ))
            })
            .map_err(|e| e.to_string())?;
        let mut map = HashMap::new();
        for row in rows {
            let (path, status) = row.map_err(|e| e.to_string())?;
            if let (Some(path), Some(status)) = (path, status)
                && let Some(folder) = folder_of_db_path(&path)
            {
                map.insert(folder, Status::from_collect(&status));
            }
        }
        Ok(map)
    })();
    let _ = fs::remove_dir_all(&tmp);
    result
}

/// `instanceFilePath` is relative in current Collect (`x/x.xml`) and absolute
/// in old versions (`/sdcard/odk/instances/x/x.xml`). Either way the folder
/// is the second-to-last component.
fn folder_of_db_path(path: &str) -> Option<String> {
    let mut parts = path.rsplit(['/', '\\']).filter(|p| !p.is_empty());
    parts.next()?;
    parts.next().map(str::to_string)
}

fn unique_suffix() -> u128 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed) as u128;
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    t ^ (n << 100) ^ n
}

/// Find the submission XML in a folder, if the folder is an instance folder.
fn instance_xml(dir: &Path) -> Option<PathBuf> {
    let name = dir.file_name()?.to_str()?;
    let own = dir.join(format!("{name}.xml"));
    if own.is_file() {
        return Some(own);
    }
    let manifest = dir.join("submission.xml");
    if manifest.is_file() {
        return Some(manifest);
    }
    None
}

/// Walk `paths` and collect every instance folder below them.
pub fn scan(paths: &[PathBuf]) -> Inventory {
    let mut inv = Inventory::default();
    let mut dbs: HashMap<PathBuf, Option<HashMap<String, Status>>> = HashMap::new();
    let mut dirs = Vec::new();
    for p in paths {
        if !p.is_dir() {
            inv.problems.push(Problem {
                path: p.clone(),
                message: "not a folder".into(),
            });
            continue;
        }
        walk(p, &mut dirs, 0);
    }
    dirs.sort();
    dirs.dedup();
    for (dir, xml_path) in dirs {
        let status = match dir.parent().and_then(Path::parent) {
            Some(project) => {
                let db_path = project.join("metadata").join("instances.db");
                let entry = dbs.entry(db_path.clone()).or_insert_with(|| {
                    if !db_path.is_file() {
                        return None;
                    }
                    match read_status_db(&db_path) {
                        Ok(m) => {
                            inv.databases.push(db_path.clone());
                            Some(m)
                        }
                        Err(e) => {
                            inv.problems.push(Problem {
                                path: db_path.clone(),
                                message: e,
                            });
                            None
                        }
                    }
                });
                let folder = dir.file_name().and_then(|n| n.to_str()).unwrap_or("");
                entry
                    .as_ref()
                    .and_then(|m| m.get(folder).copied())
                    .unwrap_or(Status::Unknown)
            }
            None => Status::Unknown,
        };
        match read_instance(&dir, &xml_path, status) {
            Ok(i) => inv.instances.push(i),
            Err(message) => inv.problems.push(Problem {
                path: xml_path,
                message,
            }),
        }
    }
    inv
}

fn walk(dir: &Path, out: &mut Vec<(PathBuf, PathBuf)>, depth: usize) {
    if depth > 12 {
        return;
    }
    if let Some(xml) = instance_xml(dir) {
        out.push((dir.to_path_buf(), xml));
        return;
    }
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let mut subdirs: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    subdirs.sort();
    for sub in subdirs {
        // Blank form media folders (`forms/x-media`) never hold submissions.
        if sub
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.ends_with("-media"))
        {
            continue;
        }
        walk(&sub, out, depth + 1);
    }
}

fn read_instance(dir: &Path, xml_path: &Path, status: Status) -> Result<Instance, String> {
    let xml = fs::read_to_string(xml_path).map_err(|e| e.to_string())?;
    let parsed = parse_submission(&xml)?;
    let mut attachments = Vec::new();
    let mut present = Vec::new();
    for entry in fs::read_dir(dir).map_err(|e| e.to_string())? {
        let path = entry.map_err(|e| e.to_string())?.path();
        if !path.is_file() || path == xml_path {
            continue;
        }
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        // Collect's autosave and editing leftovers are not attachments.
        if name.starts_with('.') || name.ends_with(".save") || name.ends_with(".xml") {
            continue;
        }
        // An encrypted submission consists of exactly the files its manifest
        // lists. Anything else is plaintext Collect failed to delete.
        if parsed.encrypted && !parsed.files.iter().any(|f| f == name) {
            continue;
        }
        present.push(name.to_string());
        attachments.push(path);
    }
    attachments.sort();
    let missing = parsed
        .files
        .iter()
        .filter(|f| !present.contains(f))
        .cloned()
        .collect();
    Ok(Instance {
        dir: dir.to_path_buf(),
        xml_path: xml_path.to_path_buf(),
        form_id: parsed.form_id,
        version: parsed.version,
        instance_id: parsed.instance_id,
        encrypted: parsed.encrypted,
        status,
        attachments,
        missing,
    })
}

/// Result of removing duplicate copies of the same submission.
#[derive(Debug, Default)]
pub struct Deduped {
    /// One instance per instanceID (plus instances without an ID).
    pub unique: Vec<Instance>,
    /// Identical copies that were dropped.
    pub copies: Vec<Instance>,
    /// instanceIDs whose copies differ; none of them are kept in `unique`.
    pub conflicts: Vec<(String, Vec<Instance>)>,
}

/// Group instances by instanceID. Byte-identical XML counts as the same
/// submission; different XML under the same ID is a conflict.
pub fn dedupe(instances: Vec<Instance>) -> Deduped {
    let mut by_id: Vec<(String, Vec<Instance>)> = Vec::new();
    let mut index: HashMap<String, usize> = HashMap::new();
    let mut out = Deduped::default();
    for inst in instances {
        match inst.instance_id.clone() {
            Some(id) => match index.get(&id) {
                Some(&i) => by_id[i].1.push(inst),
                None => {
                    index.insert(id.clone(), by_id.len());
                    by_id.push((id, vec![inst]));
                }
            },
            None => out.unique.push(inst),
        }
    }
    for (id, mut group) in by_id {
        if group.len() == 1 {
            out.unique.push(group.remove(0));
            continue;
        }
        // A draft is an earlier state of the same submission (e.g. the same
        // phone copied before and after finalizing). When a non-draft copy
        // exists, drafts never make a conflict.
        if group.iter().any(|g| g.status != Status::Draft) {
            let (drafts, rest): (Vec<_>, Vec<_>) =
                group.into_iter().partition(|g| g.status == Status::Draft);
            out.copies.extend(drafts);
            group = rest;
            if group.len() == 1 {
                out.unique.push(group.remove(0));
                continue;
            }
        }
        let first = fs::read(&group[0].xml_path).unwrap_or_default();
        let same = group[1..]
            .iter()
            .all(|g| fs::read(&g.xml_path).unwrap_or_default() == first);
        if same {
            // Keep the copy with the most informative status and most files.
            group.sort_by_key(|g| (g.status, std::cmp::Reverse(g.attachments.len())));
            let keep = group.remove(0);
            out.unique.push(keep);
            out.copies.extend(group);
        } else {
            out.conflicts.push((id, group));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_plain_submission() {
        let xml = r#"<?xml version='1.0' ?><data id="hh_survey" version="2024031501" xmlns:orx="http://openrosa.org/xforms"><name>Ana</name><photo>1700000000000.jpg</photo><note>see file.txt later</note><grp><voice>rec.m4a</voice></grp><meta><audit>audit.csv</audit><orx:instanceID>uuid:abc</orx:instanceID></meta></data>"#;
        let p = parse_submission(xml).unwrap();
        assert_eq!(p.form_id, "hh_survey");
        assert_eq!(p.version.as_deref(), Some("2024031501"));
        assert_eq!(p.instance_id.as_deref(), Some("uuid:abc"));
        assert!(!p.encrypted);
        assert_eq!(p.files, vec!["1700000000000.jpg", "rec.m4a", "audit.csv"]);
    }

    #[test]
    fn parses_encrypted_manifest() {
        let xml = r#"<data id="secret" encrypted="yes" xmlns="http://www.opendatakit.org/xforms/encrypted"><base64EncryptedKey>AAA</base64EncryptedKey><meta xmlns="http://openrosa.org/xforms"><instanceID>uuid:e1</instanceID></meta><media><file>a.jpg.enc</file></media><encryptedXmlFile>submission.xml.enc</encryptedXmlFile><base64EncryptedElementSignature>x</base64EncryptedElementSignature></data>"#;
        let p = parse_submission(xml).unwrap();
        assert!(p.encrypted);
        assert_eq!(p.instance_id.as_deref(), Some("uuid:e1"));
        assert_eq!(p.files, vec!["a.jpg.enc", "submission.xml.enc"]);
    }

    #[test]
    fn blank_instance_id_is_none() {
        let xml = r#"<data id="f"><meta><instanceID>  </instanceID></meta></data>"#;
        assert_eq!(parse_submission(xml).unwrap().instance_id, None);
    }

    #[test]
    fn rejects_bad_xml_and_missing_id() {
        assert!(parse_submission("<data>").is_err());
        assert!(parse_submission("<data><a>1</a></data>").is_err());
    }

    #[test]
    fn file_heuristic() {
        assert!(looks_like_file("IMG_1.JPG"));
        assert!(!looks_like_file("3.5"));
        assert!(!looks_like_file(".jpg"));
        assert!(!looks_like_file("a b.jpg"));
        assert!(!looks_like_file("http://x/y.jpg"));
    }

    #[test]
    fn status_mapping() {
        assert_eq!(Status::from_collect("complete"), Status::Finalized);
        assert_eq!(Status::from_collect("submissionFailed"), Status::Finalized);
        assert_eq!(Status::from_collect("incomplete"), Status::Draft);
        assert_eq!(Status::from_collect("valid"), Status::Draft);
        assert_eq!(Status::from_collect("submitted"), Status::Sent);
        assert_eq!(Status::from_collect("weird"), Status::Unknown);
    }

    #[test]
    fn db_paths() {
        assert_eq!(
            folder_of_db_path("f_2024/f_2024.xml").as_deref(),
            Some("f_2024")
        );
        assert_eq!(
            folder_of_db_path("/storage/emulated/0/odk/instances/f_1/f_1.xml").as_deref(),
            Some("f_1")
        );
        assert_eq!(folder_of_db_path("x.xml"), None);
    }
}
