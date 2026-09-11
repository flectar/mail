use super::*;
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// Real HTTP framing exercises reqwest and serialization, not an alternate
/// mock implementation of the protocol client.
async fn server(
    replies: Vec<(u16, String, String)>,
) -> (String, tokio::task::JoinHandle<Vec<String>>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        let mut requests = Vec::new();
        for (status, headers, body) in replies {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut bytes = Vec::new();
            let mut chunk = [0; 4096];
            loop {
                let count = stream.read(&mut chunk).await.unwrap();
                assert!(count > 0);
                bytes.extend_from_slice(&chunk[..count]);
                if let Some(pos) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                    let head = String::from_utf8_lossy(&bytes[..pos]);
                    let length = head
                        .lines()
                        .find_map(|l| {
                            l.to_lowercase()
                                .strip_prefix("content-length:")
                                .and_then(|s| s.trim().parse::<usize>().ok())
                        })
                        .unwrap_or(0);
                    if bytes.len() >= pos + 4 + length {
                        break;
                    }
                }
            }
            requests.push(String::from_utf8(bytes).unwrap());
            stream.write_all(format!("HTTP/1.1 {status} Test\r\nConnection: close\r\nContent-Length: {}\r\n{headers}\r\n{body}",body.len()).as_bytes()).await.unwrap();
        }
        requests
    });
    (url, task)
}

