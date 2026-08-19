// Everything a test runner needs from this app, in one place so the runner
// examples contain nothing but tests.
//
// The split here is deliberate and is what makes a database per test file
// work. `bake()` is expensive and runs **once**, from a script, before any
// runner starts. `startDatabase()` is what tests call, and it only ever loads
// the image `bake()` produced — about a millisecond, with nothing to race over
// and no migration to time out on.

import { createHash } from 'node:crypto';
import { execFileSync } from 'node:child_process';
import fs from 'node:fs';
import path from 'node:path';

import { start, resolveBinary, type Server } from '@codevachon/mysql-ultralight';
import { PrismaClient } from '@prisma/client';

const root = path.join(__dirname, '..');
const migrations = path.join(root, 'prisma', 'migrations');
const cache = path.join(root, '.cache');

/** A database, and a client pointed at it. Stop it when the test file ends. */
export interface Database {
    server: Server;
    db: PrismaClient;
    stop(): Promise<void>;
}

/**
 * A hash of the migrations and the engine that will apply them.
 *
 * The image is reused until either changes. Keying it more loosely eventually
 * runs a suite against a schema its migrations no longer describe, and those
 * failures land nowhere near the cause.
 */
function fingerprint(): string {
    const hash = createHash('sha256');
    const walk = (directory: string) => {
        for (const entry of fs
            .readdirSync(directory, { withFileTypes: true })
            .sort((a, b) => a.name.localeCompare(b.name))) {
            const full = path.join(directory, entry.name);
            if (entry.isDirectory()) {
                walk(full);
            } else {
                // The path, not the bare name: every file here is called
                // `migration.sql`, so hashing names alone leaves a renamed
                // migration directory with an unchanged hash.
                hash.update(path.relative(migrations, full));
                hash.update(fs.readFileSync(full));
            }
        }
    };
    walk(migrations);

    const binary = fs.statSync(resolveBinary());
    hash.update(`${binary.size}:${binary.mtimeMs}`);
    return hash.digest('hex').slice(0, 16);
}

/** Where the image for the current migrations and engine lives. */
export function imagePath(): string {
    return path.join(cache, `${fingerprint()}.mus`);
}

/**
 * Applies the migrations once and snapshots the result.
 *
 * Called from `npm run bake`, which every runner's `pretest` invokes — one
 * process, before any worker exists. Doing this inside a test hook instead
 * would run it once per worker on a cold cache, and would put a multi-second
 * migration inside a hook with a five-second timeout.
 */
export async function bake({ quiet = false } = {}): Promise<string> {
    const image = imagePath();
    if (fs.existsSync(image)) return image;

    fs.mkdirSync(cache, { recursive: true });
    const current = path.basename(image);
    for (const stale of fs.readdirSync(cache)) {
        if (stale.endsWith('.mus') && stale !== current) {
            fs.rmSync(path.join(cache, stale), { force: true });
        }
    }

    const server = await start({ database: 'app' });
    try {
        // Output is inherited, not discarded: when a migration fails, the
        // engine's message is the whole reason anyone is reading this.
        execFileSync('npx', ['--no-install', 'prisma', 'migrate', 'deploy'], {
            cwd: root,
            env: { ...process.env, DATABASE_URL: server.url },
            stdio: quiet ? ['ignore', 'ignore', 'inherit'] : 'inherit'
        });

        // The snapshot is an ordinary SELECT, so any MySQL client can take it.
        const prisma = new PrismaClient({ datasources: { db: { url: server.url } } });
        try {
            await prisma.$queryRawUnsafe(`SELECT ultralight_snapshot('${image}')`);
        } finally {
            await prisma.$disconnect();
        }
    } finally {
        await server.stop();
    }
    return image;
}

/**
 * An empty database — no image, no schema.
 *
 * Only the migration tests want this: everything else starts from the image,
 * because replaying a history per test file is the cost this project exists to
 * remove.
 */
export async function startEmpty(options: { database?: string } = {}): Promise<Database> {
    const server = await start({ database: options.database ?? 'app' });
    const db = new PrismaClient({ datasources: { db: { url: server.url } } });
    return {
        server,
        db,
        async stop() {
            try {
                await db.$disconnect();
            } finally {
                await server.stop();
            }
        }
    };
}

/**
 * A client bound to a URL, defaulting to `DATABASE_URL`.
 *
 * What the shared model uses: `globalSetup` puts a URL in the environment and
 * every test file connects to it. Exported from here so a runner example needs
 * no dependency on Prisma at all — this package owns the schema, so it owns
 * the client generated from it.
 */
export function connect(url: string | undefined = process.env.DATABASE_URL): PrismaClient {
    if (!url) {
        throw new Error('connect() needs a URL, or DATABASE_URL to be set');
    }
    return new PrismaClient({ datasources: { db: { url } } });
}

/**
 * A database of this test file's own, loaded from the baked image.
 *
 * Fails loudly rather than baking on the spot: a test hook is the wrong place
 * to discover that the migrations have not run yet.
 */
export async function startDatabase(): Promise<Database> {
    const image = imagePath();
    if (!fs.existsSync(image)) {
        throw new Error(
            `no state image at ${image}. Run \`npm run bake\` in examples/app first — ` +
                "every runner's `pretest` does this for you."
        );
    }

    const server = await start({ image, database: 'app' });
    const db = new PrismaClient({ datasources: { db: { url: server.url } } });

    return {
        server,
        db,
        // The server is stopped whatever the client does: it is a detached
        // child, so a rejected disconnect would otherwise leave it running.
        async stop() {
            try {
                await db.$disconnect();
            } finally {
                await server.stop();
            }
        }
    };
}
