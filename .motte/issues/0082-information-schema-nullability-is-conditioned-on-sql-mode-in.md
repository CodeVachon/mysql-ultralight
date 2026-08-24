---
id: 82
title: information_schema nullability is conditioned on sql_mode in MySQL
state: Todo
parent: 9
labels: [compat]
created: 2026-08-24T18:02:26Z
updated: 2026-08-24T18:02:26Z
---

## Description

Found while building the ANSI differential group for #0053.

MySQL reports the nullability of information_schema's own columns differently depending on the session's sql_mode. Testing one flag at a time, STRICT_TRANS_TABLES is the discriminator: with it, COLUMN_NAME and DATA_TYPE are reported nullable; without it -- and ANSI does not set it -- they are reported NOT NULL. This engine reports them nullable under every mode.

The same shape as #0050, where information_schema describes its own columns differently from a plain scan, with the new wrinkle that the answer is not fixed for a given server -- it depends on how the session is configured.

Kept out of the differential corpus: it surfaced inside the new ANSI group, and including it would have put another issue's gap in that group and cost the empty baseline.
