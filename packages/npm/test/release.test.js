'use strict';

const assert = require('node:assert/strict');
const { execFileSync } = require('node:child_process');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { describe, it } = require('node:test');

const packageRoot = path.join(__dirname, '..');
const script = path.join(packageRoot, 'scripts', 'prepare-release.js');

/**
 * A throwaway copy of the package tree.
 *
 * The script rewrites manifests in place, and a test that did that to the real
 * ones would leave the working tree dirty on the first failure.
 */
function copyOfPackage(t) {
    const to = fs.mkdtempSync(path.join(os.tmpdir(), 'mu-release-'));
    t.after(() => fs.rmSync(to, { recursive: true, force: true }));

    fs.mkdirSync(path.join(to, 'scripts'), { recursive: true });
    fs.copyFileSync(script, path.join(to, 'scripts', 'prepare-release.js'));
    fs.copyFileSync(path.join(packageRoot, 'package.json'), path.join(to, 'package.json'));
    fs.cpSync(path.join(packageRoot, 'platform'), path.join(to, 'platform'), { recursive: true });
    return to;
}

function run(cwd, args) {
    return execFileSync(process.execPath, [path.join(cwd, 'scripts', 'prepare-release.js'), ...args], {
        cwd,
        encoding: 'utf8'
    });
}

function manifests(root) {
    const main = JSON.parse(fs.readFileSync(path.join(root, 'package.json'), 'utf8'));
    const platforms = fs
        .readdirSync(path.join(root, 'platform'))
        .map((key) => JSON.parse(fs.readFileSync(path.join(root, 'platform', key, 'package.json'), 'utf8')));
    return { main, platforms };
}

describe('prepare-release', () => {
    it('sets one version across every package', (t) => {
        const root = copyOfPackage(t);
        run(root, ['1.2.3']);

        const { main, platforms } = manifests(root);
        assert.equal(main.version, '1.2.3');
        for (const platform of platforms) {
            assert.equal(platform.version, '1.2.3', platform.name);
            // Pinned exactly: a range would let npm pair the wrapper with a
            // binary built from different sources.
            assert.equal(main.optionalDependencies[platform.name], '1.2.3');
        }
    });

    it('takes a tag as readily as a version', (t) => {
        const root = copyOfPackage(t);
        run(root, ['v9.8.7']);
        assert.equal(manifests(root).main.version, '9.8.7');
    });

    it('refuses something that is not a version', (t) => {
        const root = copyOfPackage(t);
        // Whatever the version is now — the point is that a mistyped tag
        // cannot half-release, not what the current number happens to be.
        const before = manifests(root).main.version;

        assert.throws(() => run(root, ['latest']), /not a semver version/);
        assert.equal(manifests(root).main.version, before);
    });

    it('stages a binary into each platform package', (t) => {
        const root = copyOfPackage(t);
        const binaries = fs.mkdtempSync(path.join(os.tmpdir(), 'mu-binaries-'));
        t.after(() => fs.rmSync(binaries, { recursive: true, force: true }));

        const { platforms } = manifests(root);
        for (const platform of platforms) {
            const directory = path.join(binaries, platform.rustTarget);
            fs.mkdirSync(directory, { recursive: true });
            fs.writeFileSync(path.join(directory, 'mysql-ultralight'), 'pretend binary');
        }

        run(root, ['2.0.0', '--binaries', binaries]);

        for (const key of fs.readdirSync(path.join(root, 'platform'))) {
            const staged = path.join(root, 'platform', key, 'mysql-ultralight');
            assert.ok(fs.existsSync(staged), `${key} has no binary`);
            // Published without the executable bit, the package installs a file
            // nothing can run.
            assert.ok(fs.statSync(staged).mode & 0o111, `${key} is not executable`);
        }
    });

    it('stops when a binary is missing rather than publishing an empty package', (t) => {
        const root = copyOfPackage(t);
        const binaries = fs.mkdtempSync(path.join(os.tmpdir(), 'mu-binaries-'));
        t.after(() => fs.rmSync(binaries, { recursive: true, force: true }));

        assert.throws(() => run(root, ['2.0.0', '--binaries', binaries]), /no binary for/);
    });

    it('notices a platform directory the wrapper does not depend on', (t) => {
        const root = copyOfPackage(t);
        // A new target added to the tree but not to optionalDependencies would
        // publish a binary npm never installs.
        const added = path.join(root, 'platform', 'linux-riscv64-gnu');
        fs.mkdirSync(added);
        fs.writeFileSync(
            path.join(added, 'package.json'),
            JSON.stringify({
                name: '@codevachon/mysql-ultralight-linux-riscv64-gnu',
                version: '0.0.1',
                rustTarget: 'riscv64gc-unknown-linux-gnu'
            })
        );

        assert.throws(() => run(root, ['1.0.0']), /optionalDependencies do not match/);
    });
});
