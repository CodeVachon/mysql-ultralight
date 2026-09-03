---
id: 132
title: LAST_INSERT_ID after an INSERT reports the last id, not the first
state: Done
parent: 9
labels: [compat]
created: 2026-08-27T22:46:01Z
updated: 2026-09-03T03:55:00Z
---

## Description

Found while implementing #0125, which made CTAS follow MySQL's rule and so left the two paths disagreeing with each other.

MySQL reports the FIRST id a statement generated, not the last:

    INSERT ... two auto-increment rows, then LAST_INSERT_ID()
    mysql 1   engine 2

219ecad fixed this for CREATE TABLE ... AS SELECT and deliberately did not touch INSERT, so the engine is now right on one path and wrong on the other. That is worth closing quickly for consistency alone.

Measured alongside it, and separate: after a two-row INSERT ... SELECT the next generated id is 4 on MySQL and 3 here, because InnoDB pre-allocates for a statement whose row count it does not know in advance. A multi-row VALUES insert agrees at 3. CTAS inherits the gap because it now uses the INSERT path, which is the correct outcome rather than a new bug. Worth deciding whether to reproduce that pre-allocation at all -- it is an InnoDB implementation detail, and a test that depends on it is testing the storage engine rather than the SQL.

## Notes

### 2026-09-03T03:55:00Z — Christopher Vachon (user)

Done in 92927c0, in an isolated worktree. Closes the disagreement #0125 left: CTAS reported the first id and INSERT the last.

Measured on 8.3: a 3-row VALUES insert reports the first id; so does a 2-row INSERT ... SELECT and an INSERT IGNORE (the first STORED id). A statement supplying every id leaves the value alone, a table with no auto-increment column leaves it alone, and a zero-row insert leaves it alone and does not move the counter. A statement mixing explicit and generated ids reports the first GENERATED one. It is per connection; a second connection sees 0. After a CTAS then an INSERT, the INSERT wins.

Review found the first cut made a prepare-time refusal consume a counter value. 8.3 distinguishes them: a statement refused for 1136 or 3105 stores nothing and leaves LAST_INSERT_ID untouched, while a row refused during execution -- 1366, 1048, 1062 -- does report the ids already generated. Both checks are now settled above the row loop, and the id is written after the commit on success and before the error return on failure.

Also fixed: the 1136 message numbered rows it had inserted rather than rows it had read, so an IGNORE that skipped one made every later row number wrong. And the 3105 test was passing for the wrong reason -- the named-column form is settled earlier, where targets are resolved, so the test was rewritten to reach the hoisted check through the implicit-column-list form.

InnoDB's pre-allocation deliberately not reproduced: it reserves a batch for a statement whose row count it cannot know and discards the remainder, so the next id after a two-row INSERT ... SELECT is 4 on 8.3 and 3 here. That is the allocator rather than SQL -- a test pinning it would pin innodb_autoinc_lock_mode, and gappy ids buy a fixture nothing. The corpus group is built so no case reads a generated id out of a table an unknown-count statement touched, and the group's setup ends by writing a value of its own so it cannot inherit another group's LAST_INSERT_ID.

9 mutations. One limit stated plainly: no single-line substitution expresses 'move the recording back after the loop', so eager-vs-deferred placement is covered by the error-path mutation plus the session-write one rather than by a single mutation.

Corpus 2736 -> 2758, baseline empty. The merge needed a real resolution -- this group and #0116's SIGN group were added at the same insertion point, so both were kept and the result recompiled and re-run rather than taking either side.

Filed: #0160 (REPLACE INTO treated as a plain INSERT), #0161 (a skipped row still consumes an id, which changes stored data), #0162 (LAST_INSERT_ID(expr) ignores its argument).