#[tokio::test]
async fn dav_listing_and_copy_use_decoded_names_and_no_overwrite() {
    let listing = r#"<d:multistatus xmlns:d="DAV:"><d:response><d:href>/dav/file/me/</d:href><d:propstat><d:prop><d:resourcetype><d:collection/></d:resourcetype><d:current-user-privilege-set><d:privilege><d:all/></d:privilege></d:current-user-privilege-set></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response><d:response><d:href>/dav/file/me/a%20%26%20b.txt</d:href><d:propstat><d:prop><d:resourcetype/><d:getetag>"v1"</d:getetag><d:getcontentlength>3</d:getcontentlength><d:current-user-privilege-set><d:privilege><d:read/></d:privilege></d:current-user-privilege-set></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response></d:multistatus>"#;
    let (base, requests) = server(vec![
        (200, "DAV: 1, 2, access-control\r\n".into(), String::new()),
        (207, String::new(), listing.into()),
        (201, String::new(), String::new()),
    ])
    .await;
    let c = dav::DavClient::connect(&format!("{base}/dav/file/me/"), "user", "secret")
        .await
        .unwrap();
    let list = c.list(&c.root).await.unwrap();
    assert_eq!(list.nodes.len(), 1);
    assert_eq!(list.nodes[0].name, "a & b.txt");
    assert!(list.nodes[0].my_rights.may_rename);
    c.relocate(&list.nodes[0], &c.root, "copy #1.txt", true)
        .await
        .unwrap();
    let requests = requests.await.unwrap();
    assert!(requests[1].contains("depth: 1"));
    assert!(requests[2].starts_with("COPY /dav/file/me/a%20&%20b.txt"));
    assert!(requests[2].contains("overwrite: F"));
    assert!(requests[2].contains("if-match: \"v1\""));
    assert!(requests[2].contains("copy%20%231.txt"));
}
#[tokio::test]
async fn dav_rejects_origin_and_path_escape_before_sending_credentials() {
    let (base, requests) = server(vec![(200, "DAV: 1\r\n".into(), String::new())]).await;
    let c = dav::DavClient::connect(&format!("{base}/dav/file/me/"), "user", "secret")
        .await
        .unwrap();
    for href in [
        "https://evil.example/file",
        "../other/file",
        "/dav/file/other/file",
        "%2e%2e/other/",
    ] {
        assert!(c.href(&c.root, href).is_err(), "{href}");
    }
    assert!(c.child_url(&c.root, "../file", false).is_err());
    assert!(c.href(&c.root, "sub%2Ffolder").is_err());
    requests.await.unwrap();
}
#[tokio::test]
async fn dav_lock_token_is_sent_for_writes_and_unlock() {
    let (base, requests) = server(vec![
        (200, "DAV: 1, 2\r\n".into(), String::new()),
        (
            200,
            "Lock-Token: <opaquelocktoken:abc>\r\n".into(),
            String::new(),
        ),
        (204, String::new(), String::new()),
        (204, String::new(), String::new()),
    ])
    .await;
    let mut c = dav::DavClient::connect(&format!("{base}/dav/file/me/"), "user", "secret")
        .await
        .unwrap();
    let node = FileNode {
        id: format!("{}file", c.root),
        etag: Some("\"v1\"".into()),
        node_type: Some("file".into()),
        ..Default::default()
    };
    c.lock(&node).await.unwrap();
    assert!(c.owns_lock(&node.id));
    c.patch_property(&node, "urn:example", "description", Some("A & B"))
        .await
        .unwrap();
    c.unlock(&node).await.unwrap();
    let requests = requests.await.unwrap();
    assert!(requests[2].contains("(<opaquelocktoken:abc>)"));
    assert!(requests[2].contains("A &amp; B"));
    assert!(requests[3].contains("lock-token: <opaquelocktoken:abc>"));
}
#[tokio::test]
async fn jmap_discovery_scopes_shared_accounts_and_preserves_state_preconditions() {
    // Session URLs need the dynamic port, so use a tiny dedicated server.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let advertised = base.clone();
    let task = tokio::spawn(async move {
        let responses = [
            json!({"capabilities":{"urn:ietf:params:jmap:core":{},jmap::CAPABILITY:{}},"primaryAccounts":{jmap::CAPABILITY:"mine"},"accounts":{"mine":{"name":"Mine","isReadOnly":false,"accountCapabilities":{jmap::CAPABILITY:{"fileNodeQuerySortOptions":["nodeType","name"],"mayCreateTopLevelFileNode":true}}},"shared":{"name":"Team","isReadOnly":true,"accountCapabilities":{jmap::CAPABILITY:{}}}},"apiUrl":format!("{advertised}/jmap"),"uploadUrl":format!("{advertised}/upload/{{accountId}}"),"downloadUrl":format!("{advertised}/download/{{accountId}}/{{blobId}}/{{name}}?type={{type}}")}),
            json!({"methodResponses":[["FileNode/query",{"accountId":"mine","ids":[],"total":0,"queryState":"q1"},"files"]]}),
            json!({"methodResponses":[["FileNode/get",{"accountId":"mine","list":[],"state":"s1"},"files"]]}),
            json!({"methodResponses":[["FileNode/set",{"accountId":"mine","created":{"new":{"id":"folder"}},"newState":"s2"},"files"]]}),
        ];
        let mut requests = Vec::new();
        for response in responses {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut bytes = vec![0; 16384];
            let count = stream.read(&mut bytes).await.unwrap();
            let request = String::from_utf8_lossy(&bytes[..count]).into_owned();
            requests.push(request);
            let body = response.to_string();
            stream.write_all(format!("HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",body.len(),body).as_bytes()).await.unwrap();
        }
        requests
    });
    let mut c = jmap::FileClient::connect(&base, "user", "secret", Some("mail-only-id"))
        .await
        .unwrap();
    assert_eq!(c.account_id, "mine");
    assert_eq!(c.accounts.len(), 2);
    assert!(c.modern());
    let page = c.list(None, "", 0).await.unwrap();
    assert!(page.nodes.is_empty());
    c.create_folder(None, "Folder").await.unwrap();
    assert_eq!(c.state.as_deref(), Some("s2"));
    c.select_account("shared").unwrap();
    assert!(c.create_folder(None, "No").await.is_err());
    let requests = task.await.unwrap();
    assert!(requests[1].contains("\"isTopLevel\":true"));
    assert!(requests[3].contains("\"ifInState\":\"s1\""));
}

