// One database for the whole run: `globalSetup` started it and put its URL in
// DATABASE_URL, so a client with no arguments finds it.
//
// Note what these assertions can and cannot say. The seeded row is stable, and
// a row this file writes is stable. A *count* is not, because another file may
// have written before this one ran. That defensiveness is the tax of sharing a
// database, and it is why `test/isolated` exists.

import { authors, connect } from '@mysql-ultralight-example/app';

// The URL `globalSetup` put in the environment; the app hands out the client
// so this package needs no dependency on Prisma of its own.
const db = connect();

afterAll(async () => {
    await db.$disconnect();
});

it('starts with the schema and the seed the image carries', async () => {
    const ada = await authors.byEmail(db, 'ada@example.com');
    expect(ada?.name).toBe('Ada Lovelace');
});

it('writes into the database the whole run shares', async () => {
    const created = await authors.register(db, { email: 'shared@example.com', name: 'Shared' });

    // The row this file wrote, not the total: the total depends on which other
    // files have run.
    const found = await authors.byEmail(db, 'shared@example.com');
    expect(found?.id).toBe(created.id);
});
