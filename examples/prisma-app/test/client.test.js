'use strict';

// Prisma Client against the engine, through the npm package — the seam where a
// real driver's expectations meet the wire protocol and the SQL at once.
//
// Every test starts its own server from the baked image, which costs about a
// millisecond and is what makes per-test isolation affordable.

const assert = require('node:assert/strict');
const { after, before, describe, it } = require('node:test');

const { start } = require('@codevachon/mysql-ultralight');
const { bakedImage, client } = require('../scripts/bake');

let image;
const running = [];

before(async () => {
    image = await bakedImage({ quiet: true });
});

after(async () => {
    // Every server is stopped even if its client fails to disconnect: the
    // child is unref'd, so one rejected promise here leaves a process behind.
    await Promise.allSettled(
        running.map(async ({ server, prisma }) => {
            try {
                await prisma.$disconnect();
            } finally {
                await server.stop();
            }
        })
    );
});

/** A server of this test's own, with a client pointed at it. */
async function fresh() {
    const server = await start({ image, database: 'app' });
    const prisma = client(server.url);
    running.push({ server, prisma });
    return prisma;
}

describe('reads', () => {
    it('finds, filters, orders and paginates', async () => {
        const prisma = await fresh();

        const published = await prisma.post.findMany({
            where: { status: 'PUBLISHED' },
            orderBy: { views: 'desc' }
        });
        assert.equal(published.length, 1);
        assert.equal(published[0].slug, 'on-the-analytical-engine');
        // Decimal and JSON come back as themselves, not as strings.
        assert.equal(published[0].price.toString(), '9.99');
        assert.deepEqual(published[0].metadata, { featured: true, tags: ['classic'] });

        const page = await prisma.author.findMany({ orderBy: { email: 'asc' }, take: 1, skip: 1 });
        assert.equal(page.length, 1);
        assert.equal(page[0].email, 'grace@example.com');
    });

    it('follows relations in both directions', async () => {
        const prisma = await fresh();

        const ada = await prisma.author.findUnique({
            where: { email: 'ada@example.com' },
            include: { posts: { include: { tags: true } } }
        });
        assert.equal(ada.name, 'Ada Lovelace');
        assert.equal(ada.posts.length, 1);
        assert.deepEqual(
            ada.posts[0].tags.map((t) => t.name).sort(),
            ['engineering', 'history']
        );

        const tagged = await prisma.tag.findMany({
            where: { posts: { some: { status: 'PUBLISHED' } } },
            orderBy: { name: 'asc' }
        });
        assert.deepEqual(tagged.map((t) => t.name), ['engineering', 'history']);
    });

    it('aggregates and groups', async () => {
        const prisma = await fresh();

        const totals = await prisma.post.aggregate({
            _count: { _all: true },
            _sum: { views: true },
            _max: { views: true }
        });
        assert.equal(totals._count._all, 2);
        assert.equal(totals._sum.views, 120);
        assert.equal(totals._max.views, 120);

        const byStatus = await prisma.post.groupBy({
            by: ['status'],
            _count: { _all: true },
            orderBy: { status: 'asc' }
        });
        assert.deepEqual(
            byStatus.map((r) => [r.status, r._count._all]),
            [
                ['DRAFT', 1],
                ['PUBLISHED', 1]
            ]
        );
    });
});

