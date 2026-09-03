# The differential harness

Runs identical SQL against a real MySQL and against mysql-ultralight, and
reports where the two disagree — on rows, on column metadata, and on which
statements are legal at all.

```sh
./scripts/differential.sh           # start mysql:8.3 in Docker, run, tear down
./scripts/differential.sh --keep    # leave MySQL running for the next run
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

## The engine it compares against

A green run is a claim about the build in the working tree, so the harness
refuses to make it about anything else. The engine prints a fresh instance id
on its `ready` line and serves the same value back as `@@ultralight_instance`;
`--engine-instance` is required, and a peer that reports a different id — or
cannot report one at all — fails the run instead of being compared.

That is not hypothetical. A run once reported zero divergences because a
mysql-ultralight left over from an earlier debugging step still held port
33307: the engine the script started could not bind, exited, and the readiness
poll was answered by the stale one, so MySQL was compared against a previous
build. It surfaced only because `scripts/mutate.sh` reported a test passing
with the mutation in place — a false green in the one place the project uses to
prove that a test bites.

`--keep` reuses MySQL and nothing else. MySQL costs seconds to start, which is
worth keeping; the engine costs milliseconds, so reusing one saves nothing and
gives up the only thing the harness guarantees.

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

So the file only ever shrinks, and it shrinks by fixing things.

**It is empty.** All 3482 statements answer exactly as MySQL 8.3 answers them,
in both protocols — the rows, and every part of the column definition listed
under "What a column is compared on" below. It started at 108 divergences
across 66 statements.

That is a statement about the corpus, not about the engine: it means every
question this file knows how to ask now gets MySQL's answer. The way to find
the next divergence is to add a case, which is why the file below is worth
more than the count above.

While the file has lines in it, each is covered by an issue under
[#0009](../../.motte/issues), and the count is worth re-deriving rather than
trusting after a run that fixes anything — five of those issues were written
only once the file was counted against the issue list, because the harness
reports what is *new* and never what is unclaimed.

## What a column is compared on

A `ColumnDefinition41` carries eleven things about a column. For its first year
this harness read four of them — the name, the type code, and two bits of the
flags word — and reported everything else as agreement. The engine sent 4096 as
the width of every string column and 20 as the width of every number for that
whole time, green, and #0126 was found by a reviewer rather than by this.

Widening it was measured before it was fixed: against the engine as it stood,
comparing the rest of the definition produced **11382 divergences over the 1684
statements the corpus held then**, in five classes. That number is the point of
the exercise, and it is recorded here rather than smoothed away.

| Field | Compared | |
|---|---|---|
| `name` | yes | |
| `column_type` | yes | |
| `flags` | most bits, on a column from a table | see below |
| `column_length` | on a column from a table | see below |
| `decimals` | **no** | 1148 divergences over 260 statements |
| `character_set` | as bytes-or-characters, on every column | the id itself: 458 over 118 |
| `schema`, `table`, `org_table`, `org_name` | **no** | 2924 over 583 |
| `catalog` | no | the constant `def` on both servers |

The three left out are left out because each is a gap of its own and not this
one, and each needs an issue rather than a silence:

- **`decimals`** is 0 for every column this engine describes. MySQL reports a
  DECIMAL's places, a temporal's fractional digits, and 31 — `NOT_FIXED_DEC` —
  for a string and for a FLOAT with no declared scale. #0111 has the numbers.
- **`character_set`** is 255 here and 45 there, because MySQL converts a result
  into the connection's character set and reports the collation it converted
  to, and this engine reports the column's own. The id says as much about the
  client as about the column, so it is not compared — but *whether it is the
  binary one* is, on every column, computed ones included. See below.
- **the origin fields** are wrong in the other direction: this engine names a
  table and an original column for expressions that have neither, so
  `SELECT COUNT(*)` claims an `org_name` of `COUNT(*)` where MySQL leaves it
  empty.

### Bytes or characters, on every column

The one part of a column definition asked of a computed column. A column of
characters described as bytes reaches the caller as a Buffer it cannot convert
to a string — `as_text` records what that costs Prisma — and the two servers
have to agree on which it is even where the exact charset id cannot be
compared.

It is here because of the hole it fills. `GROUP_CONCAT` went out of this engine
with the binary character set for exactly one review cycle, and no other line in
`compare.rs` could have seen it: a computed column is out of scope for the width
and for the flags, and the charset id differs on every text column for a reason
that is not the engine's. This comparison is the one that would have failed.

`UNHEX` and `FROM_BASE64` are the first computed columns this engine describes
as bytes rather than as characters, and the comparison is what holds them to
it: nothing else in a `SELECT UNHEX('4142')` distinguishes the right answer
from a `VAR_STRING` of text that happens to print the same two characters.

Eleven cases decline it, each with its reason written above it, and both
directions are MySQL following a collation this engine does not model: a string
function over a NULL literal or over an explicitly `BINARY` value is binary
there (`UPPER(NULL)`, `SOUNDEX(BINARY 'x')`), and a temporal that has been
through a temporary table comes back in the connection's character set where the
same column read from its table is binary (`MIN(tm)`, a `DATETIME(3)` through a
view). They are `Case::value_kind`.

### Only a column that came from a table

`column_length` and the rest of the flags word are asked only where MySQL
reports a non-empty `org_table` — a column read out of a table or a view, whose
shape is a *declaration*. A computed column's shape is `Item::max_length` and
the collation arithmetic around it, a width per function, per operator and per
literal, and this engine models none of it: asking would report the same
missing feature on 560 statements and drown every other signal in the file.

The scope is read off the reference's answer alone, so an engine that wrongly
claimed a table for a computed column could not exempt itself from the
comparison by doing so.

A handful of cases opt out of the width alone, each with its reason written
above it, because a view's column *is* an expression and MySQL declares it from
that expression's width — `varchar(2)` for a view over `'ab'`, where this
engine declares `varchar(512)`. They are `Case::widths`, and like the baseline
that list only ever gets shorter.

### The bits of the flags word

Compared: NOT_NULL, UNSIGNED, BINARY, BLOB, ZEROFILL, TIMESTAMP, ENUM, SET.

ENUM and SET joined that list for #0087 and are the worked example of the order
this file argues for. They were excluded and *counted* while the engine set
neither, and taking the two names out of `UNMODELLED` in `compare.rs` before
touching the engine is what turned the count into a test: **50 divergences over
24 statements**, measured against the engine as it stood. The engine then had to
empty the list rather than the baseline absorb it.

Not compared, and each for its own reason:

- **PRI_KEY, UNIQUE_KEY, MULTIPLE_KEY, PART_KEY, AUTO_INCREMENT,
  NO_DEFAULT_VALUE, ON_UPDATE_NOW** — the bits that describe a column's role in
  the schema rather than what it holds. This engine sends none of them: a
  primary key arrives with no PRI_KEY. 224 divergences over 81 statements, and
  its own gap — closing it means carrying the catalog out to the wire, where
  everything else here needs only the type.
- **NUM** — not a fact about the column at all. MySQL sets it on a field a
  temporary table produced, so the same column carries it under `GROUP BY` and
  not without one; comparing it would compare the two servers' query plans.
  Measured rather than assumed: on 8.3 a nullable `ENUM` column is `ENUM` from
  `SELECT e FROM E` and `ENUM|NUM` from `SELECT e FROM E GROUP BY e`, and the
  `NOT NULL` column beside it is `ENUM` under both.

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

A third rule, which the harness cannot enforce: **two groups that set the same
session variable must not share a statement's text.** The client caches a
prepared statement by its text for the life of the connection, and MySQL reads
`div_precision_increment` when a statement is *prepared*, fixing the quotient's
and the average's scale then — a later `SET` does not re-fix an already-prepared
statement. So the second group's binary pass asks MySQL a question about the
first group's setting, and the divergence it reports is the corpus's, not the
engine's. Measured on 8.3: prepared at 4 and re-executed after
`SET SESSION div_precision_increment = 0`,
`SELECT @@div_precision_increment, 1/3, AVG(i) FROM Quot` answers 0, 0.3333,
1.6667 through the binary protocol against 0, 0, 1 through the text one. It bites
only for a statement over literals; one that names a table is re-prepared when
`reset` replaces the database under it. Give each group's statements an alias of
their own.

A group may change `sql_mode` in its setup and leave it changed for its cases —
`REAL_AS_FLOAT` does. The two connections outlive every group, so the mode is
restored to the server's default before each one rather than trusted to be it;
add to the mode in force rather than replacing it, or the group silently runs
without `STRICT_TRANS_TABLES` as well.

`default_week_format` is the second variable a group may leave changed, and is
restored the same way. `EXTRACT(WEEK FROM x)` takes no mode argument and reads
that variable instead, so the only way to ask MySQL the other seven week
questions is to set it — and a group that ran after one which did would
otherwise be counting weeks under somebody else's mode.

`div_precision_increment` is the third, restored to 4. It scales every quotient
and every average, so a group left running under somebody else's increment
would not be asking about arithmetic at all.

`group_concat_max_len` is the fourth, restored to 1024. `GROUP_CONCAT` truncates
at it, so a group that ran after one which lowered it would report every long
concatenation as a divergence about somebody else's limit. Any further variable
a group sets needs the same line in `reset`, for the same reason.

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
