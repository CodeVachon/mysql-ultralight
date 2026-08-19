import { defineConfig } from 'vitest/config';

// The isolated model: one database per test file.
//
// `setupFiles` runs once per test file, so the hooks it registers are per file.
// Nothing is shared, so there is no globalSetup at all.
export default defineConfig({
    test: {
        include: ['test/isolated/**/*.test.ts'],
        setupFiles: ['./test/support/per-file.ts']
    }
});
