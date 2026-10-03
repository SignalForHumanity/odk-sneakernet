//! End-to-end tests on synthetic phone copies and a local mock OpenRosa server.

use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

fn tempdir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!(
        "odk-sneakernet-test-{name}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(&d).unwrap();
    d
}

fn submission(id: &str, name: &str, photo: Option<&str>) -> String {
    let photo = photo
        .map(|p| format!("<photo>{p}</photo>"))
        .unwrap_or_default();
    format!(
        "<?xml version='1.0' ?><data id=\"hh\" version=\"3\" xmlns:orx=\"http://openrosa.org/xforms\"><village>{name}</village>{photo}<household><member><name>A</name></member><member><name>B</name></member></household><meta><orx:instanceID>{id}</orx:instanceID></meta></data>"
    )
}

fn put_instance(instances: &Path, folder: &str, xml: &str, files: &[(&str, usize)]) {
    let d = instances.join(folder);
    fs::create_dir_all(&d).unwrap();
    fs::write(d.join(format!("{folder}.xml")), xml).unwrap();
    for (f, size) in files {
        fs::write(d.join(f), vec![b'x'; *size]).unwrap();
    }
}

const FORM: &str = r#"<h:html xmlns="http://www.w3.org/2002/xforms" xmlns:h="http://www.w3.org/1999/xhtml"><h:head><model><instance><data id="hh" version="3"><village/><photo/><household><member><name/></member></household><meta><instanceID/></meta></data></instance></model></h:head><h:body><repeat nodeset="/data/household/member"/></h:body></h:html>"#;

/// Two phones: a modern Collect copy with instances.db, and a legacy
/// `odk` folder without a database.
fn fixture() -> PathBuf {
    let root = tempdir("fixture");
    let project = root.join("phone1/projects/0a1b-uuid");
    let inst = project.join("instances");
    put_instance(
        &inst,
        "hh_2024-05-01_09-00-00",
        &submission("uuid:1", "Kisu", Some("p1.jpg")),
        &[("p1.jpg", 400), ("hh_2024-05-01_09-00-00.xml.save", 3)],
    );
    put_instance(
        &inst,
        "hh_2024-05-01_10-00-00",
        &submission("uuid:2", "Bora", Some("gone.jpg")),
        &[],
    );
    put_instance(
        &inst,
        "hh_2024-05-01_11-00-00",
        &submission("uuid:draft", "Draft", None),
        &[],
    );
    put_instance(
        &inst,
        "hh_2024-05-01_12-00-00",
        &submission("uuid:sent", "Sent", None),
        &[],
    );
    fs::create_dir_all(project.join("metadata")).unwrap();
    fs::create_dir_all(project.join("forms/hh-media")).unwrap();
    fs::write(project.join("forms/hh.xml"), FORM).unwrap();
    let conn = rusqlite::Connection::open(project.join("metadata/instances.db")).unwrap();
    conn.execute_batch(
        "PRAGMA journal_mode=WAL;
         CREATE TABLE instances (_id INTEGER PRIMARY KEY, displayName TEXT, instanceFilePath TEXT, jrFormId TEXT, status TEXT);
         INSERT INTO instances (instanceFilePath, jrFormId, status) VALUES
           ('hh_2024-05-01_09-00-00/hh_2024-05-01_09-00-00.xml', 'hh', 'complete'),
           ('hh_2024-05-01_10-00-00/hh_2024-05-01_10-00-00.xml', 'hh', 'submissionFailed'),
           ('hh_2024-05-01_11-00-00/hh_2024-05-01_11-00-00.xml', 'hh', 'incomplete'),
           ('hh_2024-05-01_12-00-00/hh_2024-05-01_12-00-00.xml', 'hh', 'submitted');",
    )
    .unwrap();
    drop(conn);

    let legacy = root.join("phone2/odk/instances");
    // Identical copy of uuid:1 (the same phone copied twice).
    put_instance(
        &legacy,
        "hh_2024-05-01_09-00-00",
        &submission("uuid:1", "Kisu", Some("p1.jpg")),
        &[("p1.jpg", 400)],
    );
    // Same instanceID with different data: a conflict.
    put_instance(
        &legacy,
        "hh_2024-05-02_08-00-00",
        &submission("uuid:3", "Mto", None),
        &[],
    );
    put_instance(
        &legacy,
        "hh_2024-05-02_08-30-00",
        &submission("uuid:3", "Mto changed", None),
        &[],
    );
    // A clean submission with unknown status and two big photos.
    put_instance(
        &legacy,
        "hh_2024-05-03_08-00-00",
        &submission("uuid:4", "Pwani", Some("a.jpg")),
        &[("a.jpg", 900), ("b.jpg", 900)],
    );
    // Encrypted submission.
    let enc = legacy.join("secret_2024-05-04_08-00-00");
    fs::create_dir_all(&enc).unwrap();
    fs::write(
        enc.join("submission.xml"),
        r#"<data id="secret" encrypted="yes" xmlns="http://www.opendatakit.org/xforms/encrypted"><base64EncryptedKey>K</base64EncryptedKey><meta xmlns="http://openrosa.org/xforms"><instanceID>uuid:enc</instanceID></meta><encryptedXmlFile>submission.xml.enc</encryptedXmlFile></data>"#,
    )
    .unwrap();
    fs::write(enc.join("submission.xml.enc"), b"\x00\x01\x02").unwrap();
    // Junk that is not an instance.
    fs::create_dir_all(legacy.join("broken_1")).unwrap();
    fs::write(legacy.join("broken_1/broken_1.xml"), "<not closed").unwrap();
    root
}

