//! Opt-in interoperability against a disposable Stalwart account. Only creates
//! and removes resources under a unique test folder. Never point at production.
use flectar_mail_core::files::{dav::DavClient, jmap::FileClient};
use serde_json::json;

#[tokio::test]
#[ignore = "requires a disposable local Stalwart server; see .devcontainer/docs/pending-changes/files.md"]
async fn stalwart_file_round_trip() {
    let base = std::env::var("FLECTAR_FILES_TEST_URL").unwrap();
    assert!(
        base.starts_with("http://127.0.0.1:"),
        "Only a disposable loopback server is allowed"
    );
    let user = std::env::var("FLECTAR_FILES_TEST_USER").unwrap();
    let secret = std::env::var("FLECTAR_FILES_TEST_PASSWORD").unwrap();
    let mut jmap = FileClient::connect(&base, &user, &secret, None)
        .await
        .unwrap();
    let root = jmap.list(None, "", 0).await.unwrap();
    let parent = root
        .nodes
        .iter()
        .find(|n| n.role.as_deref() == Some("home"))
        .map(|n| n.id.clone());
    let folder = format!("flectar-test-{}", chrono::Utc::now().timestamp_millis());
    let created = jmap
        .create_folder(parent.as_deref(), &folder)
        .await
        .unwrap();
    let folder_id = created["created"]["new"]["id"].as_str().unwrap().to_owned();
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("résumé & budget.txt");
    std::fs::write(&source, b"Hello from Flectar").unwrap();
    jmap.upload(Some(&folder_id), "résumé & budget.txt", &source, None)
        .await
        .unwrap();
    let listing = jmap.list(Some(&folder_id), "", 0).await.unwrap();
    assert_eq!(listing.nodes.len(), 1);
    let node = listing.nodes[0].clone();
    assert_eq!(node.media_type.as_deref(), Some("text/plain"));
    assert_eq!(jmap.download(&node).await.unwrap(), b"Hello from Flectar");
    let downloaded = temp.path().join("jmap-download.txt");
    jmap.download_to(&node, &downloaded).await.unwrap();
    assert_eq!(std::fs::read(&downloaded).unwrap(), b"Hello from Flectar");
    assert!(jmap.download_to(&node, &downloaded).await.is_err());
    jmap.update(&node.id, json!({"name":"renamed.txt"}))
        .await
        .unwrap();
    jmap.copy(&node.id, Some(&folder_id), "copy.txt")
        .await
        .unwrap();
    let listing = jmap.list(Some(&folder_id), "", 0).await.unwrap();
    assert_eq!(listing.nodes.len(), 2);
    let dav_root = std::env::var("FLECTAR_FILES_TEST_DAV").unwrap();
    let mut dav = DavClient::connect(&dav_root, &user, &secret).await.unwrap();
    let listing = dav.list(&dav.root).await.unwrap();
    let dir = listing
        .nodes
        .iter()
        .find(|n| n.name == folder)
        .expect("JMAP-created folder must be visible over DAV")
        .clone();
    let listing = dav.list(&dir.id).await.unwrap();
    assert_eq!(listing.nodes.len(), 2);
    let file = listing
        .nodes
        .iter()
        .find(|n| n.name == "renamed.txt")
        .unwrap()
        .clone();
    assert_eq!(dav.download(&file).await.unwrap(), b"Hello from Flectar");
    dav.lock(&file).await.unwrap();
    dav.patch_property(
        &file,
        "urn:flectar:files",
        "description",
        Some("Test & value"),
    )
    .await
    .unwrap();
    dav.unlock(&file).await.unwrap();
    assert!(
        dav.relocate(&file, &dir.id, "stale.txt", false)
            .await
            .is_err(),
        "metadata edit changes the ETag"
    );
    let refreshed = dav.list(&dir.id).await.unwrap();
    let file = refreshed
        .nodes
        .iter()
        .find(|n| n.name == "renamed.txt")
        .unwrap();
    dav.relocate(file, &dir.id, "via-dav.txt", false)
        .await
        .unwrap();
    dav.create_folder(&dir.id, "nested").await.unwrap();
    let nested = dav.child_url(&dir.id, "nested", true).unwrap();
    dav.upload(&nested, "uploaded.txt", &source, None)
        .await
        .unwrap();
    let listing = dav.list(&nested).await.unwrap();
    assert_eq!(listing.nodes.len(), 1);
    dav.delete(&listing.nodes[0]).await.unwrap();
    let listing = jmap.list(Some(&folder_id), "", 0).await.unwrap();
    assert!(listing.nodes.iter().any(|n| n.name == "via-dav.txt"));
    // Discover the recipient ID: account IDs depend on provisioning order.
    let bob_own_account = FileClient::connect(&base, "bob@files.test", &secret, None)
        .await
        .unwrap()
        .account_id;
    let recipient_path = format!(
        "shareWith/{}",
        bob_own_account.replace('~', "~0").replace('/', "~1")
    );
    // Share with the second disposable account and verify the returned rights.
    jmap.update(&folder_id,json!({recipient_path.clone():{"mayRead":true,"mayAddChildren":false,"mayRename":false,"mayDelete":false,"mayModifyContent":false,"mayShare":false}})).await.unwrap();
    let mut bob = FileClient::connect(&base, "bob@files.test", &secret, None)
        .await
        .unwrap();
    assert!(
        bob.accounts.iter().any(|a| a.id == jmap.account_id),
        "shared account discovery"
    );
    bob.select_account(&jmap.account_id).unwrap();
    let shared = bob.list(Some(&folder_id), "", 0).await.unwrap();
    assert!(!shared.nodes.is_empty());
    assert!(
        shared
            .nodes
            .iter()
            .all(|n| n.my_rights.may_read && !n.my_rights.modify())
    );
    assert!(
        bob.create_folder(Some(&folder_id), "forbidden")
            .await
            .is_err()
    );
    let notifications = bob.share_notifications().await.unwrap();
    let ids = notifications["list"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|n| n["objectId"] == folder_id)
        .filter_map(|n| n["id"].as_str().map(str::to_owned))
        .collect::<Vec<_>>();
    if !ids.is_empty() {
        bob.dismiss_share_notifications(&ids, notifications["state"].as_str().unwrap())
            .await
            .unwrap();
    }
    let bob_dav = DavClient::connect(&dav_root, "bob@files.test", &secret)
        .await
        .unwrap();
    let shared = bob_dav.list(&dir.id).await.unwrap();
    assert!(!shared.current.my_rights.add());
    // Stalwart 0.16.21 DAV does not expose inherited child grants. Apply a
    // direct grant to exercise DAV sharing independently of that limitation.
    jmap.list(Some(&folder_id), "", 0).await.unwrap();
    jmap.update(&node.id, json!({recipient_path:{"mayRead":true}}))
        .await
        .unwrap();
    let shared = bob_dav.list(&dir.id).await.unwrap();
    let file = shared
        .nodes
        .iter()
        .find(|n| n.name == "via-dav.txt")
        .unwrap();
    assert_eq!(bob_dav.download(file).await.unwrap(), b"Hello from Flectar");
    let shared_state = bob.list(Some(&folder_id), "", 0).await.unwrap().state;
    bob.select_account(&bob_own_account).unwrap();
    bob.list(None, "", 0).await.unwrap();
    let copied = bob
        .copy_from_account(
            &jmap.account_id,
            &shared_state,
            &node.id,
            None,
            &format!("{folder}.txt"),
        )
        .await
        .unwrap();
    let copied_id = copied["created"][&node.id]["id"].as_str().unwrap();
    bob.delete(copied_id, false).await.unwrap();
    let copied_tree = bob
        .copy_from_account(
            &jmap.account_id,
            &shared_state,
            &folder_id,
            None,
            &format!("{folder}-tree"),
        )
        .await
        .unwrap();
    assert_eq!(copied_tree["created"].as_object().unwrap().len(), 4);
    let copied_folder_id = copied_tree["created"][&folder_id]["id"].as_str().unwrap();
    let children = bob.list(Some(copied_folder_id), "", 0).await.unwrap();
    assert_eq!(children.nodes.len(), 3);
    let copied_file = children
        .nodes
        .iter()
        .find(|n| n.name == "via-dav.txt")
        .unwrap();
    assert_eq!(
        bob.download(copied_file).await.unwrap(),
        b"Hello from Flectar"
    );
    bob.delete(copied_folder_id, true).await.unwrap();
    let refreshed_file = dav
        .list(&dir.id)
        .await
        .unwrap()
        .nodes
        .into_iter()
        .find(|n| n.name == "via-dav.txt")
        .unwrap();
    dav.share(&refreshed_file, "/dav/pal/bob@files.test/", false, true)
        .await
        .unwrap();
    let refreshed = dav.list(&dav.root).await.unwrap();
    let shared_folder = refreshed.nodes.iter().find(|n| n.name == folder).unwrap();
    dav.share(shared_folder, "/dav/pal/bob@files.test/", false, true)
        .await
        .unwrap();
    assert!(
        bob_dav.list(&dir.id).await.is_err(),
        "revoked access is enforced"
    );
    jmap.list(None, "", 0).await.unwrap();
    let changes = jmap.changes(&root.state).await.unwrap();
    assert!(changes["created"].as_array().unwrap().len() >= 3);
    // A locked destination may be replaced only with both validators and
    // the owned lock in the same DAV condition list.
    dav.upload(&dir.id, "replace-target.txt", &source, None)
        .await
        .unwrap();
    let files = dav.list(&dir.id).await.unwrap().nodes;
    let source_node = files.iter().find(|n| n.name == "via-dav.txt").unwrap();
    let target = files
        .iter()
        .find(|n| n.name == "replace-target.txt")
        .unwrap();
    dav.lock(target).await.unwrap();
    dav.relocate_with_policy(
        source_node,
        &dir.id,
        "replace-target.txt",
        true,
        flectar_mail_core::files::CollisionPolicy::Replace,
    )
    .await
    .unwrap();
    let files = dav.list(&dir.id).await.unwrap().nodes;
    let target = files
        .iter()
        .find(|n| n.name == "replace-target.txt")
        .unwrap();
    dav.unlock(target).await.unwrap();
    jmap.list(None, "", 0).await.unwrap();
    let dates=jmap.set(json!({"create":{
        "older":{"name":"older-source.txt","parentId":folder_id,"blobId":node.blob_id,"modified":"2000-01-01T00:00:00Z"},
        "newer":{"name":"newer-target.txt","parentId":folder_id,"blobId":node.blob_id,"modified":"2020-01-01T00:00:00Z"}
    }})).await.unwrap();
    let older = dates["created"]["older"]["id"].as_str().unwrap();
    let newer = dates["created"]["newer"]["id"].as_str().unwrap();
    jmap.collision = flectar_mail_core::files::CollisionPolicy::Newest;
    assert!(
        jmap.copy(older, Some(&folder_id), "newer-target.txt")
            .await
            .is_err(),
        "Keep newest must preserve the newer destination"
    );
    assert_eq!(
        jmap.get(&[newer.into()]).await.unwrap()["list"][0]["modified"],
        "2020-01-01T00:00:00Z"
    );
    jmap.collision = flectar_mail_core::files::CollisionPolicy::Reject;
    jmap.list(None, "", 0).await.unwrap();
    let result = jmap.delete(&folder_id, false).await;
    assert!(result.is_err(), "nonrecursive delete must reject children");
    jmap.list(None, "", 0).await.unwrap();
    jmap.delete(&folder_id, true).await.unwrap();
    assert!(dav.list(&dir.id).await.is_err());
}

