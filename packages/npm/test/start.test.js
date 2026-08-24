'use strict';

const assert = require('node:assert/strict');
const { spawn } = require('node:child_process');
const fs = require('node:fs');
const net = require('node:net');
const os = require('node:os');
const path = require('node:path');
const { after, describe, it } = require('node:test');

const { start, platformKey, PLATFORMS, resolveBinary } = require('../src/index');

/** Servers started by a test, stopped however the test ends. */
const running = [];
after(async () => {
    await Promise.all(running.map((server) => server.stop()));
});

async function startTracked(options) {
    const server = await start(options);
    running.push(server);
    return server;
}

/**
 * The raw ready line, read straight from a server spawned for the purpose.
 *
 * Not read through `start`, which parses the line and keeps only the fields it
 * has a use for: a contract asserted through the parser cannot see a field
 * that stopped being printed.
 */
function readyLine() {
    return new Promise((resolve, reject) => {
        const child = spawn(resolveBinary(), ['--port', '0'], {
            stdio: ['ignore', 'pipe', 'pipe']
        });
        let stdout = '';
        const timer = setTimeout(() => {
            child.kill('SIGKILL');
            reject(new Error(`no ready line within 10s; stdout was "${stdout}"`));
        }, 10_000);
        child.stdout.setEncoding('utf8');
        child.stdout.on('data', (chunk) => {
            stdout += chunk;
            const line = stdout.split('\n').find((l) => l.startsWith('ready '));
            if (!line) return;
            clearTimeout(timer);
            child.kill('SIGKILL');
            resolve(line.trim());
        });
        // A spawn that fails rejects immediately, but the pending timer would keep
        // the event loop alive for its full 10s and then SIGKILL a child that never
        // started -- so `node --test` would stall long after the test had failed.
        child.on('error', (err) => {
            clearTimeout(timer);
            reject(err);
        });
    });
}

/** Whether a TCP port accepts a connection, which is the only real readiness. */
function reachable(port) {
    return new Promise((resolve) => {
        const socket = net.connect({ port, host: '127.0.0.1' });
        socket.on('connect', () => {
            socket.destroy();
            resolve(true);
        });
        socket.on('error', () => resolve(false));
    });
}

describe('start', () => {
    it('resolves only once the server is accepting connections', async () => {
        const server = await startTracked();

        assert.ok(server.port > 0, 'a free port was chosen');
        assert.ok(Number.isFinite(server.startupMs));
        assert.equal(await reachable(server.port), true);
    });

    it('gives every server its own port', async () => {
        // Parallel test workers must not race for one, which is the whole
        // reason the default is port 0.
        const [a, b] = await Promise.all([startTracked(), startTracked()]);
        assert.notEqual(a.port, b.port);
    });

    it('builds a URL a MySQL client can use', async () => {
        const server = await startTracked({ database: 'shop' });
        assert.equal(server.url, `mysql://root@127.0.0.1:${server.port}/shop`);
    });

    it('puts the socket in the URL when there is one', async () => {
        const server = await startTracked({ socket: true, database: 'shop' });

        assert.ok(server.socketPath, 'a socket path was chosen');
        assert.ok(fs.existsSync(server.socketPath), 'the socket exists');
        assert.equal(
            server.url,
            `mysql://root@localhost/shop?socket=${encodeURIComponent(server.socketPath)}`
        );
    });

    it('carries a password into the URL', async () => {
        const server = await startTracked({ password: 'p@ss word' });
        assert.match(server.url, /^mysql:\/\/root:p%40ss%20word@127\.0\.0\.1:\d+\/test$/);
    });

    it('refuses an image that is not there', async () => {
        await assert.rejects(() => start({ image: '/nonexistent/schema.mus' }), /no state image/);
    });

    it('explains a binary that is not there', () => {
        assert.throws(() => resolveBinary('/nonexistent/mysql-ultralight'), /no binary at/);
    });

    it('reports a server that dies during startup', async () => {
        // Port 1 needs privileges this process does not have, so the server
        // exits before it is ready. The reason has to reach the caller rather
        // than sit in a stream nobody reads.
        await assert.rejects(() => start({ port: 1, readyTimeoutMs: 5_000 }), /before it was ready/);
    });
});