fn run(args: &[&str]) -> (i32, String) {
    let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
    let mut out = Vec::new();
    let code = odk_sneakernet::run(&args, &mut out);
    (code, String::from_utf8(out).unwrap())
}

#[test]
fn scan_reports_status_duplicates_and_missing_files() {
    let root = fixture();
    let (code, out) = run(&["scan", root.to_str().unwrap()]);
    assert_eq!(code, 1, "{out}");
    assert!(out.contains("read status from"), "{out}");
    let hh = out.lines().find(|l| l.starts_with("hh ")).unwrap();
    let cols: Vec<&str> = hh.split_whitespace().collect();
    // form finalized draft sent unknown encrypted missing versions
    assert_eq!(cols, vec!["hh", "2", "1", "1", "4", "0", "1", "3"], "{out}");
    let secret = out.lines().find(|l| l.starts_with("secret ")).unwrap();
    assert!(secret.split_whitespace().nth(5) == Some("1"), "{out}");
    assert!(
        out.contains("9 submission folders, 6 unique submissions"),
        "{out}"
    );
    assert!(out.contains("1 identical copies"), "{out}");
    assert!(
        out.contains("CONFLICT: uuid:3 has 2 different versions"),
        "{out}"
    );
    assert!(
        out.contains("MISSING FILES") && out.contains("gone.jpg"),
        "{out}"
    );
    assert!(out.contains("broken_1.xml: invalid XML"), "{out}");
}

#[test]
fn scan_without_database_notes_it() {
    let root = fixture();
    let (_, out) = run(&["scan", root.join("phone2").to_str().unwrap()]);
    assert!(out.contains("no metadata/instances.db found"), "{out}");
}

