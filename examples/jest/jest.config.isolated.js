// The isolated model: one database per test file.
//
// `setupFilesAfterEnv` runs once per test file inside its worker, so the
// `beforeAll` it registers is per file rather than per run. There is no
// globalSetup here — nothing needs to be shared.

const path = require('path');

/** @type {import('ts-jest').JestConfigWithTsJest} */
module.exports = {
    preset: 'ts-jest',
    testEnvironment: 'node',
    roots: ['<rootDir>/test/isolated'],
    setupFilesAfterEnv: [path.resolve(__dirname, './test/support/per-file.ts')]
};
