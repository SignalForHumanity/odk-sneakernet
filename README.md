# odk-sneakernet

Get ODK Collect submissions that were copied off phones over USB into
**ODK Central**, or into CSV files, without the deprecated ODK Briefcase.
It is a single binary and needs no Java or Python install.

## The problem

Survey teams working with [ODK Collect](https://getodk.org) in places with no
coverage sometimes have to carry the data out by hand. A supervisor
collects the phones and drives for days to reach internet. A tablet breaks
before it can send. A project runs with no server at all. The ODK docs say
to copy each phone's `instances` folder to a laptop. But ODK Central has no
way to upload such folders ("There isn't yet a dedicated bulk upload
feature", ODK forum, 2025). The tool that used to do it, ODK Briefcase, was
deprecated in 2021 and needs Java 8.

`odk-sneakernet` covers that gap:

- **`scan`** gives a per-form inventory across all the phone copies you give
  it. It counts finalized, draft, already-sent and unknown submissions,
  counts identical copies, lists conflicting versions of the same instanceID,
  and reports media files the submission refers to that weren't copied.
- **`push`** uploads finalized submissions and their attachments to Central
  (or any OpenRosa server) over the same protocol Collect uses. It splits
  large submissions to fit the server's size limit, keeps a log so an
  interrupted upload can resume, and never logs App User tokens.
- **`csv`** writes one CSV per form plus one per repeat group, with
  Central-style column names and `PARENT_KEY`/`KEY` links. It needs no
  server.

Background, existing tools and why this exists: [PROBLEM.md](PROBLEM.md).

## Who it's for

Survey supervisors, M&E officers and data managers at NGOs, research groups
and health programs that use ODK Collect with ODK Central (self-hosted or
ODK Cloud). Also anyone recovering data from a broken device.

## Install

There are no prebuilt release binaries yet. Build it with
[Rust](https://rustup.rs) (this produces one self-contained binary you can
copy to other machines of the same OS):

```
cargo install --git https://github.com/SignalForHumanity/odk-sneakernet
```

## 1. Copy the data off each phone

Connect the phone over USB (on macOS use Android File Transfer) and open:

```
Android/data/org.odk.collect.android/files/projects/<long-id>/
```

For KoboCollect the app folder is `org.koboc.collect.android`. Copy the
**whole `<long-id>` folder**, or at least both `instances` and `metadata`.
Collect records whether a form is a draft or finalized only in
`metadata/instances.db`. Without that file the tool can't tell drafts
apart. Put each phone in its own folder, e.g. `fieldwork/phone01`,
`fieldwork/phone02`. Old Collect versions keep everything under `/sdcard/odk`;
copy that folder.

## 2. Check what you have

```
odk-sneakernet scan fieldwork/
```

```
form                         finalized  draft   sent  unknown encrypted  missing files  versions
hh_survey                          412     9     37        0         0              2  2024031501

458 submission folders, 455 unique submissions
3 identical copies of the same submission (sent once)
MISSING FILES in fieldwork/phone07/.../hh_survey_2024-05-01_10-00-00: 1714550400.jpg
```

## 3. Upload to Central

Use the server URL the phones are configured with. In Central, open
*Project → App Users*, create an App User with access to the form, and copy
its URL from the configuration QR code. It looks like
`https://central.example.org/v1/key/<token>/projects/3`. No password is
needed with this URL, but the token in it is a secret: put it in the
`ODK_SERVER` environment variable rather than on the command line, where
`ps` and your shell history would show it. (`--server URL` also works and
takes precedence.)

```
read -rs ODK_SERVER && export ODK_SERVER   # paste the URL, then Enter
odk-sneakernet push fieldwork/
```

Or log in as a web user with Basic auth (only over `https://`; plain
`http://` is refused except for a server on this machine):

```
read -rs ODK_PASSWORD && export ODK_PASSWORD
ODK_SERVER=https://central.example.org/v1/projects/3 \
  odk-sneakernet push --user me@example.org fieldwork/
```

Without `ODK_PASSWORD` the tool asks for the password, and what you type
is visible on screen.

- Finalized submissions are sent. Drafts and submissions Collect already
  sent are skipped unless you pass `--include-drafts` or `--include-sent`.
  Submissions with unknown status (no `instances.db`) are sent with a
  warning; `--skip-unknown` leaves them out.
- Results are appended to `odk-sneakernet-log.csv` (change with `--log`).
  If the connection drops, run the same command again and it continues
  where it stopped.
- `--dry-run` lists what would be sent.
- A submission whose media files were not copied is still sent (Central
  shows the files as missing), with a warning. It is logged as `partial`,
  not `created`, so running the command again after copying the files sends
  it again and Central adds the files to the submission it already has.
  Until then the exit code is 1.
- A submission whose files can't be read is logged as `unreadable` and the
  upload continues with the next one.
- Central answers `409 Conflict` when it already has a *different*
  submission with the same instanceID. Those are reported and logged, not
  overwritten.
- If the copies you give hold different versions of the same instanceID,
  none is sent and they are listed as conflicts. Exception: a draft copy
  next to a finalized or sent copy (the same phone copied before and after
  finalizing) is treated as an older copy, and the non-draft one is used.
- A submission the server refuses with `403` (for example a form the App
  User has no access to) is logged as rejected and the upload continues with
  the next one.
- Encrypted forms are uploaded as they are: the manifest, `submission.xml.enc`
  and the `.enc` media files the manifest lists. Any other file in the folder
  (for example plaintext media Collect failed to delete) is never sent.
  Central decrypts them as usual.

## 4. Or export CSV without a server

```
odk-sneakernet csv --out csv/ fieldwork/
```

This writes `csv/<form>.csv` and `csv/<form>-<group>-<repeat>.csv`. Repeat
groups are taken from the blank form XML in the copied `forms` folder, or
from `--forms DIR`. Without a form definition, a group counts as a repeat
when it occurs more than once in some submission. Drafts are left out unless
you pass `--include-drafts`. Encrypted submissions are skipped. Values
starting with `=`, `+`, `@`, a tab or a carriage return get a leading `'`
so spreadsheets don't run them as formulas.

## Exit codes

- `0`: everything went fine.
- `1`: something needs attention: conflicts, rejected or unreadable
  submissions, or an upload interrupted by a network error.
- `2`: usage error, bad server URL, or bad credentials.

## Data sources and standards

- [OpenRosa Form Submission API](https://docs.getodk.org/openrosa-form-submission/),
  as implemented by ODK Central (`POST <server>/submission`, `X-OpenRosa-Version: 1.0`,
  `X-OpenRosa-Accept-Content-Length`, `*isIncomplete*` for split uploads).
- The ODK Collect storage layout and its `instances.db` schema
  (`instances` table, `instanceFilePath` and `status` columns).
- [ODK XForms](https://getodk.github.io/xforms-spec/) submission format
  (`meta/instanceID`, encrypted submission manifests).

The tool collects nothing. It reads files you point it at and talks only to
the server you name.

## Limitations

- Status comes only from `instances.db`. If it wasn't copied, drafts can't
  be told apart from finalized forms.
- "Missing files" is a heuristic. Answers that look like media file names
  (`*.jpg`, `*.m4a`, `audit.csv`, …) are checked against the folder.
- Basic auth is sent up front, which is what Central expects. Servers that
  only accept Digest auth are not supported; use an App User URL with
  Central. KoboToolbox users can also use Kobo's own
  [bulk submission page](https://support.kobotoolbox.org/manual_upload.html).
- Submission edits (`deprecatedID`) are uploaded like any other submission.
  The tool doesn't reorder edit chains.
- The CSV export has no select-multiple splitting, no labels, and no
  decryption.
- It doesn't move data between phones. Copy folders with your computer's
  file manager or `adb pull`.

## Development

```
cargo test        # unit tests plus end-to-end tests against a local mock server
cargo clippy --all-targets -- -D warnings
```

## License

MIT OR Apache-2.0, at your option. See [LICENSE-MIT](LICENSE-MIT) and
[LICENSE-APACHE](LICENSE-APACHE).
