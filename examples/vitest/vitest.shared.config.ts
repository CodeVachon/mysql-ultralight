import { defineConfig } from 'vitest/config';

// The drop-in model: one database for the whole run, brought up in
// globalSetup, which also returns the teardown.
export default defineConfig({
    test: {
        include: ['test/shared/**/*.test.ts'],
        globalSetup: ['./test/support/global-setup.ts']
    }
});
