#!/usr/bin/env node
'use strict';

// Sets every package's version and stages each platform binary, so the release
// workflow is a few obvious steps rather than a pile of `sed`.
//
//   node scripts/prepare-release.js 1.2.3 [--binaries <dir>]
//
// `--binaries <dir>` expects one subdirectory per Rust target triple, each
// holding a `mysql-ultralight` binary — the shape `actions/download-artifact`
// produces when the build matrix uploads by target.

const fs = require('node:fs');
const path = require('node:path');

const root = path.join(__dirname, '..');
const platformRoot = path.join(root, 'platform');

function fail(message) {
    console.error(`prepare-release: ${message}`);
    process.exit(1);
}

function parseArgs(argv) {
    const [version, ...rest] = argv;
    if (!version) fail('usage: prepare-release.js <version> [--binaries <dir>]');
    // A tag is `v1.2.3`; the version inside a package.json is not.
    const cleaned = version.replace(/^v/, '');
    if (!/^\d+\.\d+\.\d+(-[0-9A-Za-z.-]+)?$/.test(cleaned)) {
        fail(`"${version}" is not a semver version`);
    }

    let binaries = null;
    for (let i = 0; i < rest.length; i += 1) {
        if (rest[i] === '--binaries') {
            binaries = rest[i + 1];
            if (!binaries) fail('--binaries needs a directory');
            i += 1;
        } else {
            fail(`unknown argument ${rest[i]}`);
        }
    }
    return { version: cleaned, binaries };
}

function readJson(file) {
    return JSON.parse(fs.readFileSync(file, 'utf8'));
}

function writeJson(file, value) {
    fs.writeFileSync(file, `${JSON.stringify(value, null, 4)}\n`);
}

/** Every platform package, with the Rust target it is built from. */
function platforms() {
    return fs
        .readdirSync(platformRoot, { withFileTypes: true })
        .filter((entry) => entry.isDirectory())
        .map((entry) => {
            const manifest = path.join(platformRoot, entry.name, 'package.json');
            const pkg = readJson(manifest);
            if (!pkg.rustTarget) fail(`${entry.name} has no rustTarget`);
            return { key: entry.name, manifest, pkg };
        });
}

function main() {
    const { version, binaries } = parseArgs(process.argv.slice(2));
    const found = platforms();

    for (const { key, manifest, pkg } of found) {
        pkg.version = version;
        writeJson(manifest, pkg);

        if (!binaries) continue;

        // The binary is not in git — the workflow drops it in here — so a
        // missing one has to stop the release rather than publish a package
        // whose only content is a manifest.
        const from = path.join(binaries, pkg.rustTarget, 'mysql-ultralight');
        if (!fs.existsSync(from)) fail(`no binary for ${key} at ${from}`);

        const to = path.join(platformRoot, key, 'mysql-ultralight');
        fs.copyFileSync(from, to);
        fs.chmodSync(to, 0o755);
        console.log(`${key}: ${(fs.statSync(to).size / 1024 / 1024).toFixed(1)} MB`);
    }

    // The main package pins the platform packages exactly. A range would let
    // npm resolve a binary built from different sources than the wrapper.
    const mainManifest = path.join(root, 'package.json');
    const main = readJson(mainManifest);
    main.version = version;

    const expected = Object.fromEntries(found.map(({ pkg }) => [pkg.name, version]));
    const declared = Object.keys(main.optionalDependencies ?? {}).sort();
    const actual = Object.keys(expected).sort();
    if (declared.join() !== actual.join()) {
        fail(
            `optionalDependencies do not match the platform directories.\n` +
                `  declared: ${declared.join(', ')}\n` +
                `  found:    ${actual.join(', ')}`
        );
    }
    main.optionalDependencies = expected;
    writeJson(mainManifest, main);

    console.log(`version ${version} across ${found.length + 1} packages`);
}

main();
