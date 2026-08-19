-- AlterTable
-- A column added and another widened: the shape a schema change usually takes,
-- and one that has to survive being replayed onto a table that already has rows.
ALTER TABLE `Post`
    ADD COLUMN `excerpt` VARCHAR(280) NULL,
    MODIFY COLUMN `body` LONGTEXT NULL;
