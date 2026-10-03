---
slug: odk-sneakernet
title: CLI that inventories ODK Collect submissions copied off phones and uploads them to ODK Central (or exports CSV) without the deprecated Briefcase
verdict: build
---

## Problem

NGO survey and monitoring teams collect data with ODK Collect in places with
no internet. Normally each phone uploads its finished forms when it gets a
connection. When that can't happen (no coverage for weeks, a broken phone, a
supervisor who has to carry the data out), someone has to copy the
submission folders off the phones over USB and get them into the server
later.

Demand, with sources:

- ODK Forum, "Export of offline submissions from Collect" (June 2025),
  https://forum.getodk.org/t/export-of-offline-submissions-from-collect/55445
  In the DRC, teams collect millions of submissions offline. Supervisors
  collect 20+ phones and travel 2–3 days to reach internet, and data
  collection stops while the phones are away. The wish is to dump the
  submissions so one device can carry them and someone can upload them later.
  A reply notes that ODK Briefcase used to do exactly this and is now
  deprecated.
- ODK Forum, "Bulk uploading to Central Server" (Aug 2025),
  https://forum.getodk.org/t/bulk-uploading-to-central-server/56169
  ODK's co-founder writes: "There isn't yet a dedicated bulk upload feature,
  so you'll need to create one submission … at a time" through the API.
- ODK Forum, "Manually upload submissions to Central",
  https://forum.getodk.org/t/manually-upload-submissions-to-central/33905
  (tablets that fail to send), and "How to push completed ODK submission on
  ODK Central Server?",
  https://forum.getodk.org/t/how-to-push-completed-odk-submission-on-odk-central-server/35388
  ("ODK Briefcase is not working anymore").
- ODK Forum, "Decryption with Briefcase" (Feb 2025),
  https://forum.getodk.org/t/decryption-with-briefcase/53386: Briefcase
  breaks on current Java/Windows 11. The fix was to install an old Zulu Java 8.

## Who benefits

M&E officers, survey supervisors and data managers at NGOs, research groups
and health programs that run ODK Central, whether self-hosted or ODK Cloud.
Also anyone recovering data from a broken tablet. These people already
follow the ODK docs to copy `instances` folders off phones over USB. They
would find the tool through the ODK forum, where these questions come up,
and run it as one downloaded binary on the Windows, macOS or Linux laptop
they use for field work. No runtime install is needed.

## Existing solutions

- **ODK Briefcase** (https://github.com/getodk/briefcase): the documented way
  to pull from a Collect folder and push to Central. It was deprecated in
  Dec 2021 and last pushed in 2022. It needs Java 8 (Ona's guide says "not
  later than java-8", https://help.ona.io/knowledge-base/how-do-i-use-odk-briefcase/).
  Forum threads show it failing to launch or decrypt on current systems.
- **centralpy** (https://github.com/pmaengineering/centralpy): a Python CLI
  whose `push` crawls a folder of Collect instances. 3 stars, last push
  Oct 2021. It needs a Python install, logs in with a web-user password, and
  doesn't know whether an instance is a draft or finalized.
- **pyodk** (https://github.com/getodk/pyodk, active) and **ruODK**:
  libraries. You have to write your own script that walks folders, builds
  multipart OpenRosa requests and handles attachments. That takes a
  programmer, which these teams usually don't have in the field.
- **KoboToolbox bulk-submission-form**
  (https://support.kobotoolbox.org/manual_upload.html): a zip upload page.
  It solves the problem for Kobo's servers only. Central has no equivalent.
- **Self-hosting Central on a laptop or Raspberry Pi in the field**:
  suggested in the DRC thread and rejected there as impractical (40+
  projects, unreliable power).
- **Re-importing folders into another phone's Collect and sending from
  there**: works for a handful of phones. It needs a matching Collect
  project on the receiving device, and it is how duplicates happen.
- crates.io: `rxform` and `rxeval` handle forms, not submission transport.

## Why build anything

Central, the server ODK now recommends, has no maintained, install-free way
to upload a folder of submissions copied from phones. The one tool built for
this (Briefcase) is deprecated and tied to Java 8. The Python alternative is
abandoned and can't tell drafts from finished forms. Kobo users have a zip
upload. Central users have nothing. Briefcase also gave teams without any
server a CSV export of those folders, and nothing replaces that either.

## Smallest useful intervention

`odk-sneakernet`, a single binary with three commands that take one or more
copied Collect folders (several phones at once):

- `scan`: an inventory per form. It counts finalized, draft, already-sent
  and unknown submissions, finds duplicate instanceIDs across phones, and
  flags missing attachments. It reads Collect's `instances.db` when that was
  copied too, which is the only place the draft/finalized status lives.
- `push`: uploads finalized submissions with their attachments to a Central
  (or any OpenRosa) server, using the same server URL Collect uses (an App
  User URL with no password, or Basic auth). It respects the server's size
  limit by splitting attachments across requests, as Collect does. It keeps
  a local log so an interrupted push can be resumed, and reports conflicts.
- `csv`: a flat CSV per form, plus one per repeat group, written offline.
  This is for teams with no server at all.

## Success criterion

- Against a local mock OpenRosa server in the tests, `push` sends every
  finalized instance exactly once, with all its attachments. It splits
  oversized submissions, skips drafts and already-logged instances, and
  reports a 409 as a conflict.
- `scan` on fixtures in both the modern (`projects/<uuid>/instances` with
  `metadata/instances.db`) and legacy (`odk/instances`) layouts reports the
  right status counts, duplicates and missing attachments.
- A supervisor can go from a USB copy of several phones to all submissions
  in Central with two commands and no runtime install.

## Maintenance

Small. The OpenRosa submission protocol has been stable for over ten years,
and Central documents it as fully compliant. The likely breakages are a
change to Collect's storage layout or `instances.db` schema (it has been
stable since the 2021 move to projects) and dependency updates (ureq,
rusqlite, roxmltree). There is no server to run and no API key.

## Decision

Build. People in the ODK community keep asking for this, ODK staff confirm
Central has no bulk upload, and the only purpose-built tool is deprecated and
tied to Java 8. A narrow, install-free CLI covers exactly the Central gap. I
leave out Kobo's servers in the docs' main path, because Kobo already has a
zip upload page.
