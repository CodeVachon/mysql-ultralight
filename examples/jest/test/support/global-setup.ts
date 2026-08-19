// The drop-in model: one database for the whole run.
//
// A suite backed by a real MySQL usually has a `globalSetup` that brings a
// database up, migrates it, and puts its URL in DATABASE_URL. This replaces
// the inside of that function and nothing else — the migrations became an
// image, and loading one is about a millisecond.

import { startDatabase, type Database } from '@mysql-ultralight-example/app';

declare global {
    // eslint-disable-next-line no-var
    var __DATABASE__: Database | undefined;
}

export default async function globalSetup(): Promise<void> {
    const database = await startDatabase();

    // Set before Jest forks its workers, so every one of them inherits it.
    process.env.DATABASE_URL = database.server.url;
    // Teardown runs in this same process, so the handle can be left here.
    globalThis.__DATABASE__ = database;
}
