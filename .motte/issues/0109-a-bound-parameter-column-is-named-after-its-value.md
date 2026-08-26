---
id: 109
title: A bound parameter column is named after its value
state: Todo
parent: 9
labels: [compat]
created: 2026-08-25T19:32:31Z
updated: 2026-08-25T21:50:56Z
---

## Description

Found while implementing #0059, which widened it, and pinned there with MySQL's answer beside each assertion.

MySQL names the column of SELECT ? by the marker, at prepare and at execute, whatever is bound:

    SELECT ?          mysql ? / ?        engine describe NULL, execute the value
    SELECT ? AS lit   mysql lit / lit    engine lit -- the one spelling that agrees

Measured through a real prepared statement, not the CLI. This engine splices a parameter into the statement text before parsing, so the literal-naming rule of #0059 now applies to the substituted value: a bound 'hello world' names the column hello world, '  x' picks up the leading-whitespace trim, and a 300-byte string is cut at 255. Prepare and execute also disagree with each other, since describe has no value to splice and answers NULL.

Naming it ? means params::bind reporting where it wrote, through rewrite::normalise, whose passes are not offset-preserving -- the same offset mapping #0060 exists for. Doing it inside #0059 would have made a naming change into a rewriter change.

Behaviour is pinned by a_bound_parameter_column_is_named_after_its_value_where_mysql_says_question_mark in crates/engine/tests/sql.rs, which cross-references the SELECT ? assertion in describe.rs.

## Notes

### 2026-08-25T21:50:56Z — Christopher Vachon (user)

The offset mapping this was waiting on landed with #0060 in 2517c12, so half the blocker is gone.

What remains is the other half: params::bind splices bound values into the text before rewrite::normalise runs, so Normalised::written is the already-substituted text rather than the text with ? in it. Naming the column ? now needs bind to report its substitutions in the same piece terms the rewriter uses, so the two compose -- a binder change rather than a naming change.

Behaviour stays pinned by a_bound_parameter_column_is_named_after_its_value_where_mysql_says_question_mark in crates/engine/tests/sql.rs, whose doc comment now says this.
