
import { expect, it } from 'vitest';
import { authors, posts } from '@mysql-ultralight-example/app';

import { db } from '../support/per-file';

it('claims the same email the sibling file claims', async () => {
    // The collision a shared database would produce is exactly what a database
    // per file removes.
    const created = await authors.register(db(), {
        email: 'claimed@example.com',
        name: 'Posts File'
    });
    expect(created.id).toBeTruthy();
});

it('slugifies a title and attaches tags, creating the new ones', async () => {
    const created = await posts.draft(db(), {
        authorEmail: 'ada@example.com',
        title: 'On Computable Numbers!',
        body: 'a'.repeat(80),
        tags: ['engineering', 'logic']
    });

    expect(created.slug).toBe('on-computable-numbers');
    expect(created.tags.map((t) => t.name).sort()).toEqual(['engineering', 'logic']);

    // `engineering` was seeded, `logic` was not: one reused, one created.
    const counts = await posts.tagCounts(db());
    expect(counts.find((t) => t.name === 'logic')?.posts).toBe(1);
    expect(counts.find((t) => t.name === 'engineering')?.posts).toBe(2);
});

it('publishes a draft, filling the excerpt from the body', async () => {
    const created = await posts.draft(db(), {
        authorEmail: 'ada@example.com',
        title: 'A Long Read',
        body: 'b'.repeat(80)
    });
    expect(created.excerpt).toBeNull();

    const published = await posts.publish(db(), created.id);
    expect(published.status).toBe('PUBLISHED');
    expect(published.excerpt).toBe(`${'b'.repeat(60)}...`);

    // Publishing twice would otherwise reset the excerpt of a live post.
    await expect(posts.publish(db(), created.id)).rejects.toThrow(/already published/);
});

it('counts a view without reading it into the process first', async () => {
    const [published] = await posts.inState(db(), 'PUBLISHED');
    const before = published.views;

    await Promise.all([
        posts.recordView(db(), published.id),
        posts.recordView(db(), published.id),
        posts.recordView(db(), published.id)
    ]);

    // Three concurrent increments, none lost: the arithmetic happens in the
    // database rather than in three copies of the old value.
    const after = await db().post.findUniqueOrThrow({ where: { id: published.id } });
    expect(after.views).toBe(before + 3);
});
