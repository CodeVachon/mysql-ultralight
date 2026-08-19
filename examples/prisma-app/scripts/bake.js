'use strict';

// Bakes the migration history into a state image, and skips the work when the
// image already matches the migrations that produced it.
//
// This is the workflow the README describes, written once here so the tests use
// the same path a real project would rather than a shortcut of their own.

const { createHash } = require('node:crypto');
const { execFileSync } = require('node:child_process');
const fs = require('node:fs');
const path = require('node:path');

const { start, resolveBinary } = require('@codevachon/mysql-ultralight');

const root = path.join(__dirname, '..');
const migrations = path.join(root, 'prisma', 'migrations');
const cache = path.join(root, '.cache');

/**
 * A hash of every migration, so an image is only reused for the history that
 * produced it.
 *
 * Keying by anything looser — a version, a timestamp — eventually boots a test
 * suite against a schema its migrations no longer describe, and the failures
 * land nowhere near the cause.
 */
function migrationsHash() {
    const hash = createHash('sha256');
    const walk = (directory) => {
        for (const entry of fs.readdirSync(directory, { withFileTypes: true }).sort((a, b) =>
            a.name.localeCompare(b.name)
        )) {
            const full = path.join(directory, entry.name);
            if (entry.isDirectory()) {
                walk(full);
            } else {
                // The path, not the bare name: every file here is called
                // `migration.sql`, so hashing the name alone leaves a renamed
                // migration *directory* with the same hash — and the image
                // reused for it records the old name in `_prisma_migrations`.
                hash.update(path.relative(migrations, full));
                hash.update(fs.readFileSync(full));
            }
        }
    };
    walk(migrations);

    // The engine too: an image is a product of the binary that wrote it, and a
    // rebuilt engine with different behaviour should not inherit an old one.
    const binary = resolveBinary();
    const stat = fs.statSync(binary);
    hash.update(`${stat.size}:${stat.mtimeMs}`);

    return hash.digest('hex').slice(0, 16);
}

/** The image for the current migrations, baking it if it is not there yet. */
async function bakedImage({ quiet = false } = {}) {
    const image = path.join(cache, `${migrationsHash()}.mus`);
    if (fs.existsSync(image)) return image;

    fs.mkdirSync(cache, { recursive: true });
    // Anything left from an earlier history is dead weight. Only images with a
    // *different* name are removed: `node --test` runs the test files in
    // parallel, and deleting the current one would pull it out from under a
    // sibling that has just confirmed it exists.
    const current = path.basename(image);
    for (const stale of fs.readdirSync(cache)) {
        if (stale.endsWith('.mus') && stale !== current) {
            fs.rmSync(path.join(cache, stale), { force: true });
        }
    }

    const server = await start({ database: 'app' });
    try {
        execFileSync('npx', ['--no-install', 'prisma', 'migrate', 'deploy'], {
            cwd: root,
            env: { ...process.env, DATABASE_URL: server.url },
            stdio: quiet ? 'ignore' : 'inherit'
        });

        // The snapshot is an ordinary SELECT, so any MySQL client can take it.
        const prisma = client(server.url);
        try {
            await prisma.$queryRawUnsafe(`SELECT ultralight_snapshot('${image}')`);
        } finally {
            await prisma.$disconnect();
        }
    } finally {
        await server.stop();
    }

    if (!quiet) {
        const size = (fs.statSync(image).size / 1024).toFixed(0);
        console.log(`baked ${path.relative(root, image)} (${size} KB)`);
    }
    return image;
}

/** A Prisma Client bound to a given server, rather than to the environment. */
function client(url) {
    const { PrismaClient } = require('@prisma/client');
    return new PrismaClient({ datasources: { db: { url } } });
}

module.exports = { bakedImage, migrationsHash, client, root };

if (require.main === module) {
    bakedImage().catch((e) => {
        console.error(e.message);
        process.exit(1);
    });
}
