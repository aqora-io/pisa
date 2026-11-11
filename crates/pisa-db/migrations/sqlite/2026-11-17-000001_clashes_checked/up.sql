-- When the version's modules verified before creates refused a header whose
-- imports declare one name twice were checked for it, once; null until then.
ALTER TABLE catalogs ADD COLUMN clashes_checked_micros BIGINT;