describe('the ready line', () => {
    it('carries an instance id, and a different one per process', async () => {
        // `scripts/differential.sh` reads this field and the harness refuses to
        // compare against a server that will not report it back over SQL --
        // which is what stops a run comparing MySQL against a mysql-ultralight
        // left behind by an earlier one. Nothing else in `scripts/check.sh`
        // spawns the binary and reads the line, so without this the field could
        // be dropped by a refactor and pass the whole gate.
        const [first, second] = await Promise.all([readyLine(), readyLine()]);

        assert.match(
            first,
            /^ready port=\d+ socket=\S+ startup_ms=[\d.]+ instance=[0-9a-f]{16}$/
        );
        const id = (line) => /\binstance=(\S+)/.exec(line)[1];
        assert.notEqual(id(first), id(second), 'two servers must not share an id');
    });
});

describe('stop', () => {
    it('stops the server and can be called again', async () => {
        const server = await start();
        const { port } = server;

        await server.stop();
        assert.equal(await reachable(port), false);

        // A teardown hook running after a failed test must not fail for a
        // second reason.
        await server.stop();
    });

    it('removes the socket it created', async () => {
        const server = await start({ socket: true });
        const { socketPath } = server;

        await server.stop();
        assert.equal(fs.existsSync(socketPath), false);
    });
});

describe('state images', () => {
    it('boots from an image and keeps the schema', async (t) => {
        // Bake: start empty, create a schema, snapshot it. The snapshot is
        // taken over the wire, which is what a real bake does after
        // `prisma migrate deploy`.
        const image = path.join(os.tmpdir(), `mu-npm-${process.pid}.mus`);
        t.after(() => fs.rmSync(image, { force: true }));

        const baking = await startTracked({ database: 'shop' });
        await mysqlQuery(baking.port, 'CREATE TABLE Item (id INT NOT NULL PRIMARY KEY)', 'shop');
        await mysqlQuery(baking.port, 'INSERT INTO Item VALUES (1), (2)', 'shop');
        await mysqlQuery(baking.port, `SELECT ultralight_snapshot('${image}')`, 'shop');
        assert.ok(fs.existsSync(image), 'the image was written');

        const booted = await startTracked({ image, database: 'shop' });
        const rows = await mysqlQuery(booted.port, 'SELECT id FROM Item ORDER BY id', 'shop');
        assert.deepEqual(rows, [['1'], ['2']]);
    });
});

describe('platform resolution', () => {
    it('names a platform npm can package', () => {
        const key = platformKey();
        assert.match(key, /^[a-z0-9]+-[a-z0-9]+(-(gnu|musl))?$/);
        if (process.platform === 'linux') {
            assert.ok(key.endsWith('-gnu') || key.endsWith('-musl'), key);
        }
    });

    it('has a package for every platform it claims to support', () => {
        for (const [key, pkg] of Object.entries(PLATFORMS)) {
            assert.ok(pkg.startsWith('@codevachon/mysql-ultralight-'), pkg);
            assert.ok(pkg.endsWith(key), `${pkg} should end with ${key}`);
        }
    });
});

/**
 * The smallest MySQL client that can run one text query.
 *
 * Written here rather than pulled in as a dependency: this package ships with
 * none, and a test that needed one would be testing the driver as much as the
 * server. Only what these tests use is implemented — `mysql_native_password`
 * with an empty password, one `COM_QUERY`, and text rows.
 */
