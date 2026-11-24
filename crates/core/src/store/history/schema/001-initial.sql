-- aria.db, version 1 — epic 05 and decision 93.
--
-- Three artefacts, three files: the markdown pages are the memory's truth, the
-- index under %LOCALAPPDATA%/ARIA/index is a cache thrown away on a whim, and
-- this file is the application's own truth. Nothing here is derived, and
-- nothing here can be rebuilt by rescanning a folder.

-- An audio thread. Its title is derived from the first thing said, never
-- generated, and stays the empty string until somebody speaks.
CREATE TABLE conversations(
    id           TEXT PRIMARY KEY,
    started_at   TEXT NOT NULL,
    last_used_at TEXT NOT NULL,
    title        TEXT NOT NULL DEFAULT ''
);

-- Most recently used first is the only order the menu ever asks for.
CREATE INDEX conversations_by_use ON conversations(last_used_at DESC);

-- One entry of the thread, mirroring `agent::journal::Entry` column for
-- column. `task` is the delegation's identifier and is NULL for what was said
-- and what was answered.
CREATE TABLE turns(
    conversation TEXT    NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,
    seq          INTEGER NOT NULL,
    at           TEXT    NOT NULL,
    kind         TEXT    NOT NULL,
    text         TEXT    NOT NULL,
    task         TEXT,
    PRIMARY KEY (conversation, seq)
);

-- A session the main agent chose to keep, because it belongs to a project.
-- A throwaway session — the weather, the news — never lands here, which is why
-- there is nothing to clean up behind one.
--
-- `expired` is set after a resume has FAILED. It is never computed from
-- `last_used_at`: epic 05 forbids deducing resumability from a date, because
-- `cleanupPeriodDays` lives in the user's own settings and can be one day.
-- The row stays either way — what is lost is the ability to continue, not the
-- memory of what was done.
CREATE TABLE project_sessions(
    conversation TEXT    NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,
    id           TEXT    NOT NULL,
    native       TEXT,
    project      TEXT    NOT NULL DEFAULT '',
    title        TEXT    NOT NULL DEFAULT '',
    last_used_at TEXT    NOT NULL,
    expired      INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (conversation, id)
);

-- Each session carries its own thirty days, independent of its conversation's.
CREATE INDEX project_sessions_by_use ON project_sessions(last_used_at);

-- Who allowed what, and through which channel. Decision 6 removes the
-- confirmation for a recognised pattern, not the trace: an auto-approval is
-- recorded as `registry` so it is legible as such rather than passing for a
-- human yes.
--
-- No foreign key, on purpose: a trace outlives the conversation it came from,
-- and retention never touches this table. An audit journal that deletes itself
-- after a month is not an audit journal.
CREATE TABLE audit(
    at       TEXT NOT NULL,
    session  TEXT,
    tool     TEXT NOT NULL,
    command  TEXT,
    risk     TEXT NOT NULL,
    decision TEXT NOT NULL,
    scope    TEXT,
    channel  TEXT NOT NULL
);

CREATE INDEX audit_by_time ON audit(at DESC);
