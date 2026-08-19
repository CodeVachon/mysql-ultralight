// The kind of code a test suite actually exercises: application logic over
// Prisma, rather than Prisma itself.
//
// Every function takes its client rather than reaching for a module-level one.
// That is what lets a test hand it a database of its own, which is the whole
// point of the engine underneath.

import type { Prisma, PrismaClient, Role } from '@prisma/client';

export interface NewAuthor {
    email: string;
    name: string;
    role?: Role;
    website?: string;
}

/**
 * Registers an author, and their profile in the same breath.
 *
 * One nested write rather than two calls, so the profile cannot be orphaned by
 * a failure between them.
 */
export async function register(db: PrismaClient, author: NewAuthor) {
    return db.author.create({
        data: {
            email: author.email.trim().toLowerCase(),
            name: author.name.trim(),
            role: author.role ?? 'VIEWER',
            profile: author.website ? { create: { website: author.website } } : undefined
        },
        include: { profile: true }
    });
}

/** An author by email, or null. Email is stored lowercased, so lookup matches. */
export function byEmail(db: PrismaClient, email: string) {
    return db.author.findUnique({
        where: { email: email.trim().toLowerCase() },
        include: { profile: true }
    });
}

/**
 * Authors with a count of what they have published.
 *
 * Ordered by name so the result is stable — a test that asserts on order
 * should not depend on insertion order.
 */
export async function withPostCounts(db: PrismaClient) {
    const authors = await db.author.findMany({
        orderBy: { name: 'asc' },
        include: { _count: { select: { posts: true } } }
    });
    return authors.map((author) => ({
        id: author.id,
        name: author.name,
        posts: author._count.posts
    }));
}

/** Promotes an author, refusing a role that would remove the last admin. */
export async function changeRole(db: PrismaClient, email: string, role: Role) {
    const author = await byEmail(db, email);
    if (!author) {
        throw new Error(`no author with the email ${email}`);
    }

    if (author.role === 'ADMIN' && role !== 'ADMIN') {
        const admins = await db.author.count({ where: { role: 'ADMIN' } });
        if (admins === 1) {
            throw new Error('cannot demote the last admin');
        }
    }

    return db.author.update({ where: { id: author.id }, data: { role } });
}

/**
 * Removes an author and everything that hangs off them.
 *
 * The posts and profile go by the schema's cascade rather than by hand, which
 * is worth a test: a cascade that silently does not fire leaves orphans.
 */
export async function remove(db: PrismaClient, email: string): Promise<number> {
    const author = await byEmail(db, email);
    if (!author) return 0;

    await db.author.delete({ where: { id: author.id } });
    return 1;
}

/** Everything an author touches, moved to someone else in one transaction. */
export async function transferPosts(db: PrismaClient, fromEmail: string, toEmail: string) {
    return db.$transaction(async (tx) => {
        const from = await tx.author.findUniqueOrThrow({ where: { email: fromEmail } });
        const to = await tx.author.findUniqueOrThrow({ where: { email: toEmail } });

        const moved: Prisma.BatchPayload = await tx.post.updateMany({
            where: { authorId: from.id },
            data: { authorId: to.id }
        });
        return moved.count;
    });
}
