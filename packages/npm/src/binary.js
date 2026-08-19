'use strict';

const fs = require('node:fs');
const path = require('node:path');

/**
 * The platform packages, keyed the way Node names the platform.
 *
 * Windows and macOS x64 are deliberately absent rather than missing: they are
 * out of scope for v1, and a clear "no build for this platform" beats a
 * download that fails halfway through an install.
 */
const PLATFORMS = {
    'darwin-arm64': '@codevachon/mysql-ultralight-darwin-arm64',
    'linux-x64-gnu': '@codevachon/mysql-ultralight-linux-x64-gnu',
    'linux-x64-musl': '@codevachon/mysql-ultralight-linux-x64-musl',
    'linux-arm64-gnu': '@codevachon/mysql-ultralight-linux-arm64-gnu'
};

/**
 * Whether this Linux is glibc or musl.
 *
 * A musl system running the glibc build fails at exec time with a message
 * about a missing loader, which names neither the cause nor the fix, so the
 * distinction is made here where it can be explained.
 */
function libc() {
    if (process.platform !== 'linux') return null;
    const report = typeof process.report?.getReport === 'function' ? process.report.getReport() : null;
    return report?.header?.glibcVersionRuntime ? 'gnu' : 'musl';
}

/** The platform key for this machine, e.g. `linux-x64-musl`. */
function platformKey() {
    const base = `${process.platform}-${process.arch}`;
    const flavour = libc();
    return flavour ? `${base}-${flavour}` : base;
}

/**
 * Where the binary is, or an error explaining why it is not.
 *
 * Three places are tried, in the order that makes a contributor's life easiest:
 * an explicit override, the platform package npm installed, and a local cargo
 * build — so working on this repo does not require publishing anything.
 */
function resolveBinary(override) {
    const name = process.platform === 'win32' ? 'mysql-ultralight.exe' : 'mysql-ultralight';

    if (override) {
        if (!fs.existsSync(override)) {
            throw new Error(`mysql-ultralight: no binary at ${override}`);
        }
        return override;
    }

    if (process.env.MYSQL_ULTRALIGHT_BINARY) {
        const fromEnv = process.env.MYSQL_ULTRALIGHT_BINARY;
        if (!fs.existsSync(fromEnv)) {
            throw new Error(`mysql-ultralight: MYSQL_ULTRALIGHT_BINARY points at ${fromEnv}, which does not exist`);
        }
        return fromEnv;
    }

    const key = platformKey();
    const pkg = PLATFORMS[key];
    if (pkg) {
        try {
            return require.resolve(`${pkg}/${name}`);
        } catch {
            // Falls through to the local build: an optional dependency that
            // did not install is not fatal on a machine that has cargo.
        }
    }

    // A checkout of this repository, built with cargo.
    for (const profile of ['release', 'debug']) {
        const local = path.join(__dirname, '..', '..', '..', 'target', profile, name);
        if (fs.existsSync(local)) return local;
    }

    if (!pkg) {
        throw new Error(
            `mysql-ultralight: no prebuilt binary for ${key}. ` +
                `Supported: ${Object.keys(PLATFORMS).join(', ')}. ` +
                'Build from source with `cargo build --release` and set MYSQL_ULTRALIGHT_BINARY.'
        );
    }

    throw new Error(
        `mysql-ultralight: the binary for ${key} is missing. ` +
            `It ships in ${pkg}, which npm installs as an optional dependency — ` +
            'an install run with --no-optional or --omit=optional will not have it. ' +
            'Reinstall without that flag, or set MYSQL_ULTRALIGHT_BINARY to a binary you built.'
    );
}

module.exports = { PLATFORMS, platformKey, resolveBinary };
