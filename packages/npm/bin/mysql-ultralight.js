#!/usr/bin/env node
'use strict';

// A shim, not a wrapper: every argument and both streams are passed straight
// through, so `npx mysql-ultralight --help` is the binary's own help and no
// flag has to be taught to two places.

const { spawn } = require('node:child_process');
const { resolveBinary } = require('../src/binary');

let binary;
try {
    binary = resolveBinary();
} catch (e) {
    console.error(e.message);
    process.exit(1);
}

const child = spawn(binary, process.argv.slice(2), { stdio: 'inherit' });

// Forwarded rather than left to the shell: under `npm run` this process is the
// one the terminal signals, and the server would otherwise be orphaned.
for (const signal of ['SIGINT', 'SIGTERM', 'SIGHUP']) {
    process.on(signal, () => child.kill(signal));
}

child.on('exit', (code, signal) => {
    if (signal) {
        // Reported the way a shell reports a signalled child, so a CI step
        // that inspects the code sees what it would from the binary itself.
        process.exit(128 + (require('node:os').constants.signals[signal] ?? 0));
    }
    process.exit(code ?? 0);
});

child.on('error', (e) => {
    console.error(`mysql-ultralight: could not run ${binary}: ${e.message}`);
    process.exit(1);
});
