//! Synthetic SQLite working-set probe. Run with:
//! cargo run -p flectar-mail-core --example sqlite_cache_probe --release -- 10000
//!
//! Optional second/third arguments override each connection's cache KiB and
//! mmap byte limit. Each run creates a fresh, temporary synthetic mailbox.
//! An optional fourth argument is a new profile data directory to retain the
//! fixture for application-level measurements; it must not already contain a
//! database.
//! Pass `--index-search` after the profile path to index synthetic messages
//! into FTS. Pass `--rich-fixture` to add a long multilingual HTML message
//! with three cached CID images and a formatted draft with a staged attachment.
//! Compare runs under the same OS cache conditions; first-query timing here is
//! not a cold-disk benchmark.

use flectar_mail_core::db::{Db, DbConnectionCacheUsage, repo};
use flectar_mail_core::models::View;
use serde_json::json;
use sha2::{Digest, Sha256};
use std::io::Read;
use std::path::Path;
use std::time::Instant;

fn argument(index: usize, default: i64) -> i64 {
    std::env::args()
        .nth(index)
        .map(|arg| arg.parse::<i64>().expect("expected an integer argument"))
        .unwrap_or(default)
}

fn png_chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    let len = u32::try_from(data.len()).expect("PNG chunk fits in u32");
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    let mut crc = !0u32;
    for byte in kind.iter().chain(data) {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xedb8_8320 & (0u32.wrapping_sub(crc & 1)));
        }
    }
    out.extend_from_slice(&(!crc).to_be_bytes());
}

/// A deterministic, lossless 768x768 PNG with stored DEFLATE blocks. Stored
/// blocks keep the encoded and decoded owners large enough to profile overlap.
fn fixture_png(seed: u8) -> Vec<u8> {
    const SIDE: usize = 768;
    let mut pixels = Vec::with_capacity(SIDE * (1 + SIDE * 4));
    for y in 0..SIDE {
        pixels.push(0); // PNG filter: none
        for x in 0..SIDE {
            let v = (x as u32).wrapping_mul(1_103_515_245)
                ^ (y as u32).wrapping_mul(12_345_679)
                ^ u32::from(seed);
            pixels.extend_from_slice(&[v as u8, (v >> 8) as u8, (v >> 16) as u8, 255]);
        }
    }
    let mut zlib = Vec::with_capacity(pixels.len() + pixels.len() / 65_535 * 5 + 16);
    zlib.extend_from_slice(&[0x78, 0x01]);
    for (index, block) in pixels.chunks(65_535).enumerate() {
        let last = (index + 1) * 65_535 >= pixels.len();
        let len = u16::try_from(block.len()).unwrap();
        zlib.push(u8::from(last));
        zlib.extend_from_slice(&len.to_le_bytes());
        zlib.extend_from_slice(&(!len).to_le_bytes());
        zlib.extend_from_slice(block);
    }
    let (mut a, mut b) = (1u32, 0u32);
    for byte in pixels {
        a = (a + u32::from(byte)) % 65_521;
        b = (b + a) % 65_521;
    }
    zlib.extend_from_slice(&((b << 16) | a).to_be_bytes());
    let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut ihdr = Vec::with_capacity(13);
    ihdr.extend_from_slice(&(SIDE as u32).to_be_bytes());
    ihdr.extend_from_slice(&(SIDE as u32).to_be_bytes());
    ihdr.extend_from_slice(&[8, 6, 0, 0, 0]); // RGBA8
    png_chunk(&mut png, b"IHDR", &ihdr);
    png_chunk(&mut png, b"IDAT", &zlib);
    png_chunk(&mut png, b"IEND", &[]);
    png
}

