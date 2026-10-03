//! odk-sneakernet: get ODK Collect submissions that were copied off phones
//! into ODK Central, or into CSV, without ODK Briefcase.

pub mod csvout;
pub mod export;
pub mod instance;
pub mod push;

use instance::{Instance, Status};
use std::collections::BTreeMap;
use std::io::Write;
use std::path::PathBuf;

pub const USAGE: &str = "\
odk-sneakernet: upload or export ODK Collect submissions copied off phones

USAGE:
  odk-sneakernet scan PATH...
  odk-sneakernet push --server URL [--user EMAIL] [options] PATH...
  odk-sneakernet csv --out DIR [--forms DIR] [options] PATH...

PATH is any folder holding copied Collect data: a phone's `projects/<uuid>`
folder, an `instances` folder, a legacy `odk` folder, or a folder of several
phones. Subfolders are searched. Copy `metadata/instances.db` along with
`instances` so drafts can be told apart from finalized forms.

push options:
  --server URL        Server URL as configured in Collect, e.g.
                      https://central.example.org/v1/key/TOKEN/projects/3
                      Defaults to $ODK_SERVER. Prefer the variable: an App
                      User URL contains a secret token, and command-line
                      arguments show up in `ps` and shell history.
  --user EMAIL        Basic auth user (password from ODK_PASSWORD or stdin).
                      Not needed with an App User URL. Needs https://.
  --log FILE          Upload log used to resume (default odk-sneakernet-log.csv)
  --dry-run           Show what would be sent, send nothing
  --skip-unknown      Do not send instances whose status is unknown

csv options:
  --out DIR           Folder to write CSV files into
  --forms DIR         Folder with blank form XML (to find repeat groups);
                      `forms` folders next to `instances` are used automatically

common options:
  --include-drafts    Also include instances Collect marks as drafts
  --include-sent      Also include instances Collect already sent (push only)
  -h, --help          Show this help
  -V, --version       Show version
";

#[derive(Debug, Default)]
struct Opts {
    paths: Vec<PathBuf>,
    server: Option<String>,
    user: Option<String>,
    log: Option<PathBuf>,
    out: Option<PathBuf>,
    forms: Vec<PathBuf>,
    dry_run: bool,
    skip_unknown: bool,
    include_drafts: bool,
    include_sent: bool,
}

fn parse_opts(args: &[String]) -> Result<Opts, String> {
    let mut o = Opts::default();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut val = |name: &str| {
            it.next()
                .cloned()
                .ok_or_else(|| format!("{name} needs a value"))
        };
        match a.as_str() {
            "--server" => o.server = Some(val("--server")?),
            "--user" => o.user = Some(val("--user")?),
            "--log" => o.log = Some(PathBuf::from(val("--log")?)),
            "--out" => o.out = Some(PathBuf::from(val("--out")?)),
            "--forms" => o.forms.push(PathBuf::from(val("--forms")?)),
            "--dry-run" => o.dry_run = true,
            "--skip-unknown" => o.skip_unknown = true,
            "--include-drafts" => o.include_drafts = true,
            "--include-sent" => o.include_sent = true,
            s if s.starts_with('-') => return Err(format!("unknown option {s}")),
            s => o.paths.push(PathBuf::from(s)),
        }
    }
    if o.paths.is_empty() {
        return Err("give at least one folder to read".into());
    }
    Ok(o)
}

/// Run the CLI. Returns the process exit code.
pub fn run(args: &[String], out: &mut dyn Write) -> i32 {
    let Some(cmd) = args.first() else {
        let _ = write!(out, "{USAGE}");
        return 2;
    };
    let rest = &args[1..];
    if matches!(cmd.as_str(), "-h" | "--help" | "help") {
        let _ = write!(out, "{USAGE}");
        return 0;
    }
    if matches!(cmd.as_str(), "-V" | "--version") {
        let _ = writeln!(out, "odk-sneakernet {}", env!("CARGO_PKG_VERSION"));
        return 0;
    }
    if rest.iter().any(|a| a == "-h" || a == "--help") {
        let _ = write!(out, "{USAGE}");
        return 0;
    }
    let opts = match parse_opts(rest) {
        Ok(o) => o,
        Err(e) => {
            let _ = writeln!(out, "error: {e}\n\n{USAGE}");
            return 2;
        }
    };
    let result = match cmd.as_str() {
        "scan" => cmd_scan(&opts, out),
        "push" => cmd_push(&opts, out),
        "csv" => cmd_csv(&opts, out),
        other => Err(format!("unknown command {other}")),
    };
    match result {
        Ok(code) => code,
        Err(e) => {
            let _ = writeln!(out, "error: {e}");
            2
        }
    }
}

