// One database per test file.
//
// `setupFilesAfterEnv` runs once per test file inside its worker, so the
// `beforeAll` here means "before this file". It only *loads* an image — the
// migrations already ran, once, in `pretest` — so it costs about a millisecond
// and cannot time out or race with another worker.

import { startDatabase, type Database, type PrismaClient } from '@mysql-ultralight-example/app';

let database: Database | undefined;
// Held so teardown can wait for a start that is still in flight: if `beforeAll`
// times out, the hook is abandoned but the server it asked for still arrives,
// and nothing else would ever stop it.
let starting: Promise<Database> | undefined;

beforeAll(async () => {
    starting = startDatabase();
    database = await starting;
});

afterAll(async () => {
    const started = database ?? (await starting?.catch(() => undefined));
    await started?.stop();
    database = undefined;
    starting = undefined;
});

/** The database belonging to the test file that calls it. */
export function db(): PrismaClient {
    if (!database) {
        throw new Error('db() was called before beforeAll ran');
    }
    return database.db;
}
