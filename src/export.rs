//! Offline CSV export of submissions, one file per form and per repeat group.
//!
//! Column names follow ODK Central's CSV export: group names joined with `-`,
//! a `KEY` column holding the instanceID, and repeat tables linked through
//! `PARENT_KEY`/`KEY` values such as `uuid:…/member[2]`.

use crate::instance::Instance;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

/// Rows and columns for one output file.
#[derive(Debug, Default, Clone)]
pub struct Table {
    pub columns: Vec<String>,
    index: HashMap<String, usize>,
    pub rows: Vec<Vec<String>>,
}

impl Table {
    fn col(&mut self, name: &str) -> usize {
        if let Some(&i) = self.index.get(name) {
            return i;
        }
        self.columns.push(name.to_string());
        self.index.insert(name.to_string(), self.columns.len() - 1);
        self.columns.len() - 1
    }

    fn set(&mut self, row: usize, name: &str, value: &str) {
        let c = self.col(name);
        let r = &mut self.rows[row];
        if r.len() <= c {
            r.resize(c + 1, String::new());
        }
        if r[c].is_empty() {
            r[c] = value.to_string();
        } else {
            // A repeated leaf in a non-repeat position: keep every value.
            r[c] = format!("{} {}", r[c], value);
        }
    }

    fn new_row(&mut self) -> usize {
        self.rows.push(Vec::new());
        self.rows.len() - 1
    }

    /// Render as CSV text with a header line.
    pub fn to_csv(&self) -> String {
        let mut s = crate::csvout::join(&self.columns);
        s.push('\n');
        for r in &self.rows {
            let mut full: Vec<String> = r.iter().map(|v| defuse(v)).collect();
            full.resize(self.columns.len(), String::new());
            s.push_str(&crate::csvout::join(&full));
            s.push('\n');
        }
        s
    }
}

/// Prefix values a spreadsheet would run as a formula with `'`.
fn defuse(v: &str) -> String {
    if v.starts_with(['=', '+', '@', '\t', '\r']) {
        format!("'{v}")
    } else {
        v.to_string()
    }
}

/// Repeat group paths (relative to the root, e.g. `household/member`) for
/// each form id, read from blank form definitions found in `dirs`.
pub fn repeats_from_forms(dirs: &[PathBuf]) -> HashMap<String, HashSet<String>> {
    let mut out: HashMap<String, HashSet<String>> = HashMap::new();
    for dir in dirs {
        let Ok(entries) = fs::read_dir(dir) else {
            continue;
        };
        for e in entries.filter_map(Result::ok) {
            let p = e.path();
            if p.extension().and_then(|x| x.to_str()) != Some("xml") {
                continue;
            }
            if let Ok(text) = fs::read_to_string(&p)
                && let Some((id, reps)) = parse_form_repeats(&text)
            {
                out.entry(id).or_default().extend(reps);
            }
        }
    }
    out
}

/// Parse an XForm definition: returns its form id and repeat paths.
pub fn parse_form_repeats(xml: &str) -> Option<(String, HashSet<String>)> {
    let doc = roxmltree::Document::parse(xml).ok()?;
    let instance = doc
        .descendants()
        .find(|n| n.tag_name().name() == "instance" && n.attribute("id").is_none())?;
    let root = instance.children().find(|n| n.is_element())?;
    let id = root.attribute("id")?.to_string();
    let prefix = format!("/{}/", root.tag_name().name());
    let reps = doc
        .descendants()
        .filter(|n| n.tag_name().name() == "repeat")
        .filter_map(|n| n.attribute("nodeset"))
        .filter_map(|ns| ns.trim().strip_prefix(&prefix).map(str::to_string))
        .collect();
    Some((id, reps))
}

/// Guess repeat paths from the data: an element with children that appears
/// more than once under the same parent. A repeat that only ever has one
/// entry cannot be told apart from a group this way.
pub fn guess_repeats(docs: &[roxmltree::Document]) -> HashSet<String> {
    fn visit(node: roxmltree::Node, path: &str, out: &mut HashSet<String>) {
        let mut seen: HashMap<&str, usize> = HashMap::new();
        for c in node.children().filter(|c| c.is_element()) {
            *seen.entry(c.tag_name().name()).or_default() += 1;
        }
        for c in node.children().filter(|c| c.is_element()) {
            let name = c.tag_name().name();
            let p = if path.is_empty() {
                name.to_string()
            } else {
                format!("{path}/{name}")
            };
            if c.children().any(|g| g.is_element()) {
                if seen[name] > 1 {
                    out.insert(p.clone());
                }
                visit(c, &p, out);
            }
        }
    }
    let mut out = HashSet::new();
    for d in docs {
        visit(d.root_element(), "", &mut out);
    }
    out
}