type R = Result<i32, String>;

fn io(e: std::io::Error) -> String {
    e.to_string()
}

fn report_problems(inv: &instance::Inventory, out: &mut dyn Write) -> std::io::Result<()> {
    for p in &inv.problems {
        writeln!(out, "warning: {}: {}", p.path.display(), p.message)?;
    }
    Ok(())
}

fn cmd_scan(o: &Opts, out: &mut dyn Write) -> R {
    let inv = instance::scan(&o.paths);
    report_problems(&inv, out).map_err(io)?;
    if inv.databases.is_empty() {
        writeln!(
            out,
            "note: no metadata/instances.db found, so drafts cannot be told apart from finalized forms"
        )
        .map_err(io)?;
    } else {
        for d in &inv.databases {
            writeln!(out, "read status from {}", d.display()).map_err(io)?;
        }
    }
    let total = inv.instances.len();
    let mut forms: BTreeMap<String, [usize; 6]> = BTreeMap::new();
    let mut versions: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for i in &inv.instances {
        let c = forms.entry(i.form_id.clone()).or_default();
        c[match i.status {
            Status::Finalized => 0,
            Status::Draft => 1,
            Status::Sent => 2,
            Status::Unknown => 3,
        }] += 1;
        if i.encrypted {
            c[4] += 1;
        }
        if !i.missing.is_empty() {
            c[5] += 1;
        }
        let v = versions.entry(i.form_id.clone()).or_default();
        let ver = i.version.clone().unwrap_or_else(|| "-".into());
        if !v.contains(&ver) {
            v.push(ver);
        }
    }
    writeln!(
        out,
        "\n{:<28} {:>9} {:>6} {:>6} {:>8} {:>9} {:>14}  versions",
        "form", "finalized", "draft", "sent", "unknown", "encrypted", "missing files"
    )
    .map_err(io)?;
    for (f, c) in &forms {
        writeln!(
            out,
            "{:<28} {:>9} {:>6} {:>6} {:>8} {:>9} {:>14}  {}",
            f,
            c[0],
            c[1],
            c[2],
            c[3],
            c[4],
            c[5],
            versions[f].join(" ")
        )
        .map_err(io)?;
    }
    let all: Vec<Instance> = inv.instances.clone();
    let missing: Vec<&Instance> = all.iter().filter(|i| !i.missing.is_empty()).collect();
    let no_id = all.iter().filter(|i| i.instance_id.is_none()).count();
    let d = instance::dedupe(all.clone());
    writeln!(
        out,
        "\n{total} submission folders, {} unique submissions",
        d.unique.len()
    )
    .map_err(io)?;
    if !d.copies.is_empty() {
        writeln!(
            out,
            "{} identical copies of the same submission (sent once)",
            d.copies.len()
        )
        .map_err(io)?;
    }
    if no_id > 0 {
        writeln!(out, "{no_id} submissions have no instanceID").map_err(io)?;
    }
    for (id, group) in &d.conflicts {
        writeln!(
            out,
            "CONFLICT: {id} has {} different versions, not sent:",
            group.len()
        )
        .map_err(io)?;
        for g in group {
            writeln!(out, "  {}", g.dir.display()).map_err(io)?;
        }
    }
    for m in &missing {
        writeln!(
            out,
            "MISSING FILES in {}: {}",
            m.dir.display(),
            m.missing.join(", ")
        )
        .map_err(io)?;
    }
    Ok(if d.conflicts.is_empty() && inv.problems.is_empty() {
        0
    } else {
        1
    })
}

