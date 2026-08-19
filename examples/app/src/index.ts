// The app, as a test suite sees it: the controllers under test, the types they
// speak, and the helpers that give a test file a database of its own.

export * as authors from './controllers/authors';
export * as posts from './controllers/posts';

export {
    bake,
    connect,
    imagePath,
    startDatabase,
    startEmpty,
    type Database
} from './testing';

// Re-exported so a runner example needs no direct dependency on Prisma: it is
// this package's schema, so it is this package's job to hand out its types.
export type { Author, Post, Prisma, PrismaClient, Profile, Role, Status, Tag } from '@prisma/client';