struct Ctx<'a> {
    repeats: &'a HashSet<String>,
    tables: BTreeMap<String, Table>,
}

impl Ctx<'_> {
    /// Walk the children of `node`, writing leaves into `row` of `table`.
    fn flatten(
        &mut self,
        node: roxmltree::Node,
        path: &str,
        col_prefix: &str,
        table: &str,
        row: usize,
        key: &str,
    ) {
        let mut counters: HashMap<&str, usize> = HashMap::new();
        for c in node.children().filter(|c| c.is_element()) {
            let name = c.tag_name().name();
            let p = if path.is_empty() {
                name.to_string()
            } else {
                format!("{path}/{name}")
            };
            let has_children = c.children().any(|g| g.is_element());
            if self.repeats.contains(&p) {
                let n = counters.entry(name).or_default();
                *n += 1;
                let child_key = format!("{key}/{name}[{n}]");
                let t = self.tables.entry(p.clone()).or_default();
                let r = t.new_row();
                self.flatten(c, &p, "", &p, r, &child_key);
                let t = self.tables.get_mut(&p).expect("table exists");
                t.set(r, "PARENT_KEY", key);
                t.set(r, "KEY", &child_key);
            } else if has_children {
                let prefix = format!("{col_prefix}{name}-");
                self.flatten(c, &p, &prefix, table, row, key);
            } else {
                let value = c.text().unwrap_or("").trim();
                let col = format!("{col_prefix}{name}");
                let t = self.tables.get_mut(table).expect("table exists");
                t.set(row, &col, value);
            }
        }
    }
}

/// Build the tables for the instances of one form.
/// Returns a map from table path ("" for the main table) to table.
pub fn build_tables(
    xmls: &[(String, String)],
    known_repeats: Option<&HashSet<String>>,
) -> Result<BTreeMap<String, Table>, String> {
    let docs: Vec<roxmltree::Document> = xmls
        .iter()
        .map(|(_, x)| roxmltree::Document::parse(x).map_err(|e| e.to_string()))
        .collect::<Result<_, _>>()?;
    let guessed;
    let repeats = match known_repeats {
        Some(r) => r,
        None => {
            guessed = guess_repeats(&docs);
            &guessed
        }
    };
    let mut ctx = Ctx {
        repeats,
        tables: BTreeMap::new(),
    };
    ctx.tables.insert(String::new(), Table::default());
    for ((key, _), doc) in xmls.iter().zip(&docs) {
        let row = ctx.tables.get_mut("").expect("main").new_row();
        ctx.flatten(doc.root_element(), "", "", "", row, key);
        ctx.tables.get_mut("").expect("main").set(row, "KEY", key);
    }
    // Central puts KEY (and PARENT_KEY) last; move them there.
    for t in ctx.tables.values_mut() {
        let order: Vec<usize> = (0..t.columns.len())
            .filter(|&i| t.columns[i] != "PARENT_KEY" && t.columns[i] != "KEY")
            .chain(
                ["PARENT_KEY", "KEY"]
                    .iter()
                    .filter_map(|k| t.index.get(*k).copied()),
            )
            .collect();
        let cols: Vec<String> = order.iter().map(|&i| t.columns[i].clone()).collect();
        let rows = t
            .rows
            .iter()
            .map(|r| {
                order
                    .iter()
                    .map(|&i| r.get(i).cloned().unwrap_or_default())
                    .collect()
            })
            .collect();
        t.index = cols
            .iter()
            .enumerate()
            .map(|(i, c)| (c.clone(), i))
            .collect();
        t.columns = cols;
        t.rows = rows;
    }
    Ok(ctx.tables)
}

fn safe_name(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' {
                c
            } else {
                '-'
            }
        })
        .collect()
}

