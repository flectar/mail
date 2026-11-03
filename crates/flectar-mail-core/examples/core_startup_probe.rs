//! Measure core-only startup and mailbox query memory on an existing fixture.
//! The fixture is copied before use and is never modified in place.

use flectar_mail_core::{
    Core,
    config::Paths,
    models::{SearchCursor, View},
};
use serde_json::json;
use std::path::Path;
use std::time::Instant;

fn memory() -> std::io::Result<serde_json::Value> {
    let rollup = std::fs::read_to_string("/proc/self/smaps_rollup")?;
    let mut values = serde_json::Map::new();
    for line in rollup.lines() {
        let Some((name, rest)) = line.split_once(':') else {
            continue;
        };
        if matches!(name, "Rss" | "Pss" | "Pss_Anon" | "Pss_File" | "Swap") {
            let kib = rest
                .split_whitespace()
                .next()
                .unwrap_or("0")
                .parse::<u64>()
                .unwrap_or(0);
            values.insert(format!("{}_kib", name.to_ascii_lowercase()), json!(kib));
        }
    }
    Ok(serde_json::Value::Object(values))
}

async fn sample(core: &Core) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    Ok(json!({
        "process": memory()?,
        "mail_connections": core.db.cache_usage().await?
            .into_iter()
            .map(|row| json!({"role": row.role, "cache_bytes": row.usage.cache_bytes}))
            .collect::<Vec<_>>(),
        "calendar_open": core.calendar_db.is_open(),
        "files_open": core.files_db.is_open(),
    }))
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let source = std::env::args()
        .nth(1)
        .ok_or("pass a profile data directory")?;
    let empty_profile = source == "--empty";
    let release_after_start = std::env::args().nth(2).as_deref() == Some("--release-after-start");
    let release_before_settle = std::env::args().any(|arg| arg == "--release-before-settle");
    let compare_reader_reopen = std::env::args().any(|arg| arg == "--compare-reader-reopen");
    if compare_reader_reopen && !release_before_settle {
        return Err("--compare-reader-reopen requires --release-before-settle".into());
    }
    let open_calendar_before_settle =
        std::env::args().any(|arg| arg == "--open-calendar-before-settle");
    let settle_seconds = std::env::args()
        .find_map(|arg| arg.strip_prefix("--settle-seconds=").map(str::to_owned))
        .map(|value| value.parse::<u64>())
        .transpose()?
        .unwrap_or(2);
    let search_after_badges = std::env::args().any(|arg| arg == "--search");
    let deep_search_after_badges = std::env::args().any(|arg| arg == "--deep-search");
    let rich_body_after_badges = std::env::args().any(|arg| arg == "--rich-body");
    let reader_cache_kib = std::env::args()
        .find_map(|arg| arg.strip_prefix("--reader-cache-kib=").map(str::to_owned))
        .map(|value| value.parse::<i64>())
        .transpose()?;
    let reader_mmap_bytes = std::env::args()
        .find_map(|arg| arg.strip_prefix("--reader-mmap-bytes=").map(str::to_owned))
        .map(|value| value.parse::<i64>())
        .transpose()?;
    if let Some(cache_kib) = reader_cache_kib {
        assert!(cache_kib > 0);
    }
    if let Some(mmap_bytes) = reader_mmap_bytes {
        assert!(mmap_bytes >= 0);
    }
    let temp = tempfile::tempdir()?;
    if !empty_profile {
        let source_db = Path::new(&source).join("flectar-mail.db");
        std::fs::copy(source_db, temp.path().join("flectar-mail.db"))?;
    }
    let started = Instant::now();
    let core =
        Core::start_mail_ui(Paths::new(temp.path().into(), temp.path().join("cache"))).await?;
    if empty_profile {
        core.notify_ui_ready();
    }
    let startup_ms = started.elapsed().as_secs_f64() * 1000.0;
    let after_start = sample(&core).await?;
    let after_release = if release_after_start {
        core.db.release_idle_connections().await?;
        Some(sample(&core).await?)
    } else {
        None
    };
    if reader_cache_kib.is_some() || reader_mmap_bytes.is_some() {
        core.db
            .read(move |conn| {
                if let Some(cache_kib) = reader_cache_kib {
                    conn.pragma_update(None, "cache_size", -cache_kib)?;
                }
                if let Some(mmap_bytes) = reader_mmap_bytes {
                    conn.pragma_update(None, "mmap_size", mmap_bytes)?;
                }
                Ok(())
            })
            .await?;
    }
    let accounts = core.list_accounts().await?;
    let folders = core.list_folders(None).await?;
    let settings = core.get_settings().await?;
    let after_metadata = sample(&core).await?;
    let started = Instant::now();
    let page = core
        .list_threads(View::Inbox, None, None, None, None, (None, 25))
        .await?;
    let list_ms = started.elapsed().as_secs_f64() * 1000.0;
    let after_list = sample(&core).await?;
    let started = Instant::now();
    let badges = core
        .mailbox_badge_counts(flectar_mail_core::models::MailGrouping::Conversations)
        .await?;
    let badges_ms = started.elapsed().as_secs_f64() * 1000.0;
    let after_badges = sample(&core).await?;
    let deep_search = if deep_search_after_badges {
        let (last_match_at, thread_id) = core
            .db
            .read(|conn| {
                conn.query_row(
                    "SELECT date, thread_id FROM messages WHERE id = (SELECT MAX(id) / 2 FROM messages)",
                    [],
                    |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
                )
                .map_err(Into::into)
            })
            .await?;
        let started = Instant::now();
        let page = core
            .search_chronological_page(
                "subject".to_owned(),
                None,
                Some(SearchCursor {
                    last_match_at,
                    thread_id,
                    relaxed: false,
                }),
                25,
            )
            .await?;
        let elapsed_ms = started.elapsed().as_secs_f64() * 1000.0;
        let after = sample(&core).await?;
        Some(json!({
            "cursor_thread_id": thread_id,
            "rows": page.threads.len(),
            "has_next": page.next_cursor.is_some(),
            "elapsed_ms": elapsed_ms,
            "after": after,
        }))
    } else {
        None
    };
    let rich_body = if rich_body_after_badges {
        let started = Instant::now();
        let body = core.get_body(1).await?;
        let body_ms = started.elapsed().as_secs_f64() * 1000.0;
        let html = body
            .html_body
            .as_deref()
            .ok_or("rich message has no HTML")?;
        let inline_images = html.matches("data:image/png;base64,").count();
        assert_eq!(
            inline_images, 3,
            "all three fixture CID images should resolve"
        );
        let after_body = sample(&core).await?;
        let draft_id = core
            .db
            .read(|conn| {
                conn.query_row("SELECT id FROM messages WHERE is_draft = 1", [], |row| {
                    row.get::<_, i64>(0)
                })
                .map_err(Into::into)
            })
            .await?;
        let started = Instant::now();
        let draft = core.get_draft(draft_id).await?;
        let draft_ms = started.elapsed().as_secs_f64() * 1000.0;
        assert_eq!(draft.attachments.len(), 1);
        assert!(Path::new(&draft.attachments[0].file_path).is_file());
        let after_draft = sample(&core).await?;
        let body_html_bytes = html.len();
        let draft_html_bytes = draft.body_html.as_deref().map_or(0, str::len);
        drop(body);
        drop(draft);
        let after_drop = sample(&core).await?;
        let started = Instant::now();
        let thread = core.get_thread(1).await?;
        let thread_ms = started.elapsed().as_secs_f64() * 1000.0;
        assert_eq!(thread.messages.len(), 1);
        assert_eq!(
            thread.messages[0]
                .html_body
                .as_deref()
                .unwrap_or_default()
                .matches("data:image/png;base64,")
                .count(),
            3,
            "the batch thread path should resolve all fixture CID images"
        );
        let after_thread = sample(&core).await?;
        drop(thread);
        Some(json!({
            "body_ms": body_ms,
            "draft_ms": draft_ms,
            "thread_ms": thread_ms,
            "body_html_bytes": body_html_bytes,
            "draft_html_bytes": draft_html_bytes,
            "inline_images": inline_images,
            "after_body": after_body,
            "after_draft": after_draft,
            "after_drop": after_drop,
            "after_thread": after_thread,
        }))
    } else {
        None
    };
    let (search_ms, repeat_search_ms, next_search_ms, search_rows, search_has_next, after_search) =
        if search_after_badges {
            let started = Instant::now();
            let search = core
                .search_chronological_page("subject".to_owned(), None, None, 25)
                .await?;
            let search_ms = started.elapsed().as_secs_f64() * 1000.0;
            let started = Instant::now();
            let repeat = core
                .search_chronological_page("subject".to_owned(), None, None, 25)
                .await?;
            let repeat_search_ms = started.elapsed().as_secs_f64() * 1000.0;
            assert_eq!(search.threads.len(), repeat.threads.len());
            let next_search_ms = if let Some(cursor) = search.next_cursor {
                let started = Instant::now();
                let next = core
                    .search_chronological_page("subject".to_owned(), None, Some(cursor), 25)
                    .await?;
                assert_eq!(next.threads.len(), search.threads.len());
                Some(started.elapsed().as_secs_f64() * 1000.0)
            } else {
                None
            };
            (
                Some(search_ms),
                Some(repeat_search_ms),
                next_search_ms,
                Some(search.threads.len()),
                Some(search.next_cursor.is_some()),
                Some(sample(&core).await?),
            )
        } else {
            (None, None, None, None, None, None)
        };
    if open_calendar_before_settle {
        core.calendar_db.read(|_| Ok(())).await?;
    }
    let warm_reopen_control = if compare_reader_reopen {
        let started = Instant::now();
        let warm_page = core
            .list_threads(View::Inbox, None, None, None, None, (None, 25))
            .await?;
        let list_ms = started.elapsed().as_secs_f64() * 1000.0;
        let started = Instant::now();
        let warm_badges = core
            .mailbox_badge_counts(flectar_mail_core::models::MailGrouping::Conversations)
            .await?;
        let badges_ms = started.elapsed().as_secs_f64() * 1000.0;
        assert_eq!(
            warm_page
                .threads
                .iter()
                .map(|row| row.id)
                .collect::<Vec<_>>(),
            page.threads.iter().map(|row| row.id).collect::<Vec<_>>()
        );
        assert_eq!(warm_badges, badges);
        Some(json!({"list_ms": list_ms, "badges_ms": badges_ms}))
    } else {
        None
    };
    let before_settle = if release_before_settle {
        core.db.release_idle_connections().await?;
        Some(sample(&core).await?)
    } else {
        None
    };
    tokio::time::sleep(std::time::Duration::from_secs(settle_seconds)).await;
    let after_settle = sample(&core).await?;
    let reader_reopen = if compare_reader_reopen {
        let started = Instant::now();
        let reopened_page = core
            .list_threads(View::Inbox, None, None, None, None, (None, 25))
            .await?;
        let first_list_ms = started.elapsed().as_secs_f64() * 1000.0;
        let started = Instant::now();
        let reopened_badges = core
            .mailbox_badge_counts(flectar_mail_core::models::MailGrouping::Conversations)
            .await?;
        let first_badges_ms = started.elapsed().as_secs_f64() * 1000.0;
        assert_eq!(
            reopened_page
                .threads
                .iter()
                .map(|row| row.id)
                .collect::<Vec<_>>(),
            page.threads.iter().map(|row| row.id).collect::<Vec<_>>()
        );
        assert_eq!(reopened_badges, badges);
        let after_reopen = sample(&core).await?;

        let started = Instant::now();
        let repeated_page = core
            .list_threads(View::Inbox, None, None, None, None, (None, 25))
            .await?;
        let repeat_list_ms = started.elapsed().as_secs_f64() * 1000.0;
        let started = Instant::now();
        let repeated_badges = core
            .mailbox_badge_counts(flectar_mail_core::models::MailGrouping::Conversations)
            .await?;
        let repeat_badges_ms = started.elapsed().as_secs_f64() * 1000.0;
        assert_eq!(
            repeated_page
                .threads
                .iter()
                .map(|row| row.id)
                .collect::<Vec<_>>(),
            page.threads.iter().map(|row| row.id).collect::<Vec<_>>()
        );
        assert_eq!(repeated_badges, badges);

        core.db.release_idle_connections().await?;
        let after_second_release = sample(&core).await?;
        tokio::time::sleep(std::time::Duration::from_secs(settle_seconds)).await;
        let after_second_settle = sample(&core).await?;
        Some(json!({
            "first_list_ms": first_list_ms,
            "first_badges_ms": first_badges_ms,
            "repeat_list_ms": repeat_list_ms,
            "repeat_badges_ms": repeat_badges_ms,
            "after_reopen": after_reopen,
            "after_second_release": after_second_release,
            "after_second_settle": after_second_settle,
        }))
    } else {
        None
    };
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "accounts": accounts.len(),
            "folders": folders.len(),
            "auto_labels_enabled": settings.auto_labels_enabled,
            "page_rows": page.threads.len(),
            "inbox_badge": badges.iter().map(|row| row.inbox).sum::<i64>(),
            "startup_ms": startup_ms,
            "list_ms": list_ms,
            "badges_ms": badges_ms,
            "search_ms": search_ms,
            "repeat_search_ms": repeat_search_ms,
            "next_search_ms": next_search_ms,
            "search_rows": search_rows,
            "search_has_next": search_has_next,
            "reader_cache_kib": reader_cache_kib,
            "reader_mmap_bytes": reader_mmap_bytes,
            "release_after_start": release_after_start,
            "empty_profile": empty_profile,
            "release_before_settle": release_before_settle,
            "compare_reader_reopen": compare_reader_reopen,
            "open_calendar_before_settle": open_calendar_before_settle,
            "settle_seconds": settle_seconds,
            "after_start": after_start,
            "after_release": after_release,
            "after_metadata": after_metadata,
            "after_list": after_list,
            "after_badges": after_badges,
            "deep_search": deep_search,
            "rich_body": rich_body,
            "after_search": after_search,
            "before_settle": before_settle,
            "after_settle": after_settle,
            "warm_reopen_control": warm_reopen_control,
            "reader_reopen": reader_reopen,
        }))?
    );
    Ok(())
}