#[derive(Debug, Clone)]
struct Req {
    method: String,
    path: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

/// Minimal HTTP/1.1 server: HEAD -> 204 with a size limit, POST -> 201,
/// except the submission with instanceID `conflict_id`, which gets 409.
fn mock_server(limit: u64, conflict_id: &'static str) -> (String, Arc<Mutex<Vec<Req>>>) {
    mock_server_forbidding(limit, conflict_id, "none")
}

/// Like `mock_server`, but submissions of form `forbidden_form` get 403, as
/// Central answers when an App User has no access to that form.
fn mock_server_forbidding(
    limit: u64,
    conflict_id: &'static str,
    forbidden_form: &'static str,
) -> (String, Arc<Mutex<Vec<Req>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let log = Arc::new(Mutex::new(Vec::new()));
    let log2 = log.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut line = String::new();
            if reader.read_line(&mut line).unwrap_or(0) == 0 {
                continue;
            }
            let mut parts = line.split_whitespace();
            let method = parts.next().unwrap_or("").to_string();
            let path = parts.next().unwrap_or("").to_string();
            let mut headers = Vec::new();
            let mut len = 0usize;
            loop {
                let mut h = String::new();
                reader.read_line(&mut h).unwrap();
                let h = h.trim_end();
                if h.is_empty() {
                    break;
                }
                let (k, v) = h.split_once(':').unwrap();
                let (k, v) = (k.trim().to_ascii_lowercase(), v.trim().to_string());
                if k == "content-length" {
                    len = v.parse().unwrap();
                }
                headers.push((k, v));
            }
            let mut body = vec![0; len];
            reader.read_exact(&mut body).unwrap();
            let text = String::from_utf8_lossy(&body).to_string();
            let response = if method == "HEAD" {
                format!(
                    "HTTP/1.1 204 No Content\r\nX-OpenRosa-Version: 1.0\r\nX-OpenRosa-Accept-Content-Length: {limit}\r\nConnection: close\r\n\r\n"
                )
            } else if text.contains(&format!("id=\"{forbidden_form}\"")) {
                let b = "<OpenRosaResponse xmlns=\"http://openrosa.org/http/response\"><message nature=\"error\">The authenticated actor does not have rights to perform that action.</message></OpenRosaResponse>";
                format!(
                    "HTTP/1.1 403 Forbidden\r\nContent-Type: text/xml\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{b}",
                    b.len()
                )
            } else if text.contains(&format!(">{conflict_id}<")) {
                let b = "<OpenRosaResponse xmlns=\"http://openrosa.org/http/response\"><message nature=\"error\">A submission already exists with this ID, but with different XML.</message></OpenRosaResponse>";
                format!(
                    "HTTP/1.1 409 Conflict\r\nContent-Type: text/xml\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{b}",
                    b.len()
                )
            } else {
                let b = "<OpenRosaResponse xmlns=\"http://openrosa.org/http/response\"><message>full submission upload was successful!</message></OpenRosaResponse>";
                format!(
                    "HTTP/1.1 201 Created\r\nContent-Type: text/xml\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{b}",
                    b.len()
                )
            };
            log2.lock().unwrap().push(Req {
                method,
                path,
                headers,
                body,
            });
            let _ = stream.write_all(response.as_bytes());
            let _ = stream.flush();
        }
    });
    (format!("http://{addr}/v1/key/SECRETTOKEN/projects/3"), log)
}

fn part_names(body: &[u8]) -> Vec<String> {
    let text = String::from_utf8_lossy(body);
    text.match_indices("form-data; name=\"")
        .map(|(i, _)| {
            let rest = &text[i + 17..];
            rest[..rest.find('"').unwrap()].to_string()
        })
        .collect()
}

#[test]
fn push_sends_finalized_once_splits_large_and_resumes() {
    let root = fixture();
    let (url, reqs) = mock_server(1500, "uuid:2");
    let logfile = root.join("log.csv");
    let (code, out) = run(&[
        "push",
        "--server",
        &url,
        "--log",
        logfile.to_str().unwrap(),
        root.to_str().unwrap(),
    ]);
    // uuid:2 conflicts on the server, uuid:3 conflicts locally -> exit 1.
    assert_eq!(code, 1, "{out}");
    assert!(out.contains("skipping 1 draft"), "{out}");
    assert!(out.contains("skipping 1 sent"), "{out}");
    assert!(out.contains("CONFLICT: uuid:3"), "{out}");
    assert!(
        out.contains("done: 3 created, 1 conflicts, 0 rejected"),
        "{out}"
    );

    let reqs = reqs.lock().unwrap().clone();
    assert_eq!(reqs[0].method, "HEAD");
    assert_eq!(reqs[0].path, "/v1/key/SECRETTOKEN/projects/3/submission");
    let posts: Vec<&Req> = reqs.iter().filter(|r| r.method == "POST").collect();
    // uuid:1, uuid:2, uuid:4 (split in two), uuid:enc
    assert_eq!(posts.len(), 5);
    for p in &posts {
        assert!(
            p.headers
                .iter()
                .any(|(k, v)| k == "x-openrosa-version" && v == "1.0")
        );
        assert_eq!(part_names(&p.body)[0], "xml_submission_file");
    }
    let bodies: Vec<String> = posts
        .iter()
        .map(|p| String::from_utf8_lossy(&p.body).to_string())
        .collect();
    let one: Vec<&String> = bodies.iter().filter(|b| b.contains(">uuid:1<")).collect();
    assert_eq!(one.len(), 1, "identical copies must be sent once");
    assert_eq!(
        part_names(one[0].as_bytes()),
        vec!["xml_submission_file", "p1.jpg"]
    );
    let four: Vec<&Req> = posts
        .iter()
        .copied()
        .filter(|p| String::from_utf8_lossy(&p.body).contains(">uuid:4<"))
        .collect();
    assert_eq!(four.len(), 2);
    assert_eq!(
        part_names(&four[0].body),
        vec!["xml_submission_file", "a.jpg", "*isIncomplete*"]
    );
    assert_eq!(
        part_names(&four[1].body),
        vec!["xml_submission_file", "b.jpg"]
    );
    let enc = bodies.iter().find(|b| b.contains(">uuid:enc<")).unwrap();
    assert!(enc.contains("name=\"submission.xml.enc\""));
    assert!(enc.contains("application/octet-stream"));
    assert!(
        !bodies
            .iter()
            .any(|b| b.contains("uuid:draft") || b.contains("uuid:sent"))
    );

    let log = fs::read_to_string(&logfile).unwrap();
    assert!(!log.contains("SECRETTOKEN"), "token must not be logged");
    assert_eq!(log.matches(",created,").count(), 3);

    // Second run: everything accepted is skipped; only the conflict is retried.
    let (_, out2) = run(&[
        "push",
        "--server",
        &url,
        "--log",
        logfile.to_str().unwrap(),
        "--dry-run",
        root.to_str().unwrap(),
    ]);
    assert!(out2.contains("3 submissions already uploaded"), "{out2}");
    assert!(
        out2.contains("dry run: 1 submissions would be sent"),
        "{out2}"
    );
    assert!(out2.contains("uuid:2"), "{out2}");
}

