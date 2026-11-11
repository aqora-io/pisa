-- How many documents a worker has open on Lean Beam, so that a document no
-- worker holds opens where fewest are open per Beam slot.
ALTER TABLE workers ADD COLUMN documents_open INTEGER NOT NULL DEFAULT 0;
