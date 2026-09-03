---
id: 192
title: WITH RECURSIVE is refused, and the app uses it
state: Todo
parent: 9
labels: [compat, app-priority-1]
created: 2026-09-03T14:17:42Z
updated: 2026-09-03T14:20:34Z
---

## Description

Found 2026-09-03 while checking Epic 9 against the real conformance surface of application-mono-repo. Not previously tracked by any open issue.

    WITH RECURSIVE FolderTree AS (...) SELECT ... FROM FolderTree
    mysql   answers
    engine  1235 mysql-ultralight does not yet support WITH RECURSIVE

crates/engine/src/exec.rs materialise_ctes refuses `with.recursive` outright before evaluating anything.

**This is not hypothetical.** apps/api/src/models/Folder.ts:393 runs a recursive CTE to walk the folder tree. Any test touching that path cannot run against this engine at all -- it is a hard refusal, not a wrong answer, so it fails loudly rather than silently, but it fails.

Priority note: the conformance scan counts 19 common table expressions in the target codebase and 1 recursive one. The non-recursive CTEs already work; this is the single recursive case.

Scope: a recursive CTE evaluates its anchor branch, then repeats the recursive branch against the rows added last until none are, subject to cte_max_recursion_depth (1000 by default, error 3636 when exceeded). Measure the depth-limit error before implementing, and note UNION vs UNION ALL differ -- UNION deduplicates between iterations.