/// Write CSV files for `instances` into `out_dir`. Encrypted instances are
/// skipped (they cannot be read without the private key).
/// Returns the written files with their row counts, plus skipped instances.
pub fn export(
    instances: &[Instance],
    forms_dirs: &[PathBuf],
    out_dir: &Path,
) -> Result<(Vec<(PathBuf, usize)>, usize), String> {
    let mut dirs: Vec<PathBuf> = forms_dirs.to_vec();
    for i in instances {
        if let Some(project) = i.dir.parent().and_then(Path::parent) {
            let f = project.join("forms");
            if f.is_dir() && !dirs.contains(&f) {
                dirs.push(f);
            }
        }
    }
    let defs = repeats_from_forms(&dirs);
    let mut by_form: BTreeMap<&str, Vec<(String, String)>> = BTreeMap::new();
    let mut skipped = 0;
    for i in instances {
        if i.encrypted {
            skipped += 1;
            continue;
        }
        let xml = fs::read_to_string(&i.xml_path).map_err(|e| e.to_string())?;
        let key = i
            .instance_id
            .clone()
            .unwrap_or_else(|| i.dir.display().to_string());
        by_form.entry(&i.form_id).or_default().push((key, xml));
    }
    fs::create_dir_all(out_dir).map_err(|e| e.to_string())?;
    let mut written = Vec::new();
    for (form, xmls) in by_form {
        let tables = build_tables(&xmls, defs.get(form))?;
        for (path, table) in tables {
            let name = if path.is_empty() {
                format!("{}.csv", safe_name(form))
            } else {
                format!(
                    "{}-{}.csv",
                    safe_name(form),
                    safe_name(&path.replace('/', "-"))
                )
            };
            let file = out_dir.join(name);
            fs::write(&file, table.to_csv()).map_err(|e| e.to_string())?;
            written.push((file, table.rows.len()));
        }
    }
    Ok((written, skipped))
}

#[cfg(test)]
mod tests {
    use super::*;

    const FORM: &str = r#"<h:html xmlns="http://www.w3.org/2002/xforms" xmlns:h="http://www.w3.org/1999/xhtml"><h:head><model><instance><data id="hh"><village/><household><member><name/><age/></member></household><meta><instanceID/></meta></data></instance><instance id="list"><root/></instance></model></h:head><h:body><group ref="/data/household"><repeat nodeset="/data/household/member"><input ref="/data/household/member/name"/></repeat></group></h:body></h:html>"#;

    #[test]
    fn reads_repeats_from_form() {
        let (id, reps) = parse_form_repeats(FORM).unwrap();
        assert_eq!(id, "hh");
        assert_eq!(reps, HashSet::from(["household/member".to_string()]));
    }

    fn sub(id: &str, members: &[(&str, &str)]) -> (String, String) {
        let m: String = members
            .iter()
            .map(|(n, a)| format!("<member><name>{n}</name><age>{a}</age></member>"))
            .collect();
        (
            id.to_string(),
            format!(
                "<data id=\"hh\"><village>Kisu, North</village><household>{m}</household><meta><instanceID>{id}</instanceID></meta></data>"
            ),
        )
    }

    #[test]
    fn builds_central_style_tables() {
        let xmls = vec![
            sub("uuid:1", &[("Ana", "34"), ("Ben", "5")]),
            sub("uuid:2", &[("Cy", "60")]),
        ];
        let reps = HashSet::from(["household/member".to_string()]);
        let t = build_tables(&xmls, Some(&reps)).unwrap();
        let main = &t[""];
        assert_eq!(main.columns, vec!["village", "meta-instanceID", "KEY"]);
        assert_eq!(
            main.to_csv(),
            "village,meta-instanceID,KEY\n\"Kisu, North\",uuid:1,uuid:1\n\"Kisu, North\",uuid:2,uuid:2\n"
        );
        let mem = &t["household/member"];
        assert_eq!(mem.columns, vec!["name", "age", "PARENT_KEY", "KEY"]);
        assert_eq!(mem.rows[1], vec!["Ben", "5", "uuid:1", "uuid:1/member[2]"]);
        assert_eq!(mem.rows[2], vec!["Cy", "60", "uuid:2", "uuid:2/member[1]"]);
    }

    #[test]
    fn defuses_spreadsheet_formulas() {
        let xmls = vec![sub("uuid:1", &[("=HYPERLINK(\"x\")", "+1"), ("@a", "-5")])];
        let t = build_tables(&xmls, None).unwrap();
        let csv = t["household/member"].to_csv();
        assert!(csv.contains("\"'=HYPERLINK(\"\"x\"\")\",'+1,"), "{csv}");
        assert!(csv.contains("'@a,-5,"), "{csv}");
    }

    #[test]
    fn guesses_repeats_without_form() {
        let xmls = vec![
            sub("uuid:1", &[("Ana", "34"), ("Ben", "5")]),
            sub("uuid:2", &[("Cy", "60")]),
        ];
        let t = build_tables(&xmls, None).unwrap();
        assert_eq!(t["household/member"].rows.len(), 3);
    }
}