#[test]
fn push_with_basic_auth_and_options() {
    let root = fixture();
    let (url, reqs) = mock_server(10_000_000, "none");
    let logfile = root.join("log.csv");
    // SAFETY: tests in this file do not read ODK_PASSWORD concurrently except this one.
    unsafe { std::env::set_var("ODK_PASSWORD", "p4ss") };
    let (code, out) = run(&[
        "push",
        "--server",
        &url,
        "--user",
        "user@x.org",
        "--skip-unknown",
        "--include-sent",
        "--log",
        logfile.to_str().unwrap(),
        root.join("phone1").to_str().unwrap(),
    ]);
    // uuid:2 lacks gone.jpg: sent, but partial (exit 1) so it is retried.
    assert_eq!(code, 1, "{out}");
    assert!(out.contains("done: 2 created"), "{out}");
    assert!(out.contains("1 sent without missing files"), "{out}");
    let reqs = reqs.lock().unwrap();
    assert!(reqs.iter().all(|r| {
        r.headers
            .iter()
            .any(|(k, v)| k == "authorization" && v == "Basic dXNlckB4Lm9yZzpwNHNz")
    }));
}

#[test]
fn push_stops_on_bad_url() {
    let root = fixture();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        for s in listener.incoming() {
            let mut s = s.unwrap();
            let mut buf = [0u8; 4096];
            let _ = s.read(&mut buf);
            let _ = s.write_all(
                b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            );
        }
    });
    let (code, out) = run(&[
        "push",
        "--server",
        &format!("http://{addr}/wrong"),
        "--log",
        root.join("l.csv").to_str().unwrap(),
        root.to_str().unwrap(),
    ]);
    assert_eq!(code, 2, "{out}");
    assert!(out.contains("no submission endpoint"), "{out}");
}

#[test]
fn csv_export_uses_form_definition() {
    let root = fixture();
    let outdir = root.join("csv");
    let (code, out) = run(&[
        "csv",
        "--out",
        outdir.to_str().unwrap(),
        root.join("phone1").to_str().unwrap(),
    ]);
    assert_eq!(code, 0, "{out}");
    let main = fs::read_to_string(outdir.join("hh.csv")).unwrap();
    let lines: Vec<&str> = main.lines().collect();
    assert_eq!(lines[0], "village,photo,meta-instanceID,KEY");
    // finalized x2 + sent; the draft is left out
    assert_eq!(lines.len(), 4, "{main}");
    assert!(!main.contains("Draft"));
    let rep = fs::read_to_string(outdir.join("hh-household-member.csv")).unwrap();
    assert!(
        rep.starts_with(
            "name,PARENT_KEY,KEY\nA,uuid:1,uuid:1/member[1]\nB,uuid:1,uuid:1/member[2]\n"
        ),
        "{rep}"
    );
}

