-- A data migration: the new column is filled from the old one rather than left
-- NULL, which is what makes the previous migration's ALTER meaningful.
UPDATE `Post`
SET `excerpt` = CONCAT(LEFT(`body`, 60), '...')
WHERE `excerpt` IS NULL
  AND `body` IS NOT NULL
  AND CHAR_LENGTH(`body`) > 60;

UPDATE `Post`
SET `excerpt` = `body`
WHERE `excerpt` IS NULL
  AND `body` IS NOT NULL;
