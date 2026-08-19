// A database of this file's own, so it can write whatever it likes.
//
// The sibling file registers the same email. Under a shared database one of
// them would fail on the unique index, depending on which ran first.
//
// Note the boundary: isolation is per *file*, not per test. The tests below
// run in order against one database, so each sets up what it needs rather than
// assuming the seed is still intact — one of them deletes it.

import { authors, posts } from '@mysql-ultralight-example/app';

import { db } from '../support/per-file';

it('registers an author and their profile in one write', async () => {
    const created = await authors.register(db(), {
        email: '  Claimed@Example.com  ',
        name: '  Ada  ',
        role: 'EDITOR',
        website: 'https://example.com'
    });

    // Trimmed and lowercased on the way in, so a lookup by the address as
    // typed still finds them.
    expect(created.email).toBe('claimed@example.com');
    expect(created.name).toBe('Ada');
    expect(created.profile?.website).toBe('https://example.com');

    const found = await authors.byEmail(db(), 'CLAIMED@example.com ');
    expect(found?.id).toBe(created.id);
});

it('leaves the profile out when there is no website', async () => {
    const created = await authors.register(db(), {
        email: 'plain@example.com',
        name: 'Plain'
    });
    expect(created.profile).toBeNull();
    expect(created.role).toBe('VIEWER');
});

it('refuses to demote the last admin', async () => {
    // The image seeds exactly one admin.
    await expect(authors.changeRole(db(), 'ada@example.com', 'VIEWER')).rejects.toThrow(
        /last admin/
    );

    // With a second admin in place the same call is allowed.
    await authors.register(db(), { email: 'second@example.com', name: 'Second', role: 'ADMIN' });
    const demoted = await authors.changeRole(db(), 'ada@example.com', 'VIEWER');
    expect(demoted.role).toBe('VIEWER');
});

it('takes an author’s posts with them when they go', async () => {
    const before = await db().post.count();
    expect(before).toBeGreaterThan(0);

    expect(await authors.remove(db(), 'ada@example.com')).toBe(1);
    // Removed by the schema's cascade, not by the controller.
    expect(await db().post.count()).toBeLessThan(before);

    // Removing someone who is not there is not an error, it is nothing.
    expect(await authors.remove(db(), 'nobody@example.com')).toBe(0);
});

it('moves posts between authors in one transaction', async () => {
    // Its own authors and its own post: the test above removed the seeded one.
    await authors.register(db(), { email: 'leaving@example.com', name: 'Leaving' });
    await authors.register(db(), { email: 'heir@example.com', name: 'Heir' });
    await posts.draft(db(), { authorEmail: 'leaving@example.com', title: 'Handover' });

    const moved = await authors.transferPosts(db(), 'leaving@example.com', 'heir@example.com');
    expect(moved).toBe(1);

    const counts = await authors.withPostCounts(db());
    expect(counts.find((a) => a.name === 'Heir')?.posts).toBe(1);
    expect(counts.find((a) => a.name === 'Leaving')?.posts).toBe(0);
});