#[test]
fn usage_errors() {
    assert_eq!(run(&[]).0, 2);
    assert_eq!(run(&["--help"]).0, 0);
    assert_eq!(run(&["scan"]).0, 2);
    assert_eq!(run(&["frob", "x"]).0, 2);
    assert_eq!(run(&["push", "x"]).0, 2);
    assert_eq!(run(&["scan", "--bogus", "x"]).0, 2);
    assert!(run(&["-V"]).1.starts_with("odk-sneakernet "));
}

#[test]
fn push_continues_past_form_the_app_user_cannot_access() {
    // Central answers 403 for a form the App User has no access to. The other
    // forms must still be uploaded and the 403 reported as a rejection.
    let root = fixture();
    let (url, reqs) = mock_server_forbidding(10_000_000, "none", "secret");
    let logfile = root.join("log.csv");
    let (code, out) = run(&[
        "push",
        "--server",
        &url,
        "--log",
        logfile.to_str().unwrap(),
        root.join("phone2").to_str().unwrap(),
    ]);
    assert_eq!(code, 1, "{out}");
    assert!(out.contains("rejected"), "{out}");
    assert!(out.contains("HTTP 403"), "{out}");
    // uuid:1 and uuid:4 are sent even though uuid:enc (form secret) is refused.
    let reqs = reqs.lock().unwrap();
    let posts = reqs.iter().filter(|r| r.method == "POST").count();
    assert_eq!(posts, 3, "{out}");
    assert!(
        out.contains("done: 2 created, 0 conflicts, 1 rejected"),
        "{out}"
    );
}

#[test]
fn push_exit_code_flags_unreadable_submissions() {
    let root = tempdir("unreadable");
    let inst = root.join("odk/instances");
    put_instance(
        &inst,
        "hh_2024-05-01_09-00-00",
        &submission("uuid:ok", "Kisu", None),
        &[],
    );
    fs::create_dir_all(inst.join("broken_1")).unwrap();
    fs::write(inst.join("broken_1/broken_1.xml"), "<not closed").unwrap();
    let (url, _) = mock_server(10_000_000, "none");
    let (code, out) = run(&[
        "push",
        "--server",
        &url,
        "--log",
        root.join("log.csv").to_str().unwrap(),
        root.to_str().unwrap(),
    ]);
    assert!(out.contains("done: 1 created"), "{out}");
    assert_eq!(code, 1, "unreadable submission must give exit 1: {out}");
}

#[test]
fn csv_exit_code_flags_conflicts_and_unreadable() {
    let root = fixture();
    let outdir = root.join("csv");
    let (code, out) = run(&[
        "csv",
        "--out",
        outdir.to_str().unwrap(),
        root.to_str().unwrap(),
    ]);
    assert!(out.contains("CONFLICT: uuid:3"), "{out}");
    assert_eq!(code, 1, "{out}");
}

#[test]
fn draft_then_finalized_copy_of_same_phone_is_not_a_conflict() {
    // The same phone copied twice: first while the form was a draft, later
    // after it was finalized. The finalized version must be sent.
    let root = tempdir("recopy");
    for (copy, name, status) in [
        ("week1", "Draft text", "incomplete"),
        ("week2", "Final text", "complete"),
    ] {
        let project = root.join(copy).join("projects/p");
        put_instance(
            &project.join("instances"),
            "hh_2024-05-01_09-00-00",
            &submission("uuid:same", name, None),
            &[],
        );
        fs::create_dir_all(project.join("metadata")).unwrap();
        let conn = rusqlite::Connection::open(project.join("metadata/instances.db")).unwrap();
        conn.execute_batch(&format!(
            "CREATE TABLE instances (_id INTEGER PRIMARY KEY, instanceFilePath TEXT, status TEXT);
             INSERT INTO instances (instanceFilePath, status) VALUES
               ('hh_2024-05-01_09-00-00/hh_2024-05-01_09-00-00.xml', '{status}');"
        ))
        .unwrap();
    }
    let (url, reqs) = mock_server(10_000_000, "none");
    let (code, out) = run(&[
        "push",
        "--server",
        &url,
        "--log",
        root.join("log.csv").to_str().unwrap(),
        root.to_str().unwrap(),
    ]);
    assert_eq!(code, 0, "{out}");
    assert!(!out.contains("CONFLICT"), "{out}");
    let reqs = reqs.lock().unwrap();
    let posts: Vec<&Req> = reqs.iter().filter(|r| r.method == "POST").collect();
    assert_eq!(posts.len(), 1, "{out}");
    assert!(String::from_utf8_lossy(&posts[0].body).contains("Final text"));
}