async fn add_rich_fixture(
    db: &Db,
    profile: &Path,
    base_date: i64,
) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    let asset_dir = profile.join("fixture-assets");
    std::fs::create_dir_all(&asset_dir)?;
    let mut images = Vec::new();
    for index in 0..3 {
        let path = asset_dir.join(format!("inline-{index}.png"));
        let png = fixture_png(index);
        std::fs::write(&path, &png)?;
        images.push((path.to_string_lossy().into_owned(), png.len()));
    }
    let staged_dir = profile.join("draft_attachments").join("rich-fixture");
    std::fs::create_dir_all(&staged_dir)?;
    let staged_path = staged_dir.join("notes.txt");
    std::fs::write(
        &staged_path,
        "Attachment for the rich draft.\n".repeat(36_000),
    )?;
    let staged_size = std::fs::metadata(&staged_path)?.len();
    let paragraph =
        "English résumé — العربية مرحبًا — हिन्दी नमस्ते — 日本語こんにちは — emoji 👩🏽‍💻 🇯🇵\n";
    let text_body = paragraph.repeat(12_000);
    let html_body = format!(
        "<html><body><h1>Multilingual image fixture</h1>{}<figure>{}</figure></body></html>",
        format!("<p>{paragraph}</p>").repeat(8_000),
        (0..3)
            .map(|i| format!("<img src=\"cid:fixture-{i}@local\" width=\"768\" height=\"768\">"))
            .collect::<String>()
    );
    let draft_html = format!(
        "<div><h2>Formatted draft</h2><p><strong>Bold</strong> and <em>italic</em> — العربية 日本語</p>{}</div>",
        "<p>Draft paragraph with <a href=\"https://example.test\">a link</a>.</p>".repeat(3_000)
    );
    let body_bytes = text_body.len() + html_body.len() + draft_html.len();
    let attachment_paths = images.clone();
    let staged_path_text = staged_path.to_string_lossy().into_owned();
    db.write(move |conn| {
            let tx = conn.transaction()?;
            tx.execute(
                "UPDATE messages SET subject = 'Multilingual image fixture', snippet = 'Multilingual fixture', body_state = 'cached', has_attachments = 1 WHERE id = 1",
                [],
            )?;
            tx.execute(
                "INSERT INTO message_bodies(message_id,text_body,html_body) VALUES(1,?1,?2)",
                rusqlite::params![text_body, html_body],
            )?;
            for (index, (path, size)) in attachment_paths.iter().enumerate() {
                tx.execute(
                    "INSERT INTO attachments(message_id,part_id,filename,mime_type,size,content_id,is_inline,file_path)
                     VALUES(1,?1,?2,'image/png',?3,?4,1,?5)",
                    rusqlite::params![format!("fixture-{index}"), format!("inline-{index}.png"),
                        *size as i64, format!("<fixture-{index}@local>"), path],
                )?;
            }
            tx.execute("INSERT INTO folders(id,account_id,path,role) VALUES(2,1,'Drafts','drafts')", [])?;
            let draft_thread_id: i64 = tx.query_row("SELECT MAX(id) + 1 FROM threads", [], |row| row.get(0))?;
            tx.execute(
                "INSERT INTO threads(id,account_id,subject_norm,last_message_at,message_count,snippet)
                 VALUES(?1,1,'formatted draft',?2,1,'Formatted draft')",
                rusqlite::params![draft_thread_id, base_date + 1],
            )?;
            tx.execute(
                "INSERT INTO messages(account_id,thread_id,folder_id,message_id,subject,from_addr,date,is_read,is_draft,is_outgoing,has_attachments,body_state)
                 VALUES(1,?1,2,'fixture-draft','Formatted draft','probe@example.test',?2,1,1,1,1,'cached')",
                rusqlite::params![draft_thread_id, base_date + 1],
            )?;
            let draft_id = tx.last_insert_rowid();
            tx.execute("INSERT INTO drafts_meta(message_id,mode) VALUES(?1,'new')", [draft_id])?;
            tx.execute(
                "INSERT INTO message_bodies(message_id,text_body,html_body) VALUES(?1,?2,?3)",
                rusqlite::params![draft_id, "Formatted draft — العربية 日本語", draft_html],
            )?;
            tx.execute(
                "INSERT INTO draft_attachments(draft_id,file_path,filename,mime_type) VALUES(?1,?2,'notes.txt','text/plain')",
                rusqlite::params![draft_id, staged_path_text],
            )?;
            tx.commit()?;
            Ok(())
        }).await?;
    Ok(json!({
        "message_id": 1,
        "image_count": images.len(),
        "image_dimensions": [768, 768],
        "image_file_bytes": images.iter().map(|(_, size)| size).sum::<usize>(),
        "body_bytes": body_bytes,
        "draft_attachment_bytes": staged_size,
    }))
}

fn file_sha256(path: &Path) -> std::io::Result<String> {
    let mut file = std::fs::File::open(path)?;
    let mut hash = Sha256::new();
    let mut chunk = [0u8; 64 * 1024];
    loop {
        let len = file.read(&mut chunk)?;
        if len == 0 {
            break;
        }
        hash.update(&chunk[..len]);
    }
    Ok(format!("{:x}", hash.finalize()))
}

