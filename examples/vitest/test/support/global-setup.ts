// The drop-in model: one database for the whole run.
//
// Vitest's `globalSetup` returns its own teardown, so there is no second file
// and no global to stash a handle in — the database is closed over.

import { startDatabase } from '@mysql-ultralight-example/app';

export default async function setup() {
    const database = await startDatabase();

    // Vitest runs test files in forked child processes by default, and forks
    // them after this has run, so they inherit this.
    process.env.DATABASE_URL = database.server.url;

    return async () => {
        await database.stop();
    };
}
