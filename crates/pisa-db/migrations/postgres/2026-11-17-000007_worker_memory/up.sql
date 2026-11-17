-- What a worker says of its memory: the limits it gives its Lean processes
-- summed over its slots in MiB (null when it runs any without a limit), the
-- memory of the machine or container it runs in (0 when it can't tell), and
-- the boot of that machine, which tells workers on one host from those on
-- others (empty when it can't tell).
ALTER TABLE workers ADD COLUMN memory_caps_mb BIGINT;
ALTER TABLE workers ADD COLUMN host_memory_mb BIGINT NOT NULL DEFAULT 0;
ALTER TABLE workers ADD COLUMN boot_id TEXT NOT NULL DEFAULT '';