fn snapshot(rows: Vec<DbConnectionCacheUsage>) -> serde_json::Value {
    json!(
        rows.into_iter()
            .map(|row| json!({
                "role": row.role,
                "cache_bytes": row.usage.cache_bytes,
                "cache_limit_kib": row.usage.cache_limit_kib,
                "mmap_limit_bytes": row.usage.mmap_limit_bytes,
            }))
            .collect::<Vec<_>>()
    )
}

/// Resident pages in mappings of the temporary mailbox file, not the full
/// process RSS or the OS page cache. Only available through Linux smaps.
#[cfg(target_os = "linux")]
fn mapped_mailbox_pages(path: &Path) -> std::io::Result<serde_json::Value> {
    let smaps = std::fs::read_to_string("/proc/self/smaps")?;
    let mut matching = false;
    let mut mappings = 0u64;
    let mut rss_kib = 0u64;
    let mut pss_kib = 0u64;
    for line in smaps.lines() {
        let mut words = line.split_whitespace();
        let first = words.next().unwrap_or_default();
        if first.contains('-')
            && first
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() || byte == b'-')
        {
            matching = line
                .split_whitespace()
                .last()
                .is_some_and(|name| name == path.to_string_lossy());
            if matching {
                mappings += 1;
            }
        } else if matching {
            if let Some(value) = line.strip_prefix("Rss:") {
                rss_kib += value
                    .split_whitespace()
                    .next()
                    .unwrap_or("0")
                    .parse::<u64>()
                    .unwrap_or(0);
            } else if let Some(value) = line.strip_prefix("Pss:") {
                pss_kib += value
                    .split_whitespace()
                    .next()
                    .unwrap_or("0")
                    .parse::<u64>()
                    .unwrap_or(0);
            }
        }
    }
    Ok(json!({"mappings": mappings, "rss_kib": rss_kib, "pss_kib": pss_kib}))
}

#[cfg(not(target_os = "linux"))]
fn mapped_mailbox_pages(_path: &Path) -> std::io::Result<serde_json::Value> {
    Ok(serde_json::Value::Null)
}

