CREATE TABLE projects (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    created_at TEXT NOT NULL
);

CREATE TABLE assets (
    id TEXT PRIMARY KEY,
    project_id TEXT NOT NULL,
    fingerprint TEXT NOT NULL,
    path TEXT NOT NULL,
    filename TEXT NOT NULL,
    size_bytes INTEGER NOT NULL,
    duration_seconds REAL NOT NULL,
    frame_rate REAL NOT NULL DEFAULT 30,
    width INTEGER NOT NULL DEFAULT 0,
    height INTEGER NOT NULL DEFAULT 0,
    codec TEXT NOT NULL DEFAULT '',
    camera TEXT NOT NULL DEFAULT 'Unknown camera',
    captured_at_raw TEXT,
    best_time TEXT,
    offset_seconds INTEGER NOT NULL DEFAULT 0,
    proxy_path TEXT,
    notes TEXT NOT NULL DEFAULT '',
    status TEXT NOT NULL DEFAULT 'unreviewed',
    included INTEGER NOT NULL DEFAULT 0,
    source_in REAL NOT NULL DEFAULT 0,
    source_out REAL NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL,
    UNIQUE(project_id, fingerprint)
);

CREATE TABLE clock_events (
    id TEXT PRIMARY KEY,
    project_id TEXT NOT NULL,
    camera TEXT NOT NULL,
    start_filename TEXT NOT NULL,
    offset_seconds INTEGER NOT NULL,
    label TEXT NOT NULL,
    created_at TEXT NOT NULL
);

CREATE TABLE asset_thumbnails (
    asset_id TEXT NOT NULL,
    quality INTEGER NOT NULL,
    path TEXT NOT NULL,
    created_at TEXT NOT NULL,
    PRIMARY KEY(asset_id, quality)
);

INSERT INTO projects (id, name, created_at)
VALUES ('project_old', 'Old project', '2026-07-18T00:00:00.000Z');

INSERT INTO assets (
    id, project_id, fingerprint, path, filename, size_bytes,
    duration_seconds, frame_rate, width, height, codec, camera,
    captured_at_raw, best_time, offset_seconds, proxy_path, notes,
    source_out, created_at
) VALUES (
    'asset_portrait', 'project_old', 'fingerprint_old', '/missing/PORTRAIT.MP4',
    'PORTRAIT.MP4', 42, 10, 30, 1080, 1920, 'h264', 'Camera A',
    '2026-07-18T00:00:00Z', '2026-07-18T00:00:00.000Z', 60,
    '/legacy/cache/asset_portrait.v2.mp4', 'keep this note', 10,
    '2026-07-18T00:00:00.000Z'
);

INSERT INTO clock_events (
    id, project_id, camera, start_filename, offset_seconds, label, created_at
) VALUES (
    'clock_old', 'project_old', 'Camera A', 'PORTRAIT.MP4', 60,
    'Existing correction', '2026-07-18T00:00:01.000Z'
);

INSERT INTO asset_thumbnails (asset_id, quality, path, created_at)
VALUES (
    'asset_portrait', 1, '/legacy/cache/asset_portrait/q1.v2.jpg',
    '2026-07-18T00:00:02.000Z'
);