#[test]
fn attachment_index_tracks_unicode_names_metadata_and_deletion_with_untrusted_schema() {
    use crate::db::testutil;
    let conn = testutil::conn();
    conn.pragma_update(None, "trusted_schema", false).unwrap();
    testutil::seed_account(&conn);
    let (_, message) =
        testutil::seed_message(&conn, "maria@example.com", "September forecast", false);
    conn.execute("INSERT INTO attachments(id,message_id,filename,mime_type) VALUES (1,?1,'Résumé final.pdf','application/pdf')",[message]).unwrap();
    let count = |query: &str| {
        conn.query_row(
            "SELECT count(*) FROM attachment_files_fts WHERE attachment_files_fts MATCH ?1",
            [super::attachment_match_query(query)],
            |r| r.get::<_, i64>(0),
        )
        .unwrap()
    };
    assert_eq!(count("resume maria"), 1);
    conn.execute(
        "UPDATE messages SET subject='October budget' WHERE id=?1",
        [message],
    )
    .unwrap();
    assert_eq!(count("october"), 1);
    assert_eq!(count("september"), 0);
    conn.execute("DELETE FROM attachments WHERE id=1", [])
        .unwrap();
    assert_eq!(count("resume"), 0);
}

#[test]
fn attachment_pages_are_scoped_stable_and_do_not_mark_mail_read() {
    use crate::db::testutil;
    let conn = testutil::conn();
    testutil::seed_account(&conn);
    let (_, message) = testutil::seed_message(&conn, "maria@example.com", "Forecast", false);
    for i in 1..=205 {
        conn.execute("INSERT INTO attachments(id,message_id,filename,mime_type) VALUES (?1,?2,?3,'application/pdf')",params![i,message,format!("Résumé {i}.pdf")]).unwrap();
    }
    let page = attachment_page(&conn, Some(1), "", None).unwrap();
    assert_eq!(page.len(), PAGE_SIZE + 1);
    assert_eq!(page[0].id, 205);
    conn.execute(
        "INSERT INTO attachments(id,message_id,filename) VALUES (206,?1,'New file')",
        [message],
    )
    .unwrap();
    let next = attachment_page(&conn, Some(1), "", Some(page[PAGE_SIZE - 1].id)).unwrap();
    assert!(next.iter().all(|a| a.id < page[PAGE_SIZE - 1].id));
    assert!(
        attachment_page(&conn, Some(99), "", None)
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        attachment_page(&conn, None, &attachment_match_query("resume maria"), None)
            .unwrap()
            .len(),
        PAGE_SIZE + 1
    );
    assert!(
        attachment_page(&conn, None, &attachment_match_query("\" OR secret*"), None)
            .unwrap()
            .is_empty()
    );
    let read: i64 = conn
        .query_row("SELECT is_read FROM messages WHERE id=?1", [message], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(read, 0);
}

#[test]
fn migration_indexes_existing_attachments_without_losing_mail() {
    let mut conn = rusqlite::Connection::open_in_memory().unwrap();
    conn.execute_batch(include_str!("../db/migrations/001_init.sql"))
        .unwrap();
    conn.pragma_update(None, "user_version", 1).unwrap();
    crate::db::testutil::seed_account(&conn);
    let (_, message) =
        crate::db::testutil::seed_message(&conn, "person@example.com", "Existing mail", false);
    conn.execute(
        "INSERT INTO attachments(message_id,filename) VALUES (?1,'old-contract.pdf')",
        [message],
    )
    .unwrap();
    crate::db::migrations::run(&mut conn).unwrap();
    let page = attachment_page(&conn, None, &attachment_match_query("contract"), None).unwrap();
    assert_eq!(page.len(), 1);
    assert_eq!(page[0].filename, "old-contract.pdf");
    assert_eq!(
        conn.query_row("PRAGMA integrity_check", [], |r| r.get::<_, String>(0))
            .unwrap(),
        "ok"
    );
}

#[tokio::test]
async fn discovery_follows_only_same_origin_redirects() {
    let (base, requests) = server(vec![
        (307, "Location: /jmap/session\r\n".into(), String::new()),
        (
            200,
            "Content-Type: application/json\r\n".into(),
            "{\"sessionState\":\"s1\"}".into(),
        ),
    ])
    .await;
    let t = transport::Transport::new(&base, "user", "secret").unwrap();
    assert_eq!(
        t.discover(&format!("{base}/.well-known/jmap"))
            .await
            .unwrap()["sessionState"],
        "s1"
    );
    assert!(requests.await.unwrap()[1].starts_with("GET /jmap/session"));
    let (base, requests) = server(vec![(
        307,
        "Location: https://other.example/jmap/session\r\n".into(),
        String::new(),
    )])
    .await;
    let t = transport::Transport::new(&base, "user", "secret").unwrap();
    assert!(
        t.discover(&format!("{base}/.well-known/jmap"))
            .await
            .is_err()
    );
    requests.await.unwrap();
}

#[tokio::test]
async fn discovery_resolves_templates_from_final_redirect_location() {
    let (base, requests) = server(vec![
        (307, "Location: /proxy/session\r\n".into(), String::new()),
        (
            200,
            "Content-Type: application/json\r\n".into(),
            json!({
                "apiUrl": "api", "uploadUrl": "upload/{accountId}",
                "downloadUrl": "download/{accountId}/{blobId}/{name}?type={type}",
                "eventSourceUrl": "events?types={types}&closeafter={closeafter}&ping={ping}"
            })
            .to_string(),
        ),
    ])
    .await;
    let transport = transport::Transport::new(&base, "user", "secret").unwrap();
    let session = transport
        .discover(&format!("{base}/.well-known/jmap"))
        .await
        .unwrap();
    assert_eq!(session["apiUrl"], format!("{base}/proxy/api"));
    assert_eq!(
        session["uploadUrl"],
        format!("{base}/proxy/upload/{{accountId}}")
    );
    assert_eq!(
        session["downloadUrl"],
        format!("{base}/proxy/download/{{accountId}}/{{blobId}}/{{name}}?type={{type}}")
    );
    assert_eq!(
        session["eventSourceUrl"],
        format!("{base}/proxy/events?types={{types}}&closeafter={{closeafter}}&ping={{ping}}")
    );
    requests.await.unwrap();
}

#[tokio::test]
async fn jmap_listing_preserves_query_order_when_get_reorders_records() {
    let session = json!({
        "capabilities": {"urn:ietf:params:jmap:core":{}, jmap::CAPABILITY:{}},
        "primaryAccounts": {jmap::CAPABILITY:"mine"},
        "accounts": {"mine":{"isReadOnly":false,"accountCapabilities":{jmap::CAPABILITY:{"fileNodeQuerySortOptions":["name"]}}}},
        "apiUrl":"/jmap", "uploadUrl":"/upload/{accountId}",
        "downloadUrl":"/download/{accountId}/{blobId}/{name}?type={type}"
    });
    let query = json!({"methodResponses":[["FileNode/query",{"accountId":"mine","ids":["b","a"],"total":2,"queryState":"q1"},"files"]]});
    let get = json!({"methodResponses":[["FileNode/get",{"accountId":"mine","list":[{"id":"a","name":"Zulu"},{"id":"b","name":"Alpha"}],"state":"s1"},"files"]]});
    let (base, requests) = server(
        [session, query, get]
            .into_iter()
            .map(|body| {
                (
                    200,
                    "Content-Type: application/json\r\n".into(),
                    body.to_string(),
                )
            })
            .collect(),
    )
    .await;
    let mut client = jmap::FileClient::connect(&base, "user", "secret", None)
        .await
        .unwrap();
    let page = client.list(None, "", 0).await.unwrap();
    assert_eq!(
        page.nodes
            .iter()
            .map(|n| n.name.as_str())
            .collect::<Vec<_>>(),
        ["Alpha", "Zulu"]
    );
    assert!(page.next_position.is_none());
    requests.await.unwrap();
}

#[test]
fn jmap_get_rejects_unrequested_and_duplicate_records() {
    let ids = vec!["b".to_owned(), "a".to_owned(), "deleted".to_owned()];
    let ordered = |records: Vec<String>| transport::ordered_records(&ids, records, String::as_str);
    assert_eq!(ordered(vec!["a".into(), "b".into()]).unwrap(), ["b", "a"]);
    assert!(ordered(vec!["unexpected".into()]).is_err());
    assert!(ordered(vec!["a".into(), "a".into()]).is_err());
}

#[test]
fn stalwart_directory_has_nullable_file_properties() {
    let node:FileNode=serde_json::from_value(json!({"id":"folder","name":"Documents","parentId":null,"nodeType":"directory","blobId":null,"size":null,"type":null,"executable":null,"myRights":{"mayRead":true,"mayAddChildren":true}})).unwrap();
    assert!(node.is_directory());
    assert!(!node.executable);
    assert!(node.my_rights.add());
}

#[tokio::test]
async fn dav_replacement_requires_both_destination_etag_and_owned_lock() {
    let listing = r#"<d:multistatus xmlns:d="DAV:"><d:response><d:href>/dav/file/me/</d:href><d:propstat><d:prop><d:resourcetype><d:collection/></d:resourcetype></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response><d:response><d:href>/dav/file/me/destination.txt</d:href><d:propstat><d:prop><d:resourcetype/><d:getetag>"target-v1"</d:getetag><d:displayname>destination.txt</d:displayname></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response></d:multistatus>"#;
    let (base, requests) = server(vec![
        (200, "DAV: 1, 2\r\n".into(), String::new()),
        (
            200,
            "Lock-Token: <opaquelocktoken:source>\r\n".into(),
            String::new(),
        ),
        (
            200,
            "Lock-Token: <opaquelocktoken:owned>\r\n".into(),
            String::new(),
        ),
        (207, String::new(), listing.into()),
        (204, String::new(), String::new()),
    ])
    .await;
    let mut client = dav::DavClient::connect(&format!("{base}/dav/file/me/"), "user", "secret")
        .await
        .unwrap();
    let source = FileNode {
        id: format!("{base}/dav/file/me/source.txt"),
        name: "source.txt".into(),
        etag: Some("\"source-v1\"".into()),
        node_type: Some("file".into()),
        ..Default::default()
    };
    let target = FileNode {
        id: format!("{base}/dav/file/me/destination.txt"),
        name: "destination.txt".into(),
        etag: Some("\"target-v1\"".into()),
        node_type: Some("file".into()),
        ..Default::default()
    };
    client.lock(&source).await.unwrap();
    client.lock(&target).await.unwrap();
    client
        .relocate_with_policy(
            &source,
            &client.root,
            "destination.txt",
            true,
            CollisionPolicy::Replace,
        )
        .await
        .unwrap();
    let requests = requests.await.unwrap();
    let request = requests[4].to_lowercase();
    assert_eq!(
        request
            .lines()
            .filter(|line| line.starts_with("if:"))
            .count(),
        1
    );
    assert!(request.contains("(<opaquelocktoken:source>)"));
    assert!(request.contains("overwrite: t\r\n"));
    assert!(request.contains("if-match: \"source-v1\"\r\n"));
    assert!(
        request.contains("([\"target-v1\"] <opaquelocktoken:owned>)"),
        "ETag and lock must be AND, not OR: {request}"
    );
}

#[tokio::test]
async fn streamed_upload_pins_length_and_reports_progress_across_tasks() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("source.txt");
    let original = "bounded-data".repeat(10_000);
    tokio::fs::write(&path, &original).await.unwrap();
    let observer = std::sync::Arc::new(progress::Progress::default());
    let (body, length) = progress::track(observer.clone(), upload_body(&path, MAX_TRANSFER))
        .await
        .unwrap();
    use tokio::io::AsyncWriteExt;
    tokio::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .await
        .unwrap()
        .write_all(b"not-in-selected-length")
        .await
        .unwrap();
    let (base, requests) = server(vec![(201, String::new(), String::new())]).await;
    tokio::spawn(async move {
        reqwest::Client::new()
            .put(base)
            .header("Content-Length", length)
            .body(body)
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap()
    })
    .await
    .unwrap();
    let request = requests.await.unwrap().pop().unwrap();
    assert!(request.ends_with(&original));
    assert!(!request.contains("not-in-selected-length"));
    let observed = observer.snapshot();
    assert_eq!(observed.done, length);
    assert_eq!(observed.total, length);
    assert_eq!(observed.phase, 1);
}
