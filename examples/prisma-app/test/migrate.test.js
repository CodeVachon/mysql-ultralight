'use strict';

// The claim this example exists to check: an image baked from a migration
// history is the same database the history produces, and the engine can be
// handed either one.

const assert = require('node:assert/strict');
const { execFileSync } = require('node:child_process');
const { after, describe, it } = require('node:test');

const { start } = require('@codevachon/mysql-ultralight');
const { bakedImage, client, root } = require('../scripts/bake');

const running = [];
after(async () => {
    await Promise.all(running.map((server) => server.stop()));
});

async function serve(options) {
    const server = await start({ database: 'app', ...options });
    running.push(server);
    return server;
}

function deploy(url) {
    // `--no-install` so a partial install cannot make the gate fetch a
    // different Prisma from the registry mid-run.
    return execFileSync('npx', ['--no-install', 'prisma', 'migrate', 'deploy'], {
        cwd: root,
        env: { ...process.env, DATABASE_URL: url },
        encoding: 'utf8'
    });
}

/**
 * The schema as `information_schema` describes it.
 *
 * Compared rather than eyeballed: "the image matches the migrations" is only
 * worth asserting if it covers types, nullability, defaults and keys, not just
 * the table names.
 */
async function shape(prisma) {
    const columns = await prisma.$queryRawUnsafe(`
        SELECT TABLE_NAME, COLUMN_NAME, COLUMN_TYPE, IS_NULLABLE, COLUMN_DEFAULT,
               COLUMN_KEY, EXTRA, CHARACTER_MAXIMUM_LENGTH, NUMERIC_PRECISION,
               NUMERIC_SCALE, DATETIME_PRECISION
        FROM information_schema.COLUMNS
        WHERE TABLE_SCHEMA = 'app'
        ORDER BY TABLE_NAME, COLUMN_NAME
    `);
    const tables = await prisma.$queryRawUnsafe(`
        SELECT TABLE_NAME, TABLE_TYPE
        FROM information_schema.TABLES
        WHERE TABLE_SCHEMA = 'app'
        ORDER BY TABLE_NAME
    `);
    // Indexes and foreign keys are most of what a schema *is*, and neither is
    // visible in COLUMNS: a lost compound unique or a dropped ON DELETE would
    // pass a comparison of columns alone.
    const indexes = await prisma.$queryRawUnsafe(`
        SELECT TABLE_NAME, INDEX_NAME, SEQ_IN_INDEX, COLUMN_NAME, NON_UNIQUE
        FROM information_schema.STATISTICS
        WHERE TABLE_SCHEMA = 'app'
        ORDER BY TABLE_NAME, INDEX_NAME, SEQ_IN_INDEX
    `);
    const foreignKeys = await prisma.$queryRawUnsafe(`
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
        const output = deploy(server.url);

        assert.match(output, /4 migrations found/);
        assert.match(output, /successfully applied/);

        const prisma = client(server.url);
        try {
            const { tables } = await shape(prisma);
            const names = tables.map((t) => t.TABLE_NAME).sort();
            assert.deepEqual(names, [
                'Author',
                'Post',
                'Profile',
                'Tag',
                '_PostToTag',
                '_prisma_migrations'
            ]);

            // The ALTER and the data migration ran, not just the CREATEs.
            // Asserted exactly: `/\.\.\.$/` would hold however LEFT() behaved,
            // including not truncating at all.
            const [post] = await prisma.$queryRawUnsafe(
                "SELECT excerpt FROM Post WHERE slug = 'on-the-analytical-engine'"
            );
            assert.equal(
                post.excerpt,
                'A note on why a machine that manipulates symbols is not mere...'
            );

            // The row whose body is NULL took the other branch and stayed NULL.
            const [draft] = await prisma.$queryRawUnsafe(
                "SELECT excerpt FROM Post WHERE slug = 'nanoseconds'"
            );
            assert.equal(draft.excerpt, null);
        } finally {
            await prisma.$disconnect();
        }
    });

    it('produces the same database whether replayed or loaded', async () => {
        const migrated = await serve();
        deploy(migrated.url);

        const booted = await serve({ image: await bakedImage({ quiet: true }) });

        const a = client(migrated.url);
        const b = client(booted.url);
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
        const server = await serve({ image: await bakedImage({ quiet: true }) });
        assert.match(deploy(server.url), /No pending migrations to apply/);
    });

    it('carries its seed data into the image', async () => {
        const server = await serve({ image: await bakedImage({ quiet: true }) });
        const prisma = client(server.url);
        try {
            assert.equal(await prisma.author.count(), 2);
            assert.equal(await prisma.tag.count(), 3);
            assert.equal(await prisma.post.count(), 2);
        } finally {
            await prisma.$disconnect();
        }
    });

    it('builds exactly what the schema describes', async (t) => {
        // The migrations are written by hand, so "they apply" is not the same
        // as "they are right". Prisma compares the history against the
        // datamodel and emits the SQL that would reconcile them; anything but
        // an empty result means the example is testing a fiction.
        //
        // The shadow database is this engine too, which makes the check depend
        // on introspection working — `prisma db pull` and `migrate dev` read
        // the same describer.
        const shadow = await serve({ database: 'shadow' });
        t.after(() => shadow.stop());

        const diff = execFileSync(
            'npx',
            [
                '--no-install',
                'prisma',
                'migrate',
                'diff',
                '--from-migrations',
                'prisma/migrations',
                '--to-schema-datamodel',
                'prisma/schema.prisma',
                '--shadow-database-url',
                shadow.url,
                '--script'
            ],
            { cwd: root, encoding: 'utf8' }
        );
        assert.match(diff, /This is an empty migration/, `drift:\n${diff}`);
    });

    it('boots from the image far faster than it replays the history', async () => {
        // Not a benchmark — a guard. If loading an image ever costs what
        // replaying migrations costs, the reason this project exists is gone.
        const server = await serve({ image: await bakedImage({ quiet: true }) });
        assert.ok(
            server.startupMs < 100,
            `booting from the image took ${server.startupMs}ms`
        );
    });
});