#[cfg(target_os = "linux")]
fn process_memory() -> std::io::Result<serde_json::Value> {
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

#[cfg(not(target_os = "linux"))]
fn process_memory() -> std::io::Result<serde_json::Value> {
    Ok(serde_json::Value::Null)
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let count = argument(1, 10_000);
    assert!((1..=100_000).contains(&count));
    let cache_kib = argument(2, 8_192);
    let mmap_bytes = argument(3, 8_388_608);
    assert!(cache_kib > 0 && mmap_bytes >= 0);

    let profile_dir = std::env::args().nth(4);
    let index_search = std::env::args().skip(5).any(|arg| arg == "--index-search");
    let rich_fixture = std::env::args().skip(5).any(|arg| arg == "--rich-fixture");
    let temp = profile_dir.is_none().then(tempfile::tempdir).transpose()?;
    let path = if let Some(profile_dir) = profile_dir {
        let profile_dir = std::path::PathBuf::from(profile_dir);
        std::fs::create_dir_all(&profile_dir)?;
        profile_dir.join("flectar-mail.db")
    } else {
        temp.as_ref().unwrap().path().join("mail.db")
    };
    if path.exists() {
        return Err(format!("refusing to overwrite existing fixture: {}", path.display()).into());
    }
    let base_date = i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_millis(),
    )?;
    let db = Db::open(&path)?;
    db.write(move |conn| {
        let tx = conn.transaction()?;
        tx.execute_batch(
            "INSERT INTO accounts (id,email,provider,auth_kind,username,incoming_host,incoming_port,smtp_host,smtp_port,created_at,mail_protocol,settings_json)
             VALUES (1,'probe@example.test','generic','password','probe','localhost',993,'localhost',587,0,'imap','{\"connection\":{\"incomingSecurity\":\"tls\",\"smtpSecurity\":\"tls\"}}');
             INSERT INTO folders (id,account_id,path,role) VALUES (1,1,'INBOX','inbox');
             INSERT INTO route_cache (sender_domain,route_key)
             VALUES ('__routing_backfill__','1');",
        )?;
        {
            let mut threads = tx.prepare(
                "INSERT INTO threads (id,account_id,subject_norm,last_message_at,message_count,unread_count,snippet)
                 VALUES (?1,1,'subject',?2,1,?3,'synthetic snippet')",
            )?;
            let mut messages = tx.prepare(
                "INSERT INTO messages (account_id,thread_id,folder_id,uid,message_id,subject,from_addr,date,is_read)
                 VALUES (1,?1,1,?1,?2,'subject','sender@example.test',?3,?4)",
            )?;
            for id in 1..=count {
                let unread = i64::from(id % 2 == 0);
                let date = base_date - (count - id) * 1_000;
                threads.execute(rusqlite::params![id, date, unread])?;
                messages.execute(rusqlite::params![
                    id,
                    format!("probe-{id}"),
                    date,
                    1 - unread
                ])?;
            }
        }
        tx.commit()?;
        Ok(())
    })
    .await?;
    let rich_metadata = if rich_fixture {
        let profile = path
            .parent()
            .expect("fixture DB has a parent directory")
            .canonicalize()?;
        Some(add_rich_fixture(&db, &profile, base_date).await?)
    } else {
        None
    };
    if index_search {
        db.write(|conn| {
            conn.execute_batch(
                "INSERT INTO messages_fts (rowid, subject, from_text, to_text, body)
                 SELECT id, subject, from_addr, to_json || ' ' || cc_json, snippet
                 FROM messages",
            )?;
            Ok(())
        })
        .await?;
    }
    db.release_idle_connections().await?;
    drop(db);
    let fixture_sha256 = rich_fixture.then(|| file_sha256(&path)).transpose()?;

    // Reopen after seeding so the sampled private caches do not include the
    // write-side fixture construction. The OS file cache may still be warm.
    let db = Db::open(&path)?;
    for read in [false, true] {
        let set_profile = move |conn: &mut rusqlite::Connection| {
            conn.pragma_update(None, "cache_size", -cache_kib)?;
            conn.pragma_update(None, "mmap_size", mmap_bytes)?;
            Ok(())
        };
        if read {
            db.read(set_profile).await?;
        } else {
            db.write(set_profile).await?;
        }
    }
    let before = snapshot(db.cache_usage().await?);
    let mapped_before = mapped_mailbox_pages(&path)?;
    let process_before = process_memory()?;
    let plans = db
        .read(|conn| {
            let args = repo::threads::ListArgs {
                view: View::Inbox,
                tab: None,
                account_id: Some(1),
                account_ids: None,
                folder_id: None,
                cursor: None,
                limit: 50,
            };
            Ok(json!({
                "inbox_badge": repo::counts::inbox_badge_query_plan(conn)?,
                "inbox_list": repo::threads::list_query_plan(conn, &args)?,
            }))
        })
        .await?;
    let mut samples = Vec::new();
    for name in ["first", "repeat"] {
        let args = repo::threads::ListArgs {
            view: View::Inbox,
            tab: None,
            account_id: Some(1),
            account_ids: None,
            folder_id: None,
            cursor: None,
            limit: 50,
        };
        let started = Instant::now();
        let badges = db
            .read(|conn| repo::counts::mailbox_badge_counts(conn))
            .await?;
        let badge_elapsed_ms = started.elapsed().as_secs_f64() * 1000.0;
        let process_after_badge = process_memory()?;
        let badge_connections = snapshot(db.cache_usage().await?);
        let started = Instant::now();
        let page = db
            .read(move |conn| repo::threads::list(conn, &args))
            .await?;
        let list_elapsed_ms = started.elapsed().as_secs_f64() * 1000.0;
        assert_eq!(badges.len(), 1);
        assert_eq!(badges[0].inbox, count / 2);
        assert_eq!(page.threads.len(), usize::try_from(count.min(50))?);
        samples.push(json!({
            "name": name,
            "elapsed_ms": badge_elapsed_ms + list_elapsed_ms,
            "badge_elapsed_ms": badge_elapsed_ms,
            "list_elapsed_ms": list_elapsed_ms,
            "inbox_badge": badges[0].inbox,
            "list_rows": page.threads.len(),
            "after_badge": {"process": process_after_badge, "connections": badge_connections},
            "process": process_memory()?,
            "connections": snapshot(db.cache_usage().await?),
            "mailbox_mapped_pages": mapped_mailbox_pages(&path)?,
        }));
    }
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "synthetic_threads": count,
            "rich_fixture": rich_metadata,
            "fixture_sha256": fixture_sha256,
            "requested_cache_kib": cache_kib,
            "requested_mmap_bytes": mmap_bytes,
            "before_query": before,
            "mailbox_mapped_pages_before": mapped_before,
            "process_before": process_before,
            "query_plans": plans,
            "queries": samples,
        }))?
    );
    Ok(())
}
