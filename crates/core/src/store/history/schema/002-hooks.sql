-- What the user validated of each hook (`hooks`).
--
-- Here and not in the hook's folder: a session that can rewrite the script
-- must not be able to rewrite its validation too. Losing this table suspends
-- every hook until it is validated again, which is the safe way to lose it.
CREATE TABLE hook_approvals (
    id          TEXT PRIMARY KEY,
    fingerprint TEXT NOT NULL,
    enabled     INTEGER NOT NULL
);
