-- Storage services, such as a Nextcloud server, that large files go from by link. Their app
-- passwords live in the credential store, under account 0.
CREATE TABLE IF NOT EXISTS link_services (
    id INTEGER PRIMARY KEY,
    server_url TEXT NOT NULL,
    login_name TEXT NOT NULL,
    display_name TEXT NOT NULL,
    created_at INTEGER NOT NULL
);