struct TestSecrets(String);
impl flectar_mail_core::accounts::credentials::CredentialStore for TestSecrets {
    fn store(
        &self,
        _: i64,
        _: flectar_mail_core::accounts::credentials::Slot,
        _: &str,
    ) -> flectar_mail_core::error::Result<()> {
        Ok(())
    }
    fn load(
        &self,
        _: i64,
        _: flectar_mail_core::accounts::credentials::Slot,
    ) -> flectar_mail_core::error::Result<String> {
        Ok(self.0.clone())
    }
    fn delete(
        &self,
        _: i64,
        _: flectar_mail_core::accounts::credentials::Slot,
    ) -> flectar_mail_core::error::Result<()> {
        Ok(())
    }
    fn delete_all(&self, _: i64) -> flectar_mail_core::error::Result<()> {
        Ok(())
    }
}
#[tokio::test]
#[ignore = "requires a disposable local Stalwart server; see .devcontainer/docs/pending-changes/files.md"]
async fn stalwart_durable_sync_and_offline_queue() {
    use flectar_mail_core::{
        Core,
        config::Paths,
        files::{
            self, ConnectionSettings,
            service::{Entry, FilesService},
        },
    };
    let base = std::env::var("FLECTAR_FILES_TEST_URL").unwrap();
    assert!(base.starts_with("http://127.0.0.1:"));
    let user = std::env::var("FLECTAR_FILES_TEST_USER").unwrap();
    let secret = std::env::var("FLECTAR_FILES_TEST_PASSWORD").unwrap();
    let root = tempfile::tempdir().unwrap();
    let core = Core::start_mail_ui_with_credentials(
        Paths::for_tests(root.path()),
        std::sync::Arc::new(TestSecrets(secret.clone())),
    )
    .await
    .unwrap();
    let email = user.clone();
    let endpoint = base.clone();
    core.db.write(move|c|{c.execute("INSERT INTO accounts(id,email,provider,auth_kind,username,imap_host,imap_port,smtp_host,smtp_port,created_at,mail_protocol,jmap_url) VALUES(1,?1,'imap','password',?1,'localhost',993,'localhost',587,0,'jmap',?2)",rusqlite::params![email,endpoint])?;Ok(())}).await.unwrap();
    core.save_file_connection_settings(
        1,
        ConnectionSettings {
            endpoint: base.clone(),
            webdav: false,
        },
    )
    .await
    .unwrap();
    let mut remote = FileClient::connect(&base, &user, &secret, None)
        .await
        .unwrap();
    remote.list(None, "", 0).await.unwrap();
    let name = format!("durable-{}", chrono::Utc::now().timestamp_millis());
    let result = remote.create_folder(None, &name).await.unwrap();
    let folder = result["created"]["new"]["id"].as_str().unwrap().to_owned();
    let source = root.path().join("queued.txt");
    std::fs::write(&source, b"durable queue").unwrap();
    let push_client = remote.clone();
    let push = tokio::spawn(async move {
        tokio::time::timeout(
            std::time::Duration::from_secs(35),
            push_client.wait_for_change(),
        )
        .await
    });
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    remote
        .upload(Some(&folder), "initial.txt", &source, None)
        .await
        .unwrap();
    push.await
        .unwrap()
        .expect("File push timed out")
        .expect("File push rejected");
    files::sync::account(&core, 1).await.unwrap();
    let mut service = FilesService {
        attachments: true,
        ..Default::default()
    };
    service
        .execute(&core, "load", "", "", -1, None)
        .await
        .unwrap();
    service
        .execute(&core, "account", "1", "", -1, None)
        .await
        .unwrap();
    // The primary Connect storage action is available from attachment scope.
    assert!(service.attachments);
    service
        .execute(&core, "connect", &base, "jmap", -1, None)
        .await
        .unwrap();
    assert!(!service.attachments);
    let index = service
        .entries
        .iter()
        .position(|n| matches!(n,Entry::Remote(n) if n.id==folder))
        .unwrap() as i32;
    service
        .execute(&core, "enter", "", "", index, None)
        .await
        .unwrap();
    // Simulate discovery being unavailable without changing the saved endpoint.
    let closed = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    service.settings.endpoint = format!("http://{}", closed.local_addr().unwrap());
    drop(closed);
    service.client = None;
    service
        .execute(&core, "refresh", "", "", -1, None)
        .await
        .unwrap();
    assert!(service.offline);
    assert_eq!(service.parent(), Some(folder.clone()));
    assert_eq!(
        service.entries.len(),
        1,
        "offline folder history must survive reconnect failure"
    );
    service.settings.endpoint = base.clone();
    service
        .execute(&core, "upload", "", "", -1, Some(source))
        .await
        .unwrap();
    let jobs = files::store::operations(&core.files_db, 1).await.unwrap();
    let job = &jobs[0];
    assert_eq!(job.state, "queued");
    service
        .execute(&core, "retry-operation", &job.id.to_string(), "", -1, None)
        .await
        .unwrap();
    assert_eq!(
        files::store::operations(&core.files_db, 1).await.unwrap()[0].state,
        "completed"
    );
    files::sync::account(&core, 1).await.unwrap();
    let space = service.cached_space.unwrap();
    let nodes = files::store::cached(&core.files_db, space, Some(folder.clone()), "".into())
        .await
        .unwrap();
    assert_eq!(nodes.len(), 2);
    let filtered = remote.list(Some(&folder), "queued", 0).await.unwrap();
    assert_eq!(
        filtered.nodes.len(),
        1,
        "Stalwart filename search must filter results"
    );
    let node = filtered.nodes[0].clone();
    let client = files::FileClient::Jmap(remote.clone());
    let cached = files::cache::content(&core, 1, space, &node, Some(&client), false)
        .await
        .unwrap();
    assert_eq!(std::fs::read(&cached).unwrap(), b"durable queue");
    assert_eq!(
        files::cache::content(&core, 1, space, &node, None, true)
            .await
            .unwrap(),
        cached
    );
    remote.delete(&node.id, false).await.unwrap();
    files::sync::account(&core, 1).await.unwrap();
    assert!(!cached.exists());
    assert_eq!(
        files::store::cached(&core.files_db, space, Some(folder.clone()), "".into())
            .await
            .unwrap()
            .len(),
        1
    );
    // Same database, explicit adapter switch, independent DAV collection cursors.
    let dav = std::env::var("FLECTAR_FILES_TEST_DAV").unwrap();
    core.save_file_connection_settings(
        1,
        ConnectionSettings {
            endpoint: dav,
            webdav: true,
        },
    )
    .await
    .unwrap();
    files::sync::account(&core, 1).await.unwrap();
    files::sync::account(&core, 1).await.unwrap();
    let count = core
        .files_db
        .read(|c| {
            Ok(c.query_row(
                "SELECT count(*) FROM nodes WHERE name='initial.txt'",
                [],
                |r| r.get::<_, i64>(0),
            )?)
        })
        .await
        .unwrap();
    assert!(count > 0);
    remote.list(None, "", 0).await.unwrap();
    remote.delete(&folder, true).await.unwrap();
    check_server_attachment_search(&core, &base, &user, &secret).await;
}

