---
id: 192
title: WITH RECURSIVE is refused, and the app uses it
state: Done
parent: 9
labels: [compat, app-priority-1]
created: 2026-09-03T14:17:42Z
updated: 2026-09-03T16:06:10Z
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

## Notes

### 2026-09-03T16:06:09Z — Christopher Vachon (user)

Landed in 025f7bf, gate green on main. Differential 3832 statements, 0 divergences.

Measured across 26 probe files rather than assumed. The rules that matter: the anchor is the branch not naming the CTE and must come first (3574 otherwise); UNION de-duplicates against everything produced so far, not just the last round, which is why it terminates over a cycle where UNION ALL runs away to 3636; cte_max_recursion_depth is 1000 and 3636 reports depth+1; the CTE's own LIMIT stops recursion before the depth limit fires while the outer query's does not; a recursive CTE's columns are all nullable and typed from the anchor alone; an unread recursive CTE is not run AND not resolved, transitively.

Review caught five real bugs, the first being the reason the issue exists: matching a CTE name case-insensitively made WITH RECURSIVE folder AS (... FROM Folder ...) read its own name in the table it walks -- the exact PascalCase shape a Prisma model produces. Fixing it also closed the pre-existing WITH q ... FROM Q divergence.

Also fixed a connection-dropping crash: Relation::origins fell out of step with rows once a round added more than the anchor had.

23 mutations. One did not bite on first run and was replaced -- the coercion mutation's 1690 came from the expression evaluator, not from the write it was meant to prove. Divergences found filed as #0194-#0197.
