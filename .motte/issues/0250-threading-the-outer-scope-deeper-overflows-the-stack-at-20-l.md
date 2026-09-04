---
id: 250
title: Threading the outer scope deeper overflows the stack at 20 levels
state: Todo
parent: 9
labels: [executor, app-priority-2]
created: 2026-09-04T21:13:26Z
updated: 2026-09-04T21:15:44Z
---

## Description

BLOCKER on #0239/#0243/#0245. The work for those three is COMPLETE and preserved on branch worktree-agent-a047704b24d72035d (commit ac5e241, rebased onto c8d250e), but it is NOT merged because it regresses stack robustness.

    cargo test -p mysql-ultralight-engine --test sql
    thread 'a_deep_nest_of_scalar_subqueries_still_answers' has overflowed its stack
    fatal runtime error: stack overflow, aborting (SIGABRT)

That test nests 20 scalar subqueries and asserts the engine ANSWERS rather than dying. Every other test in the workspace passes on the rebased branch, and the differential was green in the agent's own worktree before the rebase.

Cause: carrying the enclosing query's schema, values and column declarations down through relation_of / subquery_column / lateral_join costs stack per nesting level, and 20 levels now exceeds the 2 MB test thread. The change clones refs, row and relation.columns into a per-level structure; whether the cost is the locals or something held across the recursive call needs diagnosis rather than a guess.

WHY IT WAS NOT LANDED: a process abort is the worst failure mode this engine has -- it takes the connection and every other one with it, which is the whole subject of #0218 and #0247. Landing a change that makes 20-level nesting abort, to fix three resolution bugs, trades a wrong answer for a crash. That is the wrong direction.

Note the interaction was predicted before the work started and the agent was warned: #0223's depth cap (MAX_EXPRESSION_DEPTH = 64) was measured against a ~90-term debug ceiling, and carrying more scope per level lowers it. The agent hit a weekly usage limit at its self-review step and never reported, so the diagnosis was never done.

To resume: measure the per-level stack cost (the earlier prepare-time work found a Cow<Query> costing a whole Query per level and fixed it by boxing -- the same shape may apply), or box the per-level scope structure, then re-run the deep-nest test before merging.

## Notes

### 2026-09-04T21:15:44Z — Christopher Vachon (user)

DIAGNOSIS NARROWED (coordinator, after the agent hit its limit). The overflow is purely stack, and the shortfall is measured:

    RUST_MIN_STACK=2MB (the default)  FAIL -- SIGABRT
    RUST_MIN_STACK=3MB                PASS
    RUST_MIN_STACK=8MB                PASS

So it needs between 2 and 3 MB where it previously fit in 2 -- roughly a 50% increase in the deep-nest path, not a marginal overrun.

THIS IS A REAL CLIENT-FACING REGRESSION, not a test artifact: #0223's agent measured that the server sets no stack size, so a connection runs on tokio's default 2 MB worker. The test thread and a real connection have the same budget.

A cheap fix I considered and rejected: Session::lateral is Option<OuterRow> inline, and OuterRow gained a third Vec (columns), so boxing it would save ~48 bytes per scoped Session clone -- about 1 KB over 20 levels against a ~1 MB shortfall. Not the cause; do not start there.

Where to look instead: OuterRow::behind ACCUMULATES the enclosing scope, so at level N the schema/values/columns hold all N levels' columns. That makes the structure grow with depth rather than staying constant, which fits a ~50% increase far better than a fixed per-level cost. Sharing the enclosing scope by reference or Rc instead of rebuilding a combined OuterRow per level is the first thing to measure.

Do NOT lower the deep-nest test's depth or raise its stack to make this pass -- both hide a regression a real client would hit.