fn posts_of(reqs: &Arc<Mutex<Vec<Req>>>) -> Vec<String> {
    reqs.lock()
        .unwrap()
        .iter()
        .filter(|r| r.method == "POST")
        .map(|r| String::from_utf8_lossy(&r.body).to_string())
        .collect()
}

#[test]
fn push_missing_attachment_is_retried_until_present() {
    // A submission whose photo was not copied is sent, but logged as partial
    // so that a later run (after copying the photo) sends it again.
    let root = tempdir("missing");
    let inst = root.join("odk/instances");
    put_instance(
        &inst,
        "hh_1",
        &submission("uuid:m", "Kisu", Some("p.jpg")),
        &[],
    );
    let (url, reqs) = mock_server(10_000_000, "none");
    let logfile = root.join("log.csv");
    let push = |extra: &[&str]| {
        let mut a = vec!["push", "--server", &url, "--log", logfile.to_str().unwrap()];
        a.extend_from_slice(extra);
        a.push(root.to_str().unwrap());
        run(&a)
    };
    let (_, dry) = push(&["--dry-run"]);
    assert!(dry.contains("warning:") && dry.contains("p.jpg"), "{dry}");
    let (code, out) = push(&[]);
    assert_eq!(code, 1, "{out}");
    assert!(out.contains("p.jpg") && out.contains("partial"), "{out}");
    let log = fs::read_to_string(&logfile).unwrap();
    assert!(!log.contains(",created,"), "{log}");
    fs::write(inst.join("hh_1/p.jpg"), b"jpg").unwrap();
    let (code, out) = push(&[]);
    assert_eq!(code, 0, "{out}");
    let posts = posts_of(&reqs);
    assert_eq!(posts.len(), 2, "{out}");
    assert!(posts[1].contains("name=\"p.jpg\""));
    let (_, out) = push(&[]);
    assert!(out.contains("nothing to send"), "{out}");
}

#[test]
fn push_refuses_basic_auth_over_plain_http() {
    let root = fixture();
    let (code, out) = run(&[
        "push",
        "--server",
        "http://central.example.org/v1/projects/3",
        "--user",
        "me@x.org",
        root.to_str().unwrap(),
    ]);
    assert_eq!(code, 2, "{out}");
    assert!(out.contains("https://"), "{out}");
}

#[test]
fn encrypted_submission_sends_only_manifest_files() {
    let root = tempdir("enc");
    let d = root.join("odk/instances/secret_1");
    fs::create_dir_all(&d).unwrap();
    // Collect renames the manifest to the instance's own XML name.
    fs::write(
        d.join("secret_1.xml"),
        r#"<data id="secret" encrypted="yes" xmlns="http://www.opendatakit.org/xforms/encrypted"><base64EncryptedKey>K</base64EncryptedKey><meta xmlns="http://openrosa.org/xforms"><instanceID>uuid:e</instanceID></meta><media><file>a.jpg.enc</file></media><encryptedXmlFile>submission.xml.enc</encryptedXmlFile></data>"#,
    )
    .unwrap();
    for f in ["submission.xml.enc", "a.jpg.enc", "a.jpg", "stray.enc"] {
        fs::write(d.join(f), b"x").unwrap();
    }
    let (url, reqs) = mock_server(10_000_000, "none");
    let (code, out) = run(&[
        "push",
        "--server",
        &url,
        "--log",
        root.join("log.csv").to_str().unwrap(),
        root.to_str().unwrap(),
    ]);
    assert_eq!(code, 0, "{out}");
    let posts = posts_of(&reqs);
    let mut names = part_names(posts[0].as_bytes());
    names.sort();
    assert_eq!(
        names,
        vec!["a.jpg.enc", "submission.xml.enc", "xml_submission_file"]
    );
}

#[test]
fn dry_run_exit_code_flags_conflicts() {
    let root = fixture();
    let (code, out) = run(&[
        "push",
        "--server",
        "https://central.example.org/v1/key/T/projects/3",
        "--dry-run",
        "--log",
        root.join("log.csv").to_str().unwrap(),
        root.to_str().unwrap(),
    ]);
    assert!(out.contains("CONFLICT: uuid:3"), "{out}");
    assert_eq!(code, 1, "{out}");
}
