-- 初期スキーマ: users / streams / webhooks / stream_events / sessions

CREATE TABLE users (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    username      TEXT NOT NULL UNIQUE,
    password_hash TEXT NOT NULL,
    role          TEXT NOT NULL CHECK (role IN ('admin', 'broadcaster')),
    disabled      BOOLEAN NOT NULL DEFAULT 0,
    created_at    TEXT NOT NULL
);

CREATE TABLE streams (
    id            TEXT PRIMARY KEY,           -- URLセーフなスラッグ
    owner_user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    title         TEXT NOT NULL,
    description   TEXT NOT NULL DEFAULT '',
    stream_key    TEXT NOT NULL UNIQUE,       -- WHIP用のシークレット(Bearer token)
    is_live       BOOLEAN NOT NULL DEFAULT 0,
    public        BOOLEAN NOT NULL DEFAULT 0, -- true:未ログインでも視聴可
    started_at    TEXT,
    peak_viewers  INTEGER NOT NULL DEFAULT 0,
    created_at    TEXT NOT NULL
);

CREATE INDEX idx_streams_owner ON streams(owner_user_id);

CREATE TABLE webhooks (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    name          TEXT NOT NULL,
    url           TEXT NOT NULL,
    notify_start  BOOLEAN NOT NULL DEFAULT 1,
    notify_stop   BOOLEAN NOT NULL DEFAULT 1,
    enabled       BOOLEAN NOT NULL DEFAULT 1,
    created_at    TEXT NOT NULL
);

CREATE TABLE stream_events (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    stream_id     TEXT NOT NULL REFERENCES streams(id) ON DELETE CASCADE,
    kind          TEXT NOT NULL CHECK (kind IN ('start', 'stop')),
    at            TEXT NOT NULL,
    peak_viewers  INTEGER NOT NULL DEFAULT 0
);

CREATE INDEX idx_stream_events_stream ON stream_events(stream_id);

-- tower-sessions 用の自前セッションストア(SqliteSessionStore)が使うテーブル
CREATE TABLE sessions (
    id          TEXT PRIMARY KEY,
    data        BLOB NOT NULL,
    expiry_date TEXT NOT NULL
);

CREATE INDEX idx_sessions_expiry ON sessions(expiry_date);
