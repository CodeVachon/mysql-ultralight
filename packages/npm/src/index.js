'use strict';

const { spawn } = require('node:child_process');
const fs = require('node:fs');
const path = require('node:path');

const { resolveBinary, platformKey, PLATFORMS } = require('./binary');

/** How long to wait for the server to announce itself before giving up. */
const DEFAULT_READY_TIMEOUT_MS = 10_000;

/**
 * Starts a server and resolves once it is accepting connections.
 *
 * The server prints one line when it is ready, carrying the port it actually
 * bound. That is what makes port 0 usable: the OS picks a free port, so test
 * workers running in parallel cannot race for one, and nothing has to poll a
 * socket to find out when the server is up.
 *
 * @param {object} [options]
 * @param {string} [options.image] Path to a baked state image to boot from.
 * @param {string} [options.database] Database to create at startup.
 * @param {number} [options.port] TCP port; 0 (the default) picks a free one.
 * @param {boolean|string} [options.socket] Unix socket: true for a temporary
 *   path, or a path of your own.
 * @param {string} [options.password] Require this password.
 * @param {string} [options.serverVersion] Version string reported to clients.
 * @param {boolean} [options.trace] Log every command to stderr.
 * @param {string} [options.binary] Use this binary instead of the resolved one.
 * @param {number} [options.readyTimeoutMs]
 * @returns {Promise<Server>}
 */
async function start(options = {}) {
    const {
        image,
        database = 'test',
        port = 0,
        socket = false,
        password,
        serverVersion,
        trace = false,
        binary,
        readyTimeoutMs = DEFAULT_READY_TIMEOUT_MS
    } = options;

    if (image && !fs.existsSync(image)) {
        throw new Error(`mysql-ultralight: no state image at ${image}`);
    }

    const socketPath = socket === true ? temporarySocketPath() : socket || null;
    const executable = resolveBinary(binary);

    const args = ['--port', String(port), '--database', database];
    if (socketPath) args.push('--socket', socketPath);
    if (image) args.push('--state', image);
    if (password) args.push('--password', password);
    if (serverVersion) args.push('--server-version', serverVersion);
    if (trace) args.push('--trace');

    const child = spawn(executable, args, { stdio: ['ignore', 'pipe', 'pipe'] });

    let stderr = '';
    child.stderr.setEncoding('utf8');
    child.stderr.on('data', (chunk) => {
        stderr += chunk;
        if (trace) process.stderr.write(chunk);
    });

    const ready = await waitForReady(child, readyTimeoutMs, () => stderr);
    return new Server({ child, ...ready, database, password, socketPath });
}

/**
 * Resolves with the parsed ready line, or rejects with something that explains
 * itself.
 *
 * A server that dies during startup is the common failure — a port already in
 * use, a corrupt image — and the reason is on stderr, so it is carried into the
 * rejection rather than left in a stream nobody is reading.
 */
function waitForReady(child, timeoutMs, stderr) {
    return new Promise((resolve, reject) => {
        let stdout = '';
        let settled = false;

        const finish = (fn, value) => {
            if (settled) return;
            settled = true;
            clearTimeout(timer);
            fn(value);
        };

        const timer = setTimeout(() => {
            child.kill('SIGKILL');
            finish(
                reject,
                new Error(
                    `mysql-ultralight: did not start within ${timeoutMs}ms.` + trailing(stderr())
                )
            );
        }, timeoutMs);
        // The timer must not hold the event loop open on its own.
        if (typeof timer.unref === 'function') timer.unref();

        child.stdout.setEncoding('utf8');
        child.stdout.on('data', (chunk) => {
            stdout += chunk;
            const line = stdout.split('\n').find((l) => l.startsWith('ready '));
            if (!line) return;

            const port = Number(/\bport=(\d+)/.exec(line)?.[1]);
            const socket = /\bsocket=(\S+)/.exec(line)?.[1];
            const startupMs = Number(/\bstartup_ms=([\d.]+)/.exec(line)?.[1]);
            if (!Number.isFinite(port)) {
                finish(reject, new Error(`mysql-ultralight: could not read the port from "${line}"`));
                return;
            }
            finish(resolve, {
                port,
                socketPath: socket && socket !== '-' ? socket : null,
                startupMs
            });
        });

        child.on('error', (e) => finish(reject, new Error(`mysql-ultralight: could not run the server: ${e.message}`)));
        child.on('exit', (code, signal) => {
            finish(
                reject,
                new Error(
                    `mysql-ultralight: exited ${signal ? `on ${signal}` : `with code ${code}`} before it was ready.` +
                        trailing(stderr())
                )
            );
        });
    });
}

function trailing(text) {
    const trimmed = (text || '').trim();
    return trimmed ? `\n${trimmed}` : '';
}

/** A socket path short enough for the kernel's ~100 byte limit. */
function temporarySocketPath() {
    const unique = `${process.pid.toString(36)}${Math.random().toString(36).slice(2, 8)}`;
    // Deliberately not os.tmpdir(): on macOS that is a long per-user path under
    // /var/folders, and a socket path over the limit fails with an OS error
    // that names neither the limit nor the path.
    return path.join('/tmp', `mu-${unique}.sock`);
}

/** A running server. */
class Server {
    constructor({ child, port, socketPath, startupMs, database, password }) {
        this.child = child;
        this.port = port;
        this.socketPath = socketPath;
        /** How long the server itself reported taking to start, in ms. */
        this.startupMs = startupMs;
        this.database = database;
        this.password = password;
        this.pid = child.pid;

        this.exited = new Promise((resolve) => child.once('exit', resolve));
        // Nothing should keep the parent alive just because a test server is
        // running; teardown stops it explicitly.
        child.unref();
    }

    /**
     * A connection URL for this server.
     *
     * The socket form is used when there is one, because a unix socket cannot
     * collide with another worker's port.
     */
    get url() {
        const credentials = this.password ? `root:${encodeURIComponent(this.password)}@` : 'root@';
        if (this.socketPath) {
            return `mysql://${credentials}localhost/${this.database}?socket=${encodeURIComponent(this.socketPath)}`;
        }
        return `mysql://${credentials}127.0.0.1:${this.port}/${this.database}`;
    }

    /**
     * Stops the server and waits for it to go.
     *
     * Safe to call twice, because a teardown hook that runs after a failed test
     * should not fail for a second reason.
     */
    async stop({ timeoutMs = 5_000 } = {}) {
        if (this.child.exitCode !== null || this.child.signalCode !== null) return;

        // Re-attached to the event loop for the duration: the child is
        // unref'd so a running server never holds the parent open, but that
        // also means nothing waits for its exit, and `await stop()` would
        // resolve only if something else happened to keep node alive.
        this.child.ref();
        this.child.kill('SIGTERM');
        const forced = setTimeout(() => this.child.kill('SIGKILL'), timeoutMs);
        if (typeof forced.unref === 'function') forced.unref();
        await this.exited;
        clearTimeout(forced);

        // The server is killed rather than asked to tidy up, so the socket
        // file outlives it. Removing it here keeps a rerun with the same path
        // from tripping over a stale one.
        if (this.socketPath) {
            try {
                fs.unlinkSync(this.socketPath);
            } catch {
                // Already gone, which is the outcome we wanted.
            }
        }
    }
}

module.exports = { start, Server, platformKey, PLATFORMS, resolveBinary };