function mysqlQuery(port, sql, database) {
    return new Promise((resolve, reject) => {
        const socket = net.connect({ port, host: '127.0.0.1' });
        const rows = [];
        let buffer = Buffer.alloc(0);
        let stage = 'greeting';
        let sequence = 0;
        // Set once the column count arrives, then counted down as the column
        // definitions go by. The EOF that follows them is not the end of the
        // result — mistaking it for one reads every result as empty.
        let columnsRemaining = null;
        let inHeader = false;

        const fail = (message) => {
            socket.destroy();
            reject(new Error(message));
        };

        const send = (payload) => {
            const header = Buffer.alloc(4);
            header.writeUIntLE(payload.length, 0, 3);
            header[3] = ++sequence;
            socket.write(Buffer.concat([header, payload]));
        };

        socket.on('error', (e) => reject(e));
        socket.on('data', (chunk) => {
            buffer = Buffer.concat([buffer, chunk]);

            for (;;) {
                if (buffer.length < 4) return;
                const length = buffer.readUIntLE(0, 3);
                if (buffer.length < 4 + length) return;
                const payload = buffer.subarray(4, 4 + length);
                sequence = buffer[3];
                buffer = buffer.subarray(4 + length);

                if (stage === 'greeting') {
                    // Capability flags the server must honour: PROTOCOL_41,
                    // SECURE_CONNECTION, PLUGIN_AUTH and, when a database was
                    // asked for, CONNECT_WITH_DB. The auth response is empty
                    // because these servers take any credentials.
                    let flags = 0x0000_0200 | 0x0000_8000 | 0x0008_0000;
                    if (database) flags |= 0x0000_0008;

                    const header = Buffer.alloc(32);
                    header.writeUInt32LE(flags, 0);
                    header.writeUInt32LE(0xffffff, 4);
                    header[8] = 45; // utf8mb4

                    const parts = [
                        header,
                        Buffer.from('root\0'),
                        Buffer.from([0]) // empty auth response
                    ];
                    if (database) parts.push(Buffer.from(`${database}\0`));
                    parts.push(Buffer.from('mysql_native_password\0'));

                    send(Buffer.concat(parts));
                    stage = 'auth';
                    continue;
                }

                if (stage === 'auth') {
                    if (payload[0] === 0xff) return fail(errorText(payload));
                    if (payload[0] !== 0x00) return fail(`unexpected auth reply 0x${payload[0].toString(16)}`);
                    sequence = -1;
                    send(Buffer.concat([Buffer.from([0x03]), Buffer.from(sql, 'utf8')]));
                    stage = 'result';
                    continue;
                }

                if (payload[0] === 0xff) return fail(errorText(payload));

                if (columnsRemaining === null) {
                    // An OK packet in place of a column count: no result set.
                    if (payload[0] === 0x00 || payload[0] === 0xfe) {
                        socket.end();
                        return resolve([]);
                    }
                    const [count] = lengthEncoded(payload, 0);
                    columnsRemaining = count;
                    inHeader = true;
                    continue;
                }

                if (columnsRemaining > 0) {
                    columnsRemaining -= 1;
                    continue;
                }

                // The one EOF that closes the column definitions, then rows
                // until the EOF that closes the result set.
                if (payload[0] === 0xfe && payload.length < 9) {
                    if (inHeader) {
                        inHeader = false;
                        continue;
                    }
                    socket.end();
                    return resolve(rows);
                }
                inHeader = false;
                rows.push(decodeRow(payload));
            }
        });
    });
}

/** One text-protocol row: length-encoded strings, with 0xFB for NULL. */
function decodeRow(packet) {
    const row = [];
    let at = 0;
    while (at < packet.length) {
        if (packet[at] === 0xfb) {
            row.push(null);
            at += 1;
            continue;
        }
        const [length, size] = lengthEncoded(packet, at);
        at += size;
        row.push(packet.subarray(at, at + length).toString('utf8'));
        at += length;
    }
    return row;
}

function lengthEncoded(buffer, at) {
    const first = buffer[at];
    if (first < 0xfb) return [first, 1];
    if (first === 0xfc) return [buffer.readUInt16LE(at + 1), 3];
    if (first === 0xfd) return [buffer.readUIntLE(at + 1, 3), 4];
    return [Number(buffer.readBigUInt64LE(at + 1)), 9];
}

function errorText(payload) {
    const code = payload.readUInt16LE(1);
    const message = payload.subarray(payload[3] === 0x23 ? 9 : 3).toString('utf8');
    return `ERROR ${code}: ${message}`;
}

describe('lifecycle in a bare script', () => {
    it('stops without anything else keeping the event loop alive', async () => {
        // The child is unref'd so a running server never holds the parent open.
        // That also means nothing waits for its exit, so `await stop()` has to
        // re-attach it — otherwise node exits first and teardown silently never
        // finishes. A test runner keeps the loop alive, which hides this, so it
        // is checked in a script that has nothing else to do.
        const script = `
            const { start } = require(${JSON.stringify(require.resolve('../src/index'))});
            (async () => {
                const server = await start();
                await server.stop();
                console.log('stopped');
            })();
        `;
        const { execFileSync } = require('node:child_process');
        const out = execFileSync(process.execPath, ['-e', script], { encoding: 'utf8' });
        assert.match(out, /stopped/);
    });
});
