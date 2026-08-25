---
id: 59
title: A string literal column is named with its quotes
state: Done
parent: 9
labels: [compat]
created: 2026-08-21T02:24:55Z
updated: 2026-08-25T19:32:16Z
---

## Description

SELECT 'abc' is named `abc` by MySQL and `'abc'` here. MySQL names a bare string-literal column by its *contents*, where every other unaliased expression is named by its source text — SELECT 1!=2 really is named 1!=2.

Pre-existing, and #0043 did not change it: the source text and the rendered tree agree here, and both keep the quotes.

Adjacent literal concatenation is worth measuring before fixing: MySQL names SELECT 'a' 'b' just `a`, not `ab` and not `'a' 'b'`.

## Notes

### 2026-08-21T13:36:26Z — Christopher Vachon (user)

Related, same area: MySQL names  after the operand, , where this engine uses the written text  — unary plus is a no-op it does not record. Measured on 8.3 while fixing #0063.

### 2026-08-21T13:37:06Z — Christopher Vachon (user)

Related, same area. MySQL names SELECT +s after the operand -- "s" -- where this engine uses the text as written, "+s". Unary plus is a no-op MySQL does not record in the name. Measured on 8.3 while fixing #0063.

### 2026-08-25T19:32:16Z — Christopher Vachon (user)

Done in f20c427.

The rule that reproduces every measurement: peel parentheses and unary plus, however many; if what remains is a string literal, a number literal or NULL, the item carries a parse-time name, otherwise the whole unpeeled item is named by its source text.

That explains the oddities. SELECT +1 is named 1 and SELECT +TRUE keeps its plus, because TRUE has no parse-time name. Unary minus is never peeled, so SELECT -1 is -1 while SELECT - 1 is '- 1' -- the text, not the literal. NULL is the only literal MySQL canonicalises, so SELECT null is named NULL. A string is named by its decoded contents, the first of an adjacent run only, leading non-graphic ASCII trimmed and trailing kept, cut at 255 bytes on a character boundary.

Both notes on this issue confirmed: 'a' 'b' is named a, and +s is named s.

Review found the change reached two places it should not have. A view stores its definition by rendering the AST and sqlparser joins an adjacent run into one literal, so SELECT 'a' 'b' came back named ab. Views now pin each unaliased name as an alias, which is what MySQL does -- 8.3 stores that view as select 'ab' AS `a` -- and pinning is what carries the name through the read path, refresh_views and the state image, all of which re-derive from that text. The guard has to compare names rather than text: SELECT 'a``b' renders to text that round-trips perfectly and reads back as one backtick, so it is refused and falls back. A wildcard at the top pinned nothing, so CTEs, derived tables and joins are walked too.

Interaction with #0060: better, not worse. A literal's name no longer comes from the text at all, so a rewritten statement names it correctly regardless. The one text read left slices the value's own span out of the same normalised string the spans point into, and falls back to the AST value otherwise.

Not fixed and pinned with MySQL's answer beside it: a bound parameter is spliced into the text before parsing, so SELECT ? is named after the value where MySQL says ? at both prepare and execute. Naming it ? needs params::bind to report where it wrote, through rewrite::normalise, whose passes are not offset-preserving -- the mapping #0060 exists for. Filed as #0109.

25 mutations. Two pass and are reported as such: one is inert by construction because the guard's name collector runs on both sides of its own comparison, and three call sites are subsumed because both paths reach the name through one. Corpus 1160 -> 1216, baseline empty.
