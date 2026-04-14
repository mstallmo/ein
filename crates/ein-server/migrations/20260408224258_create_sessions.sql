CREATE TABLE sessions (
    id                  TEXT    PRIMARY KEY,
    created_at          INTEGER NOT NULL,
    session_config_json TEXT    NOT NULL,
    messages_json       TEXT    NOT NULL
);
