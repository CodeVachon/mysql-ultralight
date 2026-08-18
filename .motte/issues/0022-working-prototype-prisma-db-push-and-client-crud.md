---
id: 22
title: "Working prototype: Prisma db push and Client CRUD"
state: Done
labels: [milestone]
created: 2026-08-18T18:13:30Z
updated: 2026-08-18T18:13:30Z
---

## Description

Milestone reached 2026-08-18. The engine runs a full Prisma stack: prisma db push creates the schema and Prisma Client performs create, findMany with orderBy, findUnique, filtering with gt and contains, count, update, take/skip pagination and delete. Cold start is 2.05ms median spawn to handshake, against 4.5-8.9s of container start plus 26.1s of migrate deploy for real MySQL.

Delivered as crates/engine: catalog, storage, expression evaluator, scalar function library, statement executor, session state and information_schema. 48 engine tests. See docs/prototype.md for the capability list and the two bugs worth remembering.
