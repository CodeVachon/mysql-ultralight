export default async function globalTeardown(): Promise<void> {
    await globalThis.__DATABASE__?.stop();
    globalThis.__DATABASE__ = undefined;
}