async fn mail_call(
    http: &reqwest::Client,
    base: &str,
    user: &str,
    secret: &str,
    method: &str,
    args: serde_json::Value,
) -> serde_json::Value {
    let response:serde_json::Value=http.post(format!("{base}/jmap/" )).basic_auth(user,Some(secret)).json(&json!({"using":["urn:ietf:params:jmap:core","urn:ietf:params:jmap:mail"],"methodCalls":[[method,args,"test"]]})).send().await.unwrap().json().await.unwrap();
    assert_eq!(response["methodResponses"][0][0], method, "{response}");
    response["methodResponses"][0][1].clone()
}
async fn check_server_attachment_search(
    core: &flectar_mail_core::Core,
    base: &str,
    user: &str,
    secret: &str,
) {
    let http = reqwest::Client::new();
    let session: serde_json::Value = http
        .get(format!("{base}/jmap/session"))
        .basic_auth(user, Some(secret))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let account = session["primaryAccounts"]["urn:ietf:params:jmap:mail"]
        .as_str()
        .unwrap();
    let mailbox=mail_call(&http,base,user,secret,"Mailbox/set",json!({"accountId":account,"create":{"new":{"name":format!("Attachment test {}",chrono::Utc::now().timestamp_millis())}}})).await;
    let mailbox = mailbox["created"]["new"]["id"].as_str().unwrap();
    let subject = format!("flectarsearch{}", chrono::Utc::now().timestamp_millis());
    let mime = format!(
        "From: sender@files.test\r\nTo: {user}\r\nSubject: {subject}\r\nMIME-Version: 1.0\r\nContent-Type: multipart/mixed; boundary=filesboundary\r\n\r\n--filesboundary\r\nContent-Type: text/plain\r\n\r\nMail body\r\n--filesboundary\r\nContent-Type: text/plain; name=attachment.txt\r\nContent-Disposition: attachment; filename=attachment.txt\r\n\r\nquasarmetadata text\r\n--filesboundary--\r\n"
    );
    let upload: serde_json::Value = http
        .post(format!("{base}/jmap/upload/{account}/"))
        .basic_auth(user, Some(secret))
        .header("Content-Type", "message/rfc822")
        .body(mime)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let imported=mail_call(&http,base,user,secret,"Email/import",json!({"accountId":account,"emails":{"new":{"blobId":upload["blobId"],"mailboxIds":{mailbox:true},"keywords":{},"receivedAt":"2000-01-01T00:00:00Z"}}})).await;
    let email = imported["created"]["new"]["id"].as_str().unwrap();
    let client = core.connect_attachment_search(1).await.unwrap();
    let page = client.search(&subject, 0).await.unwrap();
    assert_eq!(page.files.len(), 1);
    assert_eq!(page.files[0].filename, "attachment.txt");
    assert!(page.files[0].date < 1_000_000_000_000);
    let path = core.attachment_file_content(&page.files[0]).await.unwrap();
    assert!(
        String::from_utf8(std::fs::read(path).unwrap())
            .unwrap()
            .contains("quasarmetadata")
    );
    let fetched = mail_call(
        &http,
        base,
        user,
        secret,
        "Email/get",
        json!({"accountId":account,"ids":[email],"properties":["keywords"]}),
    )
    .await;
    assert!(
        fetched["list"][0]["keywords"].get("$seen").is_none(),
        "file access must not mark email read"
    );
    let count = core
        .db
        .read(|c| Ok(c.query_row("SELECT count(*) FROM messages", [], |r| r.get::<_, i64>(0))?))
        .await
        .unwrap();
    assert_eq!(
        count, 0,
        "remote attachment search must not change Mail's history window"
    );
    mail_call(
        &http,
        base,
        user,
        secret,
        "Email/set",
        json!({"accountId":account,"destroy":[email]}),
    )
    .await;
    mail_call(
        &http,
        base,
        user,
        secret,
        "Mailbox/set",
        json!({"accountId":account,"destroy":[mailbox]}),
    )
    .await;
}
