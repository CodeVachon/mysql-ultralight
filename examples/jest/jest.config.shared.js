// The drop-in model: one database for the whole run, brought up in globalSetup.
// Closest to what a suite backed by a real MySQL already looks like.

const path = require('path');

/** @type {import('ts-jest').JestConfigWithTsJest} */
module.exports = {
    preset: 'ts-jest',
    testEnvironment: 'node',
    roots: ['<rootDir>/test/shared'],
    globalSetup: path.resolve(__dirname, './test/support/global-setup.ts'),
    globalTeardown: path.resolve(__dirname, './test/support/global-teardown.ts')
};
