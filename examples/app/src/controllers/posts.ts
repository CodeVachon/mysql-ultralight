import type { PrismaClient, Status } from '@prisma/client';

export interface Draft {
    authorEmail: string;
    title: string;
    body?: string;
    price?: number;
    tags?: string[];
}

/** A slug from a title: lowercase, words joined by hyphens. */
export function slugify(title: string): string {
    return title
        .toLowerCase()
        .replace(/[^a-z0-9]+/g, '-')
        .replace(/^-+|-+$/g, '');
}

/**
 * Writes a draft, attaching tags by name and creating any that are new.
 *
 * The slug is unique per author rather than globally, which is the sort of
 * constraint that only shows up as a bug when two authors pick the same title.
 */
export async function draft(db: PrismaClient, post: Draft) {
    const author = await db.author.findUniqueOrThrow({
        where: { email: post.authorEmail.trim().toLowerCase() }
    });

    return db.post.create({
        data: {
            authorId: author.id,
            title: post.title,
            slug: slugify(post.title),
            body: post.body,
            price: post.price,
            tags: {
                connectOrCreate: (post.tags ?? []).map((name) => ({
                    where: { name },
                    create: { name }
                }))
            }
        },
        include: { tags: true }
    });
}

/**
 * Publishes a draft, filling the excerpt if it has none.
 *
 * Refuses anything already published, so publishing twice cannot quietly reset
 * the excerpt of a live post.
 */
export async function publish(db: PrismaClient, id: string) {
    const post = await db.post.findUniqueOrThrow({ where: { id } });
    if (post.status === 'PUBLISHED') {
        throw new Error(`post ${id} is already published`);
    }

    return db.post.update({
        where: { id },
        data: {
            status: 'PUBLISHED',
            excerpt: post.excerpt ?? excerptOf(post.body)
        }
    });
}

/** The first 60 characters of a body, marked as truncated when it is. */
export function excerptOf(body: string | null): string | null {
    if (!body) return null;
    return body.length > 60 ? `${body.slice(0, 60)}...` : body;
}

/** Posts in a state, newest first, with their author and tags. */
export function inState(db: PrismaClient, status: Status) {
    return db.post.findMany({
        where: { status },
        orderBy: { createdAt: 'desc' },
        include: { author: true, tags: true }
    });
}

/** How many posts each tag carries, busiest first. */
export async function tagCounts(db: PrismaClient) {
    const tags = await db.tag.findMany({
        include: { _count: { select: { posts: true } } }
    });
    return tags
        .map((tag) => ({ name: tag.name, posts: tag._count.posts }))
        .sort((a, b) => b.posts - a.posts || a.name.localeCompare(b.name));
}

/**
 * Records a view, as an atomic increment rather than a read and a write.
 *
 * Two concurrent calls must not lose one of the views, which they would if the
 * count were read into the process and written back.
 */
export function recordView(db: PrismaClient, id: string) {
    return db.post.update({ where: { id }, data: { views: { increment: 1 } } });
}
