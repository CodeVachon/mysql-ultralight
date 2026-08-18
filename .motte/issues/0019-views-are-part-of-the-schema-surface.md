---
id: 19
title: Views are part of the schema surface
state: Done
parent: 4
labels: [compat, blocker]
created: 2026-08-18T15:29:25Z
updated: 2026-08-18T18:52:11Z
---

## Description

Correction to the earlier conformance read. Views were reported absent because only the Prisma migrations directory was searched. They are not created by Prisma.

provisions/mysql/3-anzen-views.sql is a generated file containing 8 CREATE OR REPLACE VIEW statements across roughly 101K of SQL: JobScheduleRequiredForm, ResourceLocationListFeed, TrainingReportListFeed, FinancialSummaryFeed, FormSubmissionActivityFeed, FormSubmissionActivityFeedFilters, JobCompletionReportView, LineItemReportFeed.

These are loaded through the container's docker-entrypoint-initdb.d alongside the structure and data dumps, so any environment built the way the dev environment is built has them, and application queries can read them.

Consequence: CREATE VIEW, view resolution during query planning, and information_schema.VIEWS all move into scope. Prisma's migration engine already queries information_schema.VIEWS as its first introspection step (see the information_schema issue), so the catalog side is required regardless.

## Notes

### 2026-08-18T15:37:17Z — Christopher Vachon (user)

Corrected count on 2026-08-18 using a whitespace-normalised scan rather than line greps: 1 CREATE VIEW in the migrations (RecipientListFeed, which splits CREATE OR REPLACE and VIEW across two lines and so evaded every single-line pattern) and 39 in provisions/mysql/3-anzen-views.sql. Also note that a fresh container built from those provisions currently fails: the views file references u.defaultPaidLeavePayrollExport, which the 2024-11-14 structure dump does not define.

### 2026-08-18T18:52:11Z — Christopher Vachon (user)

Implemented 2026-08-18. CREATE VIEW, CREATE OR REPLACE VIEW and DROP VIEW are supported; a view is stored as its definition text and materialised on read, so it reflects later writes rather than being a copy. Views are queryable, filterable and joinable like tables, appear in information_schema.VIEWS with their definition, and appear in information_schema.TABLES with TABLE_TYPE = VIEW so Prisma's introspection filter excludes them from base tables. A depth limit refuses a self-referential view.
