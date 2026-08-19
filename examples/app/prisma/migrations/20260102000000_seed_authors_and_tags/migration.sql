-- Seed data lives in a migration, the way the target application does it, so a
-- baked image carries it and a test process starts with something in the tables.
INSERT IGNORE INTO `Author` (`id`, `email`, `name`, `role`, `bio`, `updatedAt`) VALUES
    ('seed_author_ada', 'ada@example.com', 'Ada Lovelace', 'ADMIN', 'Wrote the first program.', NOW(3)),
    ('seed_author_grace', 'grace@example.com', 'Grace Hopper', 'EDITOR', NULL, NOW(3));

INSERT IGNORE INTO `Tag` (`name`) VALUES ('engineering'), ('history'), ('release-notes');

INSERT IGNORE INTO `Post` (`id`, `authorId`, `title`, `slug`, `body`, `status`, `views`, `price`, `metadata`, `updatedAt`) VALUES
    (
        '11111111-1111-4111-8111-111111111111',
        'seed_author_ada',
        'On the Analytical Engine',
        'on-the-analytical-engine',
        'A note on why a machine that manipulates symbols is not merely a calculator.',
        'PUBLISHED',
        120,
        9.99,
        '{"featured": true, "tags": ["classic"]}',
        NOW(3)
    ),
    (
        '22222222-2222-4222-8222-222222222222',
        'seed_author_grace',
        'Nanoseconds',
        'nanoseconds',
        NULL,
        'DRAFT',
        0,
        NULL,
        NULL,
        NOW(3)
    );

INSERT IGNORE INTO `_PostToTag` (`A`, `B`)
SELECT '11111111-1111-4111-8111-111111111111', `id` FROM `Tag` WHERE `name` IN ('engineering', 'history');
