'use strict';

// The claim the project rests on: an image baked from a migration history is
// the same database the history produces, and the engine can be handed either.
//
// These are engine-level rather than application-level, which is why they live
// with the plainest runner rather than in the Jest or Vitest examples.

const assert = require('node:assert/strict');
const { execFileSync } = require('node:child_process');
const path = require('node:path');
const { after, describe, it } = require('node:test');

// Everything comes from the app: it owns the schema, the migrations and the
// two ways of getting a database, so this package holds nothing but tests.
const { connect, imagePath, startDatabase, startEmpty } = require('@mysql-ultralight-example/app');

// The app owns the schema and the migrations, so Prisma is run from there.
const app = path.dirname(require.resolve('@mysql-ultralight-example/app/package.json'));

const running = [];
after(async () => {
    await Promise.allSettled(running.map((started) => started.stop()));
});

async function serve({ image, database } = {}) {
    const started = image
        ? await startDatabase()
        : await startEmpty(database ? { database } : {});
    running.push(started);
    return started.server;
}

function prisma(args, url) {
    // `--no-install` so a partial install cannot make this fetch a different
    // Prisma from the registry mid-run.
    return execFileSync('npx', ['--no-install', 'prisma', ...args], {
        cwd: app,
        env: { ...process.env, DATABASE_URL: url },
        encoding: 'utf8'
    });
}

/**
 * The schema as `information_schema` describes it.
 *
 * Compared rather than eyeballed, and deliberately wider than the columns:
 * indexes and foreign keys are most of what a schema *is*, and a lost compound
 * unique or a dropped ON DELETE would pass a comparison of columns alone.
 */
async function shape(db) {
    const columns = await db.$queryRawUnsafe(`
        SELECT TABLE_NAME, COLUMN_NAME, COLUMN_TYPE, IS_NULLABLE, COLUMN_DEFAULT,
               COLUMN_KEY, EXTRA, CHARACTER_MAXIMUM_LENGTH, NUMERIC_PRECISION,
               NUMERIC_SCALE, DATETIME_PRECISION
        FROM information_schema.COLUMNS
        WHERE TABLE_SCHEMA = 'app'
        ORDER BY TABLE_NAME, COLUMN_NAME
    `);
    const tables = await db.$queryRawUnsafe(`
        SELECT TABLE_NAME, TABLE_TYPE FROM information_schema.TABLES
        WHERE TABLE_SCHEMA = 'app' ORDER BY TABLE_NAME
    `);
    const indexes = await db.$queryRawUnsafe(`
        SELECT TABLE_NAME, INDEX_NAME, SEQ_IN_INDEX, COLUMN_NAME, NON_UNIQUE
        FROM information_schema.STATISTICS
        WHERE TABLE_SCHEMA = 'app'
        ORDER BY TABLE_NAME, INDEX_NAME, SEQ_IN_INDEX
    `);
    const foreignKeys = await db.$queryRawUnsafe(`
        SELECT kcu.TABLE_NAME, kcu.CONSTRAINT_NAME, kcu.COLUMN_NAME,
               kcu.REFERENCED_TABLE_NAME, kcu.REFERENCED_COLUMN_NAME,
               rc.DELETE_RULE, rc.UPDATE_RULE
        FROM information_schema.KEY_COLUMN_USAGE kcu
        JOIN information_schema.REFERENTIAL_CONSTRAINTS rc
          ON rc.CONSTRAINT_NAME = kcu.CONSTRAINT_NAME
        WHERE kcu.TABLE_SCHEMA = 'app' AND kcu.REFERENCED_TABLE_NAME IS NOT NULL
        ORDER BY kcu.TABLE_NAME, kcu.CONSTRAINT_NAME, kcu.COLUMN_NAME
    `);
    return { tables, columns, indexes, foreignKeys };
}

describe('the migration history', () => {
    it('applies from empty', async () => {
        const server = await serve();
        const output = prisma(['migrate', 'deploy'], server.url);

        assert.match(output, /4 migrations found/);
        assert.match(output, /successfully applied/);

        const db = connect(server.url);
        try {
            const { tables } = await shape(db);
            assert.deepEqual(tables.map((t) => t.TABLE_NAME).sort(), [
                'Author',
                'Post',
                'Profile',
                'Tag',
                '_PostToTag',
                '_prisma_migrations'
            ]);

            // The ALTER and the data migration ran, not just the CREATEs.
            // Asserted exactly: a regex ending in `...` would hold however
            // LEFT() behaved, including not truncating at all.
            const [post] = await db.$queryRawUnsafe(
                "SELECT excerpt FROM Post WHERE slug = 'on-the-analytical-engine'"
            );
            assert.equal(
                post.excerpt,
                'A note on why a machine that manipulates symbols is not mere...'
            );

            const [draft] = await db.$queryRawUnsafe(
                "SELECT excerpt FROM Post WHERE slug = 'nanoseconds'"
            );
            assert.equal(draft.excerpt, null);
        } finally {
            await db.$disconnect();
        }
    });

    it('produces the same database whether replayed or loaded', async () => {
        const migrated = await serve();
        prisma(['migrate', 'deploy'], migrated.url);
        const booted = await serve({ image: imagePath() });

        const a = connect(migrated.url);
        const b = connect(booted.url);
        try {
            const fromMigrations = await shape(a);
            const fromImage = await shape(b);

            // Both sides come from this engine, so two empty results would
            // compare equal and prove nothing. Pin the size first.
            assert.equal(fromMigrations.tables.length, 6);
            assert.equal(fromMigrations.columns.length, 35);
            assert.equal(fromMigrations.foreignKeys.length, 4);
            assert.ok(fromMigrations.indexes.length >= 10);

            assert.deepEqual(fromImage, fromMigrations);
        } finally {
            await a.$disconnect();
            await b.$disconnect();
        }
    });

    it('leaves nothing for prisma to apply when booted from an image', async () => {
        // The whole point: a test process can skip the migrations entirely and
        // still be a database Prisma agrees is up to date.
        const server = await serve({ image: imagePath() });
        assert.match(prisma(['migrate', 'deploy'], server.url), /No pending migrations to apply/);
    });

    it('builds exactly what the schema describes', async () => {
        // The migrations are written by hand, so "they apply" is not the same
        // as "they are right". Prisma compares the history against the
        // datamodel; anything but an empty result means the example is testing
        // a fiction. The shadow database is this engine too, so this also
        // depends on introspection working.
        const shadow = await serve({ database: 'shadow' });
        const diff = prisma([
            'migrate',
            'diff',
            '--from-migrations',
            'prisma/migrations',
            '--to-schema-datamodel',
            'prisma/schema.prisma',
            '--shadow-database-url',
            shadow.url,
            '--script'
        ]);
        assert.match(diff, /This is an empty migration/, `drift:\n${diff}`);
    });

    it('boots from the image far faster than it replays the history', async () => {
        // Not a benchmark — a guard. If loading an image ever costs what
        // replaying migrations costs, the reason this project exists is gone.
        const server = await serve({ image: imagePath() });
        assert.ok(server.startupMs < 100, `booting took ${server.startupMs}ms`);
    });
});