describe('writes', () => {
    it('creates a graph in one call', async () => {
        const prisma = await fresh();

        const created = await prisma.author.create({
            data: {
                email: 'alan@example.com',
                name: 'Alan Turing',
                role: 'EDITOR',
                profile: { create: { website: 'https://example.com', settings: { theme: 'dark' } } },
                posts: {
                    create: [
                        {
                            title: 'On Computable Numbers',
                            slug: 'on-computable-numbers',
                            status: 'PUBLISHED',
                            tags: {
                                connectOrCreate: [
                                    { where: { name: 'history' }, create: { name: 'history' } },
                                    { where: { name: 'logic' }, create: { name: 'logic' } }
                                ]
                            }
                        }
                    ]
                }
            },
            include: { profile: true, posts: { include: { tags: true } } }
        });

        // The generated id is a cuid, not something the test supplied.
        assert.match(created.id, /^c[a-z0-9]+$/);
        assert.equal(created.profile.settings.theme, 'dark');
        assert.deepEqual(created.posts[0].tags.map((t) => t.name).sort(), ['history', 'logic']);
        // `connectOrCreate` reused the seeded tag rather than duplicating it.
        assert.equal(await prisma.tag.count(), 4);
    });

    it('updates, upserts and deletes', async () => {
        const prisma = await fresh();

        const updated = await prisma.post.update({
            where: { id: '22222222-2222-4222-8222-222222222222' },
            data: { status: 'PUBLISHED', views: { increment: 5 } }
        });
        assert.equal(updated.status, 'PUBLISHED');
        // `increment` is server-side arithmetic, not a read-then-write.
        assert.equal(updated.views, 5);
        // Prisma sends its own timestamp for @updatedAt, so this says the
        // column round-trips with sub-second precision — not that the engine
        // restamped anything. `DATETIME(3)` truncating to whole seconds would
        // make these equal.
        assert.notEqual(
            updated.updatedAt.toISOString(),
            updated.createdAt.toISOString()
        );

        // The upsert has to find the seeded row rather than make a new one.
        const seeded = await prisma.tag.findUnique({ where: { name: 'engineering' } });
        const upserted = await prisma.tag.upsert({
            where: { name: 'engineering' },
            create: { name: 'engineering' },
            update: { name: 'engineering' }
        });
        assert.equal(upserted.id, seeded.id);
        assert.equal(await prisma.tag.count(), 3);

        await prisma.author.delete({ where: { email: 'grace@example.com' } });
        // The cascade took the post with it.
        assert.equal(await prisma.post.count(), 1);
    });

    it('commits a transaction', async () => {
        const prisma = await fresh();
        const before = await prisma.author.count();

        await prisma.$transaction(async (tx) => {
            await tx.author.create({ data: { email: 'committed@example.com', name: 'Committed' } });
            await tx.tag.create({ data: { name: 'committed' } });
        });

        assert.equal(await prisma.author.count(), before + 1);
        assert.ok(await prisma.author.findUnique({ where: { email: 'committed@example.com' } }));
        assert.ok(await prisma.tag.findUnique({ where: { name: 'committed' } }));
    });

    it('rolls a transaction back', async () => {
        const prisma = await fresh();
        const before = await prisma.author.count();

        let written = null;
        await assert.rejects(
            () =>
                prisma.$transaction(async (tx) => {
                    // Held onto so the test can prove the row really existed
                    // inside the transaction. Without that, everything below
                    // passes just as well against an engine that wrote nothing.
                    written = await tx.author.create({
                        data: { email: 'rolled@example.com', name: 'Rolled Back' }
                    });
                    assert.ok(written.id);
                    assert.equal(await tx.author.count(), before + 1);

                    // A duplicate email, so the transaction cannot commit.
                    await tx.author.create({ data: { email: 'ada@example.com', name: 'Clash' } });
                }),
            /Unique constraint failed/
        );

        assert.ok(written, 'the first insert never ran, so nothing was rolled back');
        assert.equal(await prisma.author.count(), before);
        assert.equal(await prisma.author.findUnique({ where: { email: 'rolled@example.com' } }), null);
    });

    it('enforces the constraints the schema declares', async () => {
        const prisma = await fresh();

        // The compound unique on (authorId, slug).
        await assert.rejects(
            () =>
                prisma.post.create({
                    data: {
                        authorId: 'seed_author_ada',
                        title: 'Duplicate',
                        slug: 'on-the-analytical-engine'
                    }
                }),
            /Unique constraint failed/
        );

        // The foreign key on Post.authorId.
        await assert.rejects(
            () =>
                prisma.post.create({
                    data: { authorId: 'nobody', title: 'Orphan', slug: 'orphan' }
                }),
            /Foreign key constraint/
        );
    });
});

describe('raw queries', () => {
    it('reads columns by name', async () => {
        // The regression this example was written for: prepared statements once
        // declared no columns, so every raw result came back positionally and
        // a lookup by name found nothing. Prisma Client hid it by mapping on
        // position from its own schema.
        const prisma = await fresh();

        const rows = await prisma.$queryRawUnsafe(
            "SELECT slug AS post_slug, views AS view_count FROM Post WHERE status = 'PUBLISHED'"
        );
        assert.equal(rows.length, 1);
        assert.deepEqual(Object.keys(rows[0]).sort(), ['post_slug', 'view_count']);
        assert.equal(rows[0].post_slug, 'on-the-analytical-engine');
    });

    it('runs the SQL an application writes by hand', async () => {
        const prisma = await fresh();

        const rows = await prisma.$queryRawUnsafe(`
            SELECT a.name AS author, COUNT(p.id) AS posts,
                   JSON_ARRAYAGG(p.status) AS statuses
            FROM Author a
            LEFT JOIN Post p ON p.authorId = a.id
            GROUP BY a.id, a.name
            ORDER BY a.name
        `);
        assert.deepEqual(rows.map((r) => r.author), ['Ada Lovelace', 'Grace Hopper']);
        // A number, not the string "1": the engine declares COUNT as an
        // integer and the driver decodes it as one.
        assert.equal(typeof rows[0].posts, 'bigint');
        assert.equal(Number(rows[0].posts), 1);

        const affected = await prisma.$executeRawUnsafe(
            'UPDATE Post SET views = views + 1 WHERE status = ?',
            'PUBLISHED'
        );
        assert.equal(affected, 1);
    });
});