/// Pick which instances an export or upload should include.
fn select(
    insts: Vec<Instance>,
    o: &Opts,
    push: bool,
) -> (Vec<Instance>, BTreeMap<&'static str, usize>) {
    let mut skipped: BTreeMap<&'static str, usize> = BTreeMap::new();
    let keep = insts
        .into_iter()
        .filter(|i| {
            let ok = match i.status {
                Status::Finalized => true,
                Status::Draft => o.include_drafts,
                Status::Sent => !push || o.include_sent,
                Status::Unknown => !(push && o.skip_unknown),
            };
            if !ok {
                *skipped.entry(i.status.label()).or_default() += 1;
            }
            ok
        })
        .collect();
    (keep, skipped)
}

fn cmd_push(o: &Opts, out: &mut dyn Write) -> R {
    let server = o
        .server
        .clone()
        .or_else(|| std::env::var("ODK_SERVER").ok().filter(|s| !s.is_empty()))
        .ok_or("push needs --server or ODK_SERVER (the server URL configured in Collect)")?;
    if !server.starts_with("http://") && !server.starts_with("https://") {
        return Err("--server must start with https:// (or http://)".into());
    }
    if o.user.is_some() && server.starts_with("http://") && !is_loopback(&server) {
        return Err("--user sends the password in clear text over http://; use https://".into());
    }
    let inv = instance::scan(&o.paths);
    report_problems(&inv, out).map_err(io)?;
    let problems = inv.problems.len();
    let d = instance::dedupe(inv.instances);
    for (id, g) in &d.conflicts {
        writeln!(
            out,
            "CONFLICT: {id} exists in {} different versions; not sent. Check: {}",
            g.len(),
            g.iter()
                .map(|i| i.dir.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        )
        .map_err(io)?;
    }
    let (todo, skipped) = select(d.unique, o, true);
    for (label, n) in &skipped {
        writeln!(out, "skipping {n} {label} submissions").map_err(io)?;
    }
    let unknown = todo.iter().filter(|i| i.status == Status::Unknown).count();
    if unknown > 0 {
        writeln!(
            out,
            "warning: {unknown} submissions have unknown status (no instances.db); drafts may be among them. Use --skip-unknown to leave them out."
        )
        .map_err(io)?;
    }
    let logged = push::redact_url(&server);
    let log = push::Log::new(
        o.log
            .as_deref()
            .unwrap_or("odk-sneakernet-log.csv".as_ref()),
    );
    let done = log.done(&logged);
    let todo: Vec<Instance> = todo
        .into_iter()
        .filter(|i| i.instance_id.as_ref().is_none_or(|id| !done.contains(id)))
        .collect();
    if !done.is_empty() {
        writeln!(
            out,
            "{} submissions already uploaded according to the log",
            done.len()
        )
        .map_err(io)?;
    }
    for i in todo.iter().filter(|i| !i.missing.is_empty()) {
        writeln!(
            out,
            "warning: {} is missing {}; it is sent without them and logged as partial, so a later run retries it",
            i.dir.display(),
            i.missing.join(", ")
        )
        .map_err(io)?;
    }
    if o.dry_run {
        for i in &todo {
            writeln!(
                out,
                "would send {} ({}, {} files) from {}",
                i.instance_id.as_deref().unwrap_or("(no instanceID)"),
                i.form_id,
                i.attachments.len(),
                i.dir.display()
            )
            .map_err(io)?;
        }
        writeln!(out, "dry run: {} submissions would be sent", todo.len()).map_err(io)?;
        return Ok(if d.conflicts.is_empty() && problems == 0 {
            0
        } else {
            1
        });
    }
    if todo.is_empty() {
        writeln!(out, "nothing to send").map_err(io)?;
        return Ok(if d.conflicts.is_empty() && problems == 0 {
            0
        } else {
            1
        });
    }
    let password = match &o.user {
        Some(_) => Some(match std::env::var("ODK_PASSWORD") {
            Ok(p) => p,
            Err(_) => {
                eprint!("password (input is visible; set ODK_PASSWORD to avoid this): ");
                let mut line = String::new();
                std::io::stdin().read_line(&mut line).map_err(io)?;
                line.trim_end_matches(['\r', '\n']).to_string()
            }
        }),
        None => None,
    };
    let mut client = push::Client::new(&server, o.user.as_deref(), password.as_deref());
    client.preflight().map_err(|e| e.to_string())?;
    let (mut ok, mut partial, mut conflict, mut failed) = (0, 0, 0, 0);
    for (n, inst) in todo.iter().enumerate() {
        let id = inst.instance_id.as_deref().unwrap_or("(no instanceID)");
        let (result, message) = match client.send(inst) {
            // Central keeps the submission and accepts the missing files later
            // (a re-post with identical XML adds attachments), so a later run
            // must try again: never log it as `created`.
            Ok(push::Outcome::Created) if !inst.missing.is_empty() => {
                partial += 1;
                ("partial", format!("missing {}", inst.missing.join(" ")))
            }
            Ok(push::Outcome::Created) => {
                ok += 1;
                ("created", String::new())
            }
            Ok(push::Outcome::Conflict(m)) => {
                conflict += 1;
                ("conflict", m)
            }
            Ok(push::Outcome::Rejected(code, m)) => {
                failed += 1;
                ("rejected", format!("HTTP {code}: {m}"))
            }
            Ok(push::Outcome::Unreadable(m)) => {
                failed += 1;
                ("unreadable", m)
            }
            Err(e) => {
                writeln!(out, "stopped at {id}: {e}").map_err(io)?;
                writeln!(
                    out,
                    "sent {ok} of {}. Run the same command again to continue.",
                    todo.len()
                )
                .map_err(io)?;
                return Ok(1);
            }
        };
        log.record(&logged, inst, result, &message).map_err(io)?;
        writeln!(
            out,
            "[{}/{}] {result:<8} {id} ({}) {message}",
            n + 1,
            todo.len(),
            inst.form_id
        )
        .map_err(io)?;
    }
    writeln!(
        out,
        "\ndone: {ok} created, {conflict} conflicts, {failed} rejected"
    )
    .map_err(io)?;
    if partial > 0 {
        writeln!(
            out,
            "{partial} sent without missing files (logged as partial; run again once the files are copied)"
        )
        .map_err(io)?;
    }
    Ok(
        if conflict + failed + partial == 0 && d.conflicts.is_empty() && problems == 0 {
            0
        } else {
            1
        },
    )
}

fn cmd_csv(o: &Opts, out: &mut dyn Write) -> R {
    let dir = o.out.clone().ok_or("csv needs --out DIR")?;
    let inv = instance::scan(&o.paths);
    report_problems(&inv, out).map_err(io)?;
    let d = instance::dedupe(inv.instances);
    for (id, _) in &d.conflicts {
        writeln!(out, "CONFLICT: {id} has differing copies; left out").map_err(io)?;
    }
    let (todo, skipped) = select(d.unique, o, false);
    for (label, n) in &skipped {
        writeln!(out, "skipping {n} {label} submissions").map_err(io)?;
    }
    let (files, encrypted) = export::export(&todo, &o.forms, &dir)?;
    if encrypted > 0 {
        writeln!(
            out,
            "skipped {encrypted} encrypted submissions (upload them to Central to decrypt)"
        )
        .map_err(io)?;
    }
    for (f, rows) in &files {
        writeln!(out, "wrote {} ({rows} rows)", f.display()).map_err(io)?;
    }
    Ok(if d.conflicts.is_empty() && inv.problems.is_empty() {
        0
    } else {
        1
    })
}

/// True for `http://localhost`, `127.x` and `[::1]` URLs, where clear text
/// never leaves the machine.
fn is_loopback(url: &str) -> bool {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    let authority = rest.split('/').next().unwrap_or("");
    let host = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    let host = if host.starts_with('[') {
        host.split(']').next().unwrap_or("").trim_start_matches('[')
    } else {
        host.split(':').next().unwrap_or("")
    };
    host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback())
}

#[cfg(test)]
mod tests {
    #[test]
    fn loopback_hosts() {
        assert!(super::is_loopback("http://127.0.0.1:8080/v1"));
        assert!(super::is_loopback("http://localhost/x"));
        assert!(super::is_loopback("http://[::1]:80/x"));
        assert!(!super::is_loopback("http://central.example.org/v1"));
        assert!(!super::is_loopback("http://127.0.0.1.evil.org/v1"));
    }
}
