# The differential harness

Runs identical SQL against a real MySQL and against mysql-ultralight, and
reports where the two disagree — on rows, on column metadata, and on which
statements are legal at all.

```sh
./scripts/differential.sh           # start mysql:8.3 in Docker, run, tear down
./scripts/differential.sh --keep    # leave it running for the next run
./scripts/differential.sh --bless   # record the current divergences
```

## Why

This engine's only job is to answer the way MySQL answers, and every bug worth
the name in it has been a place where it did not. Nearly all of those were
caught by a reviewer who happened to know MySQL — which does not scale, and did
not always work: `JSON_QUOTE` was changed to return a document on the strength
of an argument that sounded right, and MySQL returns a string.

The harness asks MySQL instead. Its first run found 108 divergences across 66
statements, in eight classes, none of which anything else had noticed.

## Both protocols

Every case runs twice, through the text protocol and through the binary one,
and the baseline records which. They are not the same question. The text
protocol sends every value as a string, so a column's declared type affects
nothing about the bytes; the binary protocol — the one a prepared statement,
and therefore Prisma, uses — encodes *by* that type, so a column announced as
one byte is sent as one byte whatever it holds.

The harness spoke only the text protocol at first, and could not see any of it.
Narrowing the type codes in #0038 to match MySQL exactly turned "too wide" into
"too narrow" in five places — `SUM` over a `TINYINT` announced as one byte, an
`UNSIGNED` column read back signed, a union of the same width and different
signs — and the text-only harness reported nothing but progress.

## The baseline

`baseline.txt` holds the divergences already known about, one per line as
`group :: statement :: what`. A run is quiet unless something changes, because
a report that lists a hundred differences every time teaches its reader to stop
looking.

Two things fail a run:

- a divergence **not** in the baseline — something got worse, or the corpus
  grew and found something new
- a line in the baseline that **no longer differs** — the file is describing a
  state that has passed, and a stale claim is worse than none

So the file only ever shrinks, and it shrinks by fixing things. Each line is
covered by an issue under [#0009](../../.motte/issues), and the whole file is
accounted for — 4 lines at the last audit, across 224 statements:

| Lines | Issue |
|---|---|
| 2 | #0046 JSON doubles outside a middling range |
| 2 | #0066 SOUNDEX unsupported |

Worth re-deriving rather than trusting after a run that fixes anything: five of
those issues were written only once the file was counted against the issue list,
because the harness reports what is *new*, never what is unclaimed.

## Writing a case

Two rules the harness enforces rather than trusts:

- **No two cases in a group may be the same statement.** A baseline line is
  keyed by group, statement and what differed, so a repeat would share a key
  and the second would never be counted. Give one an alias.
- **Anything returning more than one row needs an `ORDER BY`.** Rows are
  compared by position, and a case without a total order can diverge because
  the two servers picked different plans — an intermittent failure is the one
  thing a baseline cannot absorb.

A setup statement that fails is a failed run, and refuses to be blessed: if
setup fails on *both* servers every case in the group then fails identically,
compares equal, and reports a group that tested nothing as a group that agreed.

## The corpus

`src/cases.rs`, grouped by area, each group with its own schema and a fresh
database. The first group is `answers I got wrong` — every statement in it is
one this engine once answered incorrectly, kept so that the next regression is
caught by MySQL rather than by review.

Adding a case is one line. Prefer statements that a real application would
write over ones that explore the grammar: the point is compatibility with what
Prisma and a test suite emit, not coverage of MySQL.

## What it does not do yet

The migrations, the raw-SQL call sites, and Prisma's generated queries — all
three named in #0009 — are not fed through it. The harness is the part that had
to exist first; pointing it at the corpus is the next step.
