//! Per-calendar pull. Order per cycle: push dirty rows first (local intent
//! wins), then for each enabled collection a ctag gate, an incremental
//! sync-collection REPORT (RFC 6578) when we hold a token, a calendar-query
//! fallback otherwise, and calendar-multiget for changed resources.

use crate::calendar::parse_ics;
use crate::db::Db;
use crate::db::repo;
use crate::error::{CoreError, Result};
use crate::events::{EventBus, NewEventInfo};
use crate::models::now_ms;

use super::route::PendingPush;
use super::{Transport, err, push, xml};

const MULTIGET_BATCH: usize = 50;
pub(crate) const MAX_NEW_EVENT_NOTIFICATIONS: usize = 1_000;

/// Normalize an href to path form so stored values compare stably no matter
/// whether the server answered with absolute URLs or paths.
fn href_path(href: &str) -> String {
    match url::Url::parse(href) {
        Ok(u) => u.path().to_string(),
        Err(_) => href.to_string(),
    }
}

/// What one connection's cycle did.
#[derive(Debug, Default)]
pub struct Synced {
    pub changed: bool,
    pub new_events: Vec<NewEventInfo>,
    /// The first calendar that could not sync, named; the connection's other
    /// calendars still did.
    pub calendar_error: Option<String>,
}

impl Synced {
    pub(crate) fn add_new_events(&mut self, events: Vec<NewEventInfo>) {
        let remaining = MAX_NEW_EVENT_NOTIFICATIONS.saturating_sub(self.new_events.len());
        self.new_events.extend(events.into_iter().take(remaining));
    }
}

/// One CalDAV connection's cycle: its routed local changes first (local
/// intent wins, and their PUTs establish etags the pull then agrees with),
/// then every enabled calendar of the connection.
pub async fn sync_connection(
    db: &Db,
    bus: &EventBus,
    t: &dyn Transport,
    account_id: i64,
    connection_id: i64,
    self_email: &str,
    pushes: &[PendingPush],
) -> Result<Synced> {
    let pushed = push::push_routed(db, bus, t, account_id, self_email, pushes).await?;

    let calendars = db
        .read(move |conn| repo::caldav::connection_calendars(conn, connection_id))
        .await?;
    let mut synced = Synced {
        changed: pushed,
        ..Synced::default()
    };
    for cal in calendars.into_iter().filter(|c| c.enabled) {
        match sync_calendar(db, t, account_id, cal.id, &cal.url).await {
            Ok((changed, fresh)) => {
                synced.changed |= changed;
                synced.add_new_events(fresh);
            }
            Err(error @ (CoreError::NeedsReauth | CoreError::Offline)) => return Err(error),
            Err(e) => {
                tracing::warn!("calendar {} sync failed: {e}", cal.id);
                let name = cal.display_name.unwrap_or(cal.url);
                synced.calendar_error.get_or_insert(format!("{name}: {e}"));
            }
        }
    }
    Ok(synced)
}

