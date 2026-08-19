// The cost of sharing, demonstrated rather than hidden.
//
// This file writes to the same database as every other one, so it can see
// their rows and they can see its. Nothing here can assert an exact count, and
// two files claiming the same unique value would make one of them fail
// depending on order. `test/isolated` is the same suite without that problem.

import { authors, connect } from '@mysql-ultralight-example/app';

// The URL `globalSetup` put in the environment; the app hands out the client
// so this package needs no dependency on Prisma of its own.
const db = connect();

afterAll(async () => {
    await db.$disconnect();
});

it('shares the database with every other file in the run', async () => {
    await authors.register(db, { email: 'leaks@example.com', name: 'Leaks' });

    // At least the seed and this row. Possibly more, if the other file has
    // already run — which is the point.
    expect(await db.author.count()).toBeGreaterThanOrEqual(3);
});