async fn sync_calendar(
    db: &Db,
    t: &dyn Transport,
    account_id: i64,
    calendar_id: i64,
    calendar_url: &str,
) -> Result<(bool, Vec<NewEventInfo>)> {
    let (stored_ctag, stored_token) = db
        .read(move |conn| repo::caldav::sync_state(conn, calendar_id))
        .await?;

    // Cheap gate: unchanged ctag = nothing to do.
    let resp = t
        .request(
            "PROPFIND",
            calendar_url,
            Some("0"),
            &[],
            Some(xml::propfind_ctag()),
        )
        .await?;
    let (ctag, current_token) = if resp.status == 207 {
        let ms = xml::parse_multistatus(&resp.body)?;
        let item = ms.items.first();
        (
            item.and_then(|i| i.ctag.clone()),
            item.and_then(|i| i.sync_token.clone()),
        )
    } else {
        (None, None)
    };
    if ctag.is_some() && ctag == stored_ctag {
        return Ok((false, Vec::new()));
    }

    // Changed hrefs via incremental sync, else the time-range fallback.
    let mut removed: Vec<String> = Vec::new();
    let mut changed: Vec<(String, Option<String>)> = Vec::new(); // (href, etag)
    let mut new_token: Option<String> = None;

    let mut incremental_ok = false;
    if let Some(token) = &stored_token {
        let resp = t
            .request(
                "REPORT",
                calendar_url,
                Some("1"),
                &[],
                Some(xml::report_sync_collection(token)),
            )
            .await?;
        if resp.status == 207 && !resp.body.contains("valid-sync-token") {
            let ms = xml::parse_multistatus(&resp.body)?;
            for item in &ms.items {
                let path = href_path(&item.href);
                if path.is_empty() || path == href_path(calendar_url) {
                    continue;
                }
                if item.status == 404 {
                    removed.push(path);
                } else {
                    changed.push((path, item.etag.clone()));
                }
            }
            new_token = ms.sync_token.or_else(|| current_token.clone());
            incremental_ok = true;
        }
        // 403/409/507 or valid-sync-token error: fall through to full query.
    }

    if !incremental_ok {
        let resp = t
            .request(
                "REPORT",
                calendar_url,
                Some("1"),
                &[],
                Some(xml::report_calendar_query()),
            )
            .await?;
        if resp.status != 207 {
            return Err(err(format!("calendar-query: HTTP {}", resp.status)));
        }
        let ms = xml::parse_multistatus(&resp.body)?;
        let server: Vec<(String, Option<String>)> = ms
            .items
            .iter()
            .filter(|i| i.status == 200 && !i.href.is_empty())
            .map(|i| (href_path(&i.href), i.etag.clone()))
            .collect();

        // Local rows in the window that vanished server-side are removals.
        let local = db
            .read(move |conn| repo::calendar::hrefs_for_calendar(conn, calendar_id))
            .await?;
        let server_set: std::collections::HashSet<&str> =
            server.iter().map(|(h, _)| h.as_str()).collect();
        for (_, href, _) in &local {
            if !server_set.contains(href.as_str()) {
                removed.push(href.clone());
            }
        }
        changed = server;
        new_token = current_token.clone();
    }

    // Drop unchanged etags before fetching bodies.
    {
        let local = db
            .read(move |conn| repo::calendar::hrefs_for_calendar(conn, calendar_id))
            .await?;
        let etags: std::collections::HashMap<String, Option<String>> = local
            .into_iter()
            .map(|(_, href, etag)| (href, etag))
            .collect();
        changed.retain(|(href, etag)| match etags.get(href) {
            Some(stored) => etag.is_none() || stored != etag,
            None => true,
        });
    }

    let mut changed_any = false;
    // Events inserted for the first time, surfaced as "new event" notifications
    // - but only for an incremental pull. The initial full sync would otherwise
    // flag every existing event as new.
    let mut new_events: Vec<NewEventInfo> = Vec::new();

    // Fetch changed resources in batches.
    for batch in changed.chunks(MULTIGET_BATCH) {
        let hrefs: Vec<String> = batch.iter().map(|(h, _)| h.clone()).collect();
        let resp = t
            .request(
                "REPORT",
                calendar_url,
                Some("1"),
                &[],
                Some(xml::report_multiget(&hrefs)),
            )
            .await?;
        if resp.status != 207 {
            return Err(err(format!("multiget: HTTP {}", resp.status)));
        }
        let ms = xml::parse_multistatus(&resp.body)?;
        let items: Vec<(String, String, String)> = ms
            .items
            .iter()
            .filter(|i| i.status == 200)
            .filter_map(|i| {
                Some((
                    href_path(&i.href),
                    i.etag.clone().unwrap_or_default(),
                    i.calendar_data.clone()?,
                ))
            })
            .collect();
        if items.is_empty() {
            continue;
        }
        changed_any = true;
        let batch_new = db
            .write(move |conn| {
                let tx = conn.transaction()?;
                let mut fresh: Vec<NewEventInfo> = Vec::new();
                for (href, etag, ics) in &items {
                    // The master is the VEVENT without RECURRENCE-ID; overrides
                    // ride along inside ical_raw for the expander.
                    let events = parse_ics(ics);
                    let Some(master) = events
                        .iter()
                        .find(|e| e.recurrence_id_ms.is_none())
                        .or_else(|| events.first())
                    else {
                        continue;
                    };
                    let inserted = repo::calendar::upsert_remote(
                        &tx,
                        account_id,
                        calendar_id,
                        href,
                        etag,
                        ics,
                        master,
                    )?;
                    if inserted {
                        fresh.push(NewEventInfo {
                            summary: master.summary.clone(),
                            starts_at: master.starts_at_ms,
                            all_day: master.all_day,
                        });
                    }
                }
                tx.commit()?;
                Ok(fresh)
            })
            .await?;
        let remaining = MAX_NEW_EVENT_NOTIFICATIONS.saturating_sub(new_events.len());
        new_events.extend(batch_new.into_iter().take(remaining));
    }

    // Only the incremental path yields genuinely-new events worth announcing;
    // the initial/fallback full pull inserts the whole collection.
    if !incremental_ok {
        new_events.clear();
    }

    if !removed.is_empty() {
        changed_any = true;
        db.write(move |conn| {
            let tx = conn.transaction()?;
            for href in &removed {
                repo::calendar::delete_by_href(&tx, calendar_id, href)?;
            }
            tx.commit()?;
            Ok(())
        })
        .await?;
    }

    let synced_at = now_ms();
    db.write(move |conn| {
        repo::caldav::set_sync_state(
            conn,
            calendar_id,
            ctag.as_deref(),
            new_token.as_deref(),
            synced_at,
        )
    })
    .await?;
    Ok((changed_any, new_events))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::caldav::http::{DavResponse, MockTransport};

    /// Account 1 with one CalDAV connection (id 1) syncing calendar 10.
    pub(crate) async fn seed_db() -> (Db, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open_calendar(&dir.path().join("t.db")).unwrap();
        db.write(|c| {
            c.execute(
                "INSERT INTO calendar_connections (id, account_id, kind, base_url) VALUES (1, 1, 'generic', 'https://dav.example.com/')",
                [],
            )?;
            c.execute(
                "INSERT INTO calendars (id, account_id, connection_id, url, enabled) VALUES (10, 1, 1, 'https://dav.example.com/cal/me/work/', 1)",
                [],
            )?;
            Ok(())
        })
        .await
        .unwrap();
        (db, dir)
    }

    /// The calendar task's cycle for the seeded account's one connection.
    pub(crate) async fn sync_account(
        db: &Db,
        bus: &EventBus,
        t: &dyn Transport,
        account_id: i64,
        self_email: &str,
    ) -> Result<bool> {
        let routes = db
            .read(move |c| crate::caldav::route::pending(c, account_id))
            .await?;
        let pushes: Vec<PendingPush> = routes.pushes.into_values().flatten().collect();
        sync_connection(db, bus, t, account_id, 1, self_email, &pushes)
            .await
            .map(|synced| synced.changed)
    }

    pub(crate) fn ms207(body: &str) -> DavResponse {
        DavResponse {
            status: 207,
            etag: None,
            body: body.into(),
        }
    }

    const CTAG_1: &str = r#"<d:multistatus xmlns:d="DAV:" xmlns:cs="http://calendarserver.org/ns/"><d:response><d:href>/cal/me/work/</d:href><d:propstat><d:prop><cs:getctag>c1</cs:getctag><d:sync-token>tok-1</d:sync-token></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response></d:multistatus>"#;
    const QUERY_ONE: &str = r#"<d:multistatus xmlns:d="DAV:"><d:response><d:href>/cal/me/work/a.ics</d:href><d:propstat><d:prop><d:getetag>"e1"</d:getetag></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response></d:multistatus>"#;
    const MULTIGET_ONE: &str = r#"<d:multistatus xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav"><d:response><d:href>/cal/me/work/a.ics</d:href><d:propstat><d:prop><d:getetag>"e1"</d:getetag><c:calendar-data>BEGIN:VCALENDAR
BEGIN:VEVENT
UID:a@server
SUMMARY:Pulled
DTSTART:20260801T090000Z
DTEND:20260801T100000Z
END:VEVENT
END:VCALENDAR</c:calendar-data></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response></d:multistatus>"#;
    const SYNC_REMOVE: &str = r#"<d:multistatus xmlns:d="DAV:"><d:response><d:href>/cal/me/work/a.ics</d:href><d:status>HTTP/1.1 404 Not Found</d:status></d:response><d:sync-token>tok-2</d:sync-token></d:multistatus>"#;

    #[tokio::test]
    async fn initial_full_sync_then_incremental_removal() {
        let (db, _dir) = seed_db().await;
        let bus = EventBus::new();

        // Cycle 1: no token yet -> ctag probe, calendar-query, multiget.
        let t = MockTransport::new(vec![ms207(CTAG_1), ms207(QUERY_ONE), ms207(MULTIGET_ONE)]);
        let changed = sync_account(&db, &bus, &t, 1, "me@test.dev").await.unwrap();
        assert!(changed);
        let events = db
            .read(|c| repo::calendar::list_range(c, 0, i64::MAX / 2))
            .await
            .unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].summary.as_deref(), Some("Pulled"));

        // Cycle 2: same ctag -> nothing else fetched.
        let t = MockTransport::new(vec![ms207(CTAG_1)]);
        let changed = sync_account(&db, &bus, &t, 1, "me@test.dev").await.unwrap();
        assert!(!changed);
        assert_eq!(t.seen.lock().unwrap().len(), 1);

        // Cycle 3: new ctag, incremental removal via stored token.
        let ctag2 = CTAG_1.replace("c1", "c2").replace("tok-1", "tok-2");
        let t = MockTransport::new(vec![ms207(&ctag2), ms207(SYNC_REMOVE)]);
        let changed = sync_account(&db, &bus, &t, 1, "me@test.dev").await.unwrap();
        assert!(changed);
        let events = db
            .read(|c| repo::calendar::list_range(c, 0, i64::MAX / 2))
            .await
            .unwrap();
        assert!(events.is_empty());
    }

    #[tokio::test]
    async fn invalid_sync_token_falls_back_to_query() {
        let (db, _dir) = seed_db().await;
        let bus = EventBus::new();
        db.write(|c| repo::caldav::set_sync_state(c, 10, Some("old"), Some("stale-token"), 1))
            .await
            .unwrap();

        let invalid = DavResponse {
            status: 409,
            etag: None,
            body: "<d:error xmlns:d=\"DAV:\"><d:valid-sync-token/></d:error>".into(),
        };
        let t = MockTransport::new(vec![
            ms207(CTAG_1),
            invalid,
            ms207(QUERY_ONE),
            ms207(MULTIGET_ONE),
        ]);
        let changed = sync_account(&db, &bus, &t, 1, "me@test.dev").await.unwrap();
        assert!(changed);
        let events = db
            .read(|c| repo::calendar::list_range(c, 0, i64::MAX / 2))
            .await
            .unwrap();
        assert_eq!(events.len(), 1);
    }

    #[tokio::test]
    async fn dirty_local_edit_survives_pull() {
        let (db, _dir) = seed_db().await;
        let bus = EventBus::new();

        db.write(|c| {
            let ev = crate::calendar::IcsEvent {
                uid: "a@server".into(),
                summary: Some("Server".into()),
                starts_at_ms: 1_000,
                ends_at_ms: Some(2_000),
                ..Default::default()
            };
            repo::calendar::upsert_remote(c, 1, 10, "/cal/me/work/a.ics", "\"e0\"", "RAW", &ev)?;
            c.execute(
                "UPDATE calendar_events SET summary = 'Mine', dirty = 1 WHERE ical_uid = 'a@server'",
                [],
            )?;
            Ok(())
        })
        .await
        .unwrap();

        // push_dirty PUT fails with 500 (row stays dirty); the pull then sees
        // a changed etag for the same href and must not clobber the edit.
        let put_fail = DavResponse {
            status: 500,
            etag: None,
            body: String::new(),
        };
        let ctag2 = CTAG_1.replace("c1", "c9");
        let t = MockTransport::new(vec![
            put_fail,
            ms207(&ctag2),
            ms207(QUERY_ONE),
            ms207(MULTIGET_ONE),
        ]);
        let _ = sync_account(&db, &bus, &t, 1, "me@test.dev").await;
        let events = db
            .read(|c| repo::calendar::list_range(c, 0, i64::MAX / 2))
            .await
            .unwrap();
        assert_eq!(events[0].summary.as_deref(), Some("Mine"));
    }

    #[tokio::test]
    async fn a_connection_pulls_only_its_calendars_and_names_a_failing_one() {
        let (db, _dir) = seed_db().await;
        db.write(|c| {
            c.execute(
                "UPDATE calendars SET display_name = 'Work' WHERE id = 10",
                [],
            )?;
            c.execute(
                "INSERT INTO calendars (id, account_id, connection_id, url, display_name, enabled)
                 VALUES (11, 1, 1, 'https://dav.example.com/cal/me/home/', 'Home', 1)",
                [],
            )?;
            // A second server of the same account syncs on its own.
            c.execute(
                "INSERT INTO calendar_connections (id, account_id, kind, base_url)
                 VALUES (2, 1, 'generic', 'https://shared.example.com/')",
                [],
            )?;
            c.execute(
                "INSERT INTO calendars (id, account_id, connection_id, url, enabled)
                 VALUES (20, 1, 2, 'https://shared.example.com/team/', 1)",
                [],
            )?;
            Ok(())
        })
        .await
        .unwrap();
        let bus = EventBus::new();
        let forbidden = DavResponse {
            status: 403,
            etag: None,
            body: String::new(),
        };
        // Home syncs normally; Work refuses both its ctag probe and the
        // calendar-query that follows.
        let home = |body: &str| body.replace("/cal/me/work/", "/cal/me/home/");
        let t = MockTransport::new(vec![
            ms207(&home(CTAG_1)),
            ms207(&home(QUERY_ONE)),
            ms207(&home(MULTIGET_ONE)),
            forbidden.clone(),
            forbidden,
        ]);
        let synced = sync_connection(&db, &bus, &t, 1, 1, "me@test.dev", &[])
            .await
            .unwrap();
        assert!(synced.changed);
        assert_eq!(
            synced.calendar_error.as_deref(),
            Some("Work: caldav error: calendar-query: HTTP 403")
        );
        let urls: Vec<String> = t
            .seen
            .lock()
            .unwrap()
            .iter()
            .map(|(_, url, _)| url.clone())
            .collect();
        assert!(
            urls.iter()
                .all(|url| url.starts_with("https://dav.example.com/"))
        );
        let events = db
            .read(|c| repo::calendar::list_range(c, 0, i64::MAX / 2))
            .await
            .unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].calendar_id, Some(11));
    }
}
