//! Explicitly requested, debug-only local data for the original Slint/Blitz UI.
//! The host owns the permanently closed platform gate and the private profile;
//! this module never starts a worker, reads Asset, or performs a network call.

use flectar_mail::flectar_mail_core::{
    Core,
    accounts::credentials::{CredentialStore, Slot},
    config::Paths,
    db::repo,
    error::{CoreError, Result as CoreResult},
    mail_security::OpenPgpKeyStore,
    models::{Address, AuthKind, MailProtocol, Provider, SenderVerification},
};
use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::Path,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

const PROFILE: &str = "debug-ui-fixture";
const MARKER: &str = "feihong-offline-ui-fixture-v1\n";
const ACCOUNT: &str = "ui-fixture@example.invalid";
const NAME: &str = "[UI验收数据] 飞鸿离线测试";
const NO_SECRETS: &str = "离线 UI 验收模式不接受账号凭据或私钥。";

/// This store has no reference to the system store, including account id 1.
#[derive(Default)]
pub struct OfflineStore;

impl CredentialStore for OfflineStore {
    fn store(&self, _: i64, _: Slot, _: &str) -> CoreResult<()> {
        Err(CoreError::CredentialStoreUnavailable(NO_SECRETS.into()))
    }
    fn load(&self, _: i64, _: Slot) -> CoreResult<String> {
        Err(CoreError::NeedsReauth)
    }
    fn delete(&self, _: i64, _: Slot) -> CoreResult<()> {
        Ok(())
    }
    fn delete_all(&self, _: i64) -> CoreResult<()> {
        Ok(())
    }
    fn check_available(&self) -> CoreResult<()> {
        Err(CoreError::CredentialStoreUnavailable(NO_SECRETS.into()))
    }
}

impl OpenPgpKeyStore for OfflineStore {
    fn load(&self, _: &str) -> CoreResult<Option<Vec<u8>>> {
        Ok(None)
    }
    fn store(&self, _: &str, _: &[u8]) -> CoreResult<()> {
        Err(CoreError::Other(NO_SECRETS.into()))
    }
    fn delete(&self, _: &str) -> CoreResult<()> {
        Ok(())
    }
    fn list(&self) -> CoreResult<Vec<String>> {
        Ok(Vec::new())
    }
}

/// Seed only a host-marked, empty private profile. Repeated calls keep existing
/// UI interactions and enforce the three external-content settings again.
/// No existing account, message, attachment, or raw file is deleted or replaced.
pub async fn seed(core: &Arc<Core>, paths: &Paths) -> Result<(), String> {
    if !cfg!(debug_assertions) {
        return Err("Offline UI acceptance data is available only in debug builds".into());
    }
    if core.platform_is_active() {
        return Err("Offline UI fixture requires the platform activity gate to be closed".into());
    }
    let root = paths
        .data_dir
        .parent()
        .ok_or("Offline fixture has no profile directory")?;
    if !paths.data_dir.is_absolute()
        || !paths.cache_dir.is_absolute()
        || paths.data_dir.file_name().and_then(|s| s.to_str()) != Some("data")
        || root.file_name().and_then(|s| s.to_str()) != Some(PROFILE)
        || paths.cache_dir.file_name().and_then(|s| s.to_str()) != Some(PROFILE)
        || root.is_symlink()
        || paths.data_dir.is_symlink()
        || paths.cache_dir.is_symlink()
    {
        return Err("Offline fixture requires its independent private data/cache profile".into());
    }
    let marker = root.join(".fixture-profile");
    if marker.is_symlink() || fs::read(&marker).map_err(|e| e.to_string())? != MARKER.as_bytes() {
        return Err("Refusing to seed an unmarked offline fixture profile".into());
    }
    let expected = fs::canonicalize(paths.db_file()).map_err(|e| e.to_string())?;
    let ready = core
        .db
        .read(move |conn| {
            let actual: String = conn.query_row("PRAGMA database_list", [], |row| row.get(2))?;
            if fs::canonicalize(actual)? != expected {
                return Err(CoreError::Other(
                    "Fixture Core database does not match its private profile".into(),
                ));
            }
            let foreign: i64 = conn.query_row(
                "SELECT COUNT(*) FROM accounts WHERE email <> ?1",
                [ACCOUNT],
                |row| row.get(0),
            )?;
            if foreign != 0 {
                return Err(CoreError::Other(
                    "Refusing to modify a profile containing non-fixture accounts".into(),
                ));
            }
            let owner: Option<String> = conn.query_row(
                "SELECT (SELECT value FROM app_settings WHERE key = 'debug-ui-fixture-owner')",
                [],
                |row| row.get(0),
            )?;
            if let Some(owner) = owner {
                if owner != MARKER {
                    return Err(CoreError::Other(
                        "Unknown offline fixture database marker".into(),
                    ));
                }
                return Ok(true);
            }
            for table in [
                "accounts",
                "folders",
                "threads",
                "messages",
                "message_bodies",
                "attachments",
                "contacts",
                "drafts_meta",
                "draft_attachments",
                "pending_actions",
                "app_settings",
                "snippets",
                "split_rules",
                "cross_store_operations",
            ] {
                let count: i64 =
                    conn.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                        row.get(0)
                    })?;
                if count != 0 {
                    return Err(CoreError::Other(format!(
                        "Refusing to seed nonempty fixture table {table}"
                    )));
                }
            }
            Ok(false)
        })
        .await
        .map_err(|e| e.to_string())?;
    if ready {
        return core
            .db
            .write(|conn| {
                let mut settings = repo::settings::get(conn)?;
                settings.load_remote_images = false;
                settings.show_avatars = false;
                settings.notifications_enabled = false;
                repo::settings::set(conn, &settings)
            })
            .await
            .map_err(|e| e.to_string());
    }
    let assets = paths.data_dir.join("acceptance-assets");
    if assets.is_symlink() {
        return Err("Offline fixture assets directory is a symbolic link".into());
    }
    fs::create_dir_all(&assets).map_err(|e| e.to_string())?;
    let pdf = two_page_pdf();
    let pdf_path = assets.join("UI-acceptance-two-pages.pdf");
    write_owned_file(&pdf_path, &pdf)?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_millis()
        .min(i64::MAX as u128) as i64;
    let samples = samples()
        .into_iter()
        .enumerate()
        .map(|(index, mut sample)| {
            let raw = raw_email(index, &sample, &pdf);
            let path = assets.join(format!("ui-acceptance-{}.eml", index + 1));
            write_owned_file(&path, raw.as_bytes())?;
            sample.raw_path = path.to_string_lossy().into_owned();
            sample.date = now - index as i64 * 3_600_000;
            Ok(sample)
        })
        .collect::<Result<Vec<_>, String>>()?;
    let pdf_path = pdf_path.to_string_lossy().into_owned();
    let pdf_size = pdf.len() as i64;
    core.db
        .write(move |conn| {
            let tx = conn.transaction()?;
            // The checked profile is exclusively owned by this process. Recheck
            // the critical boundary in the same transaction as the first insert.
            let count: i64 = tx.query_row("SELECT COUNT(*) FROM accounts", [], |row| row.get(0))?;
            if count != 0 {
                return Err(CoreError::Other(
                    "Fixture profile changed while preparing acceptance data".into(),
                ));
            }
            let account_id = repo::accounts::insert_with_sync_state(
                &tx,
                &repo::accounts::NewAccount {
                    email: ACCOUNT,
                    display_name: Some(NAME),
                    avatar_url: None,
                    provider: Provider::Imap,
                    auth_kind: AuthKind::Password,
                    mail_protocol: MailProtocol::Imap,
                    username: ACCOUNT,
                    jmap_url: "",
                    jmap_account_id: None,
                    imap_host: "0.0.0.0",
                    imap_port: 0,
                    smtp_host: "0.0.0.0",
                    smtp_port: 0,
                },
                "offline",
            )?;
            let inbox = repo::folders::upsert(&tx, account_id, "INBOX", Some("/"), Some("inbox"))?;
            for (name, role) in [
                ("Sent", "sent"),
                ("Drafts", "drafts"),
                ("Archive", "archive"),
                ("Trash", "trash"),
            ] {
                repo::folders::upsert(&tx, account_id, name, Some("/"), Some(role))?;
            }
            let mut previous_thread = None;
            for (index, sample) in samples.iter().enumerate() {
                let thread = if sample.reply {
                    previous_thread
                        .ok_or_else(|| CoreError::Other("Fixture reply has no parent".into()))?
                } else {
                    repo::threads::create(&tx, account_id, None, sample.subject)?
                };
                previous_thread = Some(thread);
                let id = repo::messages::insert(
                    &tx,
                    &repo::messages::NewMessage {
                        account_id,
                        folder_id: inbox,
                        uid: Some(index as i64 + 1),
                        message_id: Some(format!("<ui-acceptance-{index}@example.invalid>")),
                        gm_msgid: None,
                        gm_thrid: None,
                        subject: sample.subject.into(),
                        from: Some(Address {
                            name: Some(sample.sender.into()),
                            email: "acceptance@example.invalid".into(),
                        }),
                        to: vec![Address {
                            name: Some(NAME.into()),
                            email: ACCOUNT.into(),
                        }],
                        cc: Vec::new(),
                        bcc: Vec::new(),
                        date: sample.date,
                        internal_date: Some(sample.date),
                        is_read: index % 2 == 1,
                        is_starred: index == 0 || sample.pdf,
                        is_draft: false,
                        is_outgoing: false,
                        is_automated: false,
                        has_attachments: sample.pdf,
                        size: None,
                        snippet: sample.text.into(),
                        references: if sample.reply {
                            vec!["<ui-acceptance-1@example.invalid>".into()]
                        } else {
                            Vec::new()
                        },
                        list_unsubscribe: None,
                        list_unsubscribe_post: None,
                        sender_addr: None,
                        sender_verification: SenderVerification::None,
                    },
                    thread,
                )?;
                // Mailbox list queries use membership, not messages.folder_id.
                tx.execute(
                    "INSERT INTO message_folders (message_id, folder_id) VALUES (?1, ?2)",
                    [id, inbox],
                )?;
                let html = flectar_mail::flectar_mail_core::mime::sanitize_html(sample.html);
                repo::messages::store_body(
                    &tx,
                    id,
                    Some(sample.text),
                    Some(&html),
                    Some(&sample.raw_path),
                    sample.pdf,
                    Some(sample.text),
                )?;
                tx.execute(
                    "UPDATE messages SET embedding_state = 'none' WHERE id = ?1",
                    [id],
                )?;
                if sample.pdf {
                    repo::messages::replace_attachments(
                        &tx,
                        id,
                        &[repo::messages::NewAttachment {
                            message_id: id,
                            part_id: Some("2"),
                            filename: Some("[UI验收数据] 两页测试文档.pdf"),
                            mime_type: Some("application/pdf"),
                            size: Some(pdf_size),
                            content_id: None,
                            is_inline: false,
                        }],
                    )?;
                    tx.execute(
                        "UPDATE attachments SET file_path = ?1 WHERE message_id = ?2",
                        (&pdf_path, id),
                    )?;
                }
                tx.execute(
                    "UPDATE threads SET routed_tab = 'important' WHERE id = ?1",
                    [thread],
                )?;
                repo::threads::recompute(&tx, thread)?;
            }
            let mut settings = repo::settings::get(&tx)?;
            settings.load_remote_images = false;
            settings.show_avatars = false;
            settings.notifications_enabled = false;
            repo::settings::set(&tx, &settings)?;
            tx.execute(
                "INSERT INTO app_settings (key, value) VALUES ('debug-ui-fixture-owner', ?1)",
                [MARKER],
            )?;
            tx.commit()?;
            Ok(())
        })
        .await
        .map_err(|e| e.to_string())?;
    if core.platform_is_active() {
        return Err("Offline fixture platform gate was opened during initialization".into());
    }
    Ok(())
}

fn write_owned_file(path: &Path, content: &[u8]) -> Result<(), String> {
    if path.is_symlink() {
        return Err(format!(
            "Refusing a symbolic-link fixture file: {}",
            path.display()
        ));
    }
    match OpenOptions::new().write(true).create_new(true).open(path) {
        Ok(mut file) => file
            .write_all(content)
            .and_then(|()| file.sync_all())
            .map_err(|e| e.to_string()),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            if fs::read(path).map_err(|e| e.to_string())? == content {
                Ok(())
            } else {
                Err(format!(
                    "Refusing to replace existing fixture file: {}",
                    path.display()
                ))
            }
        }
        Err(error) => Err(error.to_string()),
    }
}

struct Sample {
    subject: &'static str,
    sender: &'static str,
    text: &'static str,
    html: &'static str,
    pdf: bool,
    reply: bool,
    date: i64,
    raw_path: String,
}
fn samples() -> Vec<Sample> {
    [
        (
            "[UI验收数据] 飞鸿邮箱欢迎信 ✉️ 🕊️",
            "验收工作组",
            "这是本地 UI 验收数据，未连接任何邮箱。欢迎体验中文、emoji 与原邮件界面。",
            WELCOME,
            false,
            false,
        ),
        (
            "[UI验收数据] 中文排版讨论：第一封 👋",
            "小飞 👋",
            "请检查中文正文、链接式引用、emoji 与会话展开。所有内容仅用于离线 UI 验收。",
            DISCUSSION,
            false,
            false,
        ),
        (
            "[UI验收数据] 回复：中文排版讨论 😊",
            "小鸿 😊",
            "已收到！这是同一会话的第二封本地邮件，用于验证原有会话视图。",
            REPLY,
            false,
            true,
        ),
        (
            "[UI验收数据] 复杂 HTML 周报与两页 PDF 📎",
            "排版验收小组",
            "表格、颜色、列表、代码块和本地两页 PDF 附件；请在原 Blitz 阅读器中查看。",
            NEWSLETTER,
            true,
            false,
        ),
        (
            "[UI验收数据] 这是一封很长的中文标题，用于验证窄屏省略、横竖屏切换与多设备布局 👨‍👩‍👧‍👦",
            "多设备验收组",
            "平板、手机与 PC 使用相同的 Rust 和 Slint 邮件界面。中文长句应自动换行，emoji 应保留。",
            LONGTEXT,
            false,
            false,
        ),
        (
            "[UI验收数据] 点击、滚动与键盘检查清单 ✅",
            "交互验收组",
            "检查列表选择、阅读滚动、会话展开、星标与已读状态。此账号不能收发或保存凭据。",
            CHECKLIST,
            false,
            false,
        ),
    ]
    .into_iter()
    .map(|(subject, sender, text, html, pdf, reply)| Sample {
        subject,
        sender,
        text,
        html,
        pdf,
        reply,
        date: 0,
        raw_path: String::new(),
    })
    .collect()
}
const WELCOME: &str = r#"<html><head><meta charset="utf-8"></head><body>
<h1>飞鸿邮箱 ✉️ 🕊️</h1><p><strong>UI 验收数据 · 完全离线</strong></p>
<p>这里展示原有 Slint 邮件列表和 Blitz HTML 阅读器。没有连接真实邮箱，也没有账号密码。</p>
<p>中文标点：你好，世界！Emoji：😊 ✅ 📎 🚀 👨‍👩‍👧‍👦</p>
<blockquote>请先验证字体、选择邮件、滚动正文以及附件预览。</blockquote></body></html>"#;
const DISCUSSION: &str = r#"<html><body><h2>中文排版讨论 👋</h2>
<p><strong>UI 验收数据</strong>：这一封邮件和下一封回复属于同一个会话。</p>
<p>测试中文、Latin text、数字 0123456789 与 emoji 😊 混合换行。</p>
<blockquote>约定：设备上的阅读界面始终使用原 Slint 与 Blitz 实现。</blockquote></body></html>"#;
const REPLY: &str = r#"<html><body><p><strong>UI 验收数据 · 本地回复 😊</strong></p>
<p>收到，继续检查会话展开与长段落排版。</p>
<blockquote><p>小飞写道：测试中文、Latin text 与 emoji 混合换行。</p></blockquote>
<p>这是离线构造的会话，没有发送到任何人。</p></body></html>"#;
const NEWSLETTER: &str = r#"<html><head><meta charset="utf-8"><style>
body{font-family:sans-serif;color:#172033;line-height:1.7;margin:20px}
.card{max-width:740px;padding:24px;border:1px solid #b9cde8;border-radius:12px;background:#f4f8ff}
h1{color:#1761a0}table{border-collapse:collapse;width:100%}th,td{border:1px solid #b9cde8;padding:12px;text-align:left}
th{background:#dcecff}pre{padding:16px;background:#e3eaf5;white-space:pre-wrap}small{color:#596579}
</style></head><body><div class="card"><h1>飞鸿 UI 验收周报 📎</h1>
<p><strong>明确标记：本地 UI 验收数据，没有真实账号和网络连接。</strong></p>
<table><thead><tr><th>项目</th><th>预期结果</th></tr></thead><tbody>
<tr><td>中文 / emoji 😊</td><td>字形清晰，基线与换行正确</td></tr>
<tr><td>Blitz HTML</td><td>表格、颜色、边框、引用和长文可滚动</td></tr>
<tr><td>本地 PDF</td><td>附件共两页，可在原 PDF 阅读器翻页</td></tr>
</tbody></table><h2>检查步骤</h2><ol><li>选择本邮件</li><li>上下滚动阅读正文</li><li>打开下方两页 PDF 附件</li></ol>
<pre>Rust + Slint + Blitz
邮件列表 → 原邮件正文 → 本地 PDF 预览</pre>
<blockquote>所有样本均保存在独立私有目录，允许保留本地已读与星标状态供重复验收。</blockquote>
<p>长文本排版测试：山川异域，风月同天。中文内容应随阅读区宽度自然换行。ABCDEFGHIJKLMNOPQRSTUVWXYZ 0123456789。</p>
<p><small>UI acceptance fixture · Offline · example.invalid</small></p></div></body></html>"#;
const LONGTEXT: &str = r#"<html><body><h2>多设备布局验收 👨‍👩‍👧‍👦</h2><p><strong>UI 验收数据</strong></p>
<p>这一段很长的中文用于检查手机、平板和 PC 布局中的自动换行。转动平板后，请观察邮件列表与正文阅读区是否仍然可用，正文是否重新排版，滚动位置是否合理。</p>
<p>混合字形：飞鸿邮箱 · HarmonyOS · Rust · Slint · Blitz · 2026 · 😊 🚀 🕊️</p>
<p>重复阅读段落：春风又绿江南岸，明月何时照我还。山川异域，风月同天。</p>
<p>重复阅读段落：春风又绿江南岸，明月何时照我还。山川异域，风月同天。</p>
<p>重复阅读段落：春风又绿江南岸，明月何时照我还。山川异域，风月同天。</p>
<p>本内容只用于 UI 验收，既没有发件请求，也没有外部图片。</p></body></html>"#;
const CHECKLIST: &str = r#"<html><body><h2>交互验收清单 ✅</h2><p><strong>UI 验收数据</strong></p>
<ul><li>邮件列表选择与滚动</li><li>邮件正文选择、缩放与滚动</li><li>同一会话的两封邮件展开</li><li>星标与已读状态本地保存</li><li>两页 PDF 附件打开、翻页与关闭</li></ul>
<p>账号为 ui-fixture@example.invalid；任何凭据保存请求都会明确失败。</p></body></html>"#;

fn raw_email(index: usize, sample: &Sample, pdf: &[u8]) -> String {
    let outer = format!("feihong-acceptance-mixed-{index}");
    let inner = format!("feihong-acceptance-alternative-{index}");
    let mut result = format!(
        "From: {} <acceptance@example.invalid>\r\nTo: {ACCOUNT}\r\nSubject: {}\r\nDate: Sat, 10 Oct 2026 10:00:00 +0800\r\nMessage-ID: <ui-acceptance-{index}@example.invalid>\r\nMIME-Version: 1.0\r\n",
        encoded_header(sample.sender),
        encoded_header(sample.subject)
    );
    if sample.reply {
        result.push_str("In-Reply-To: <ui-acceptance-1@example.invalid>\r\nReferences: <ui-acceptance-1@example.invalid>\r\n");
    }
    result.push_str(&format!("Content-Type: multipart/mixed; boundary=\"{outer}\"\r\n\r\n--{outer}\r\nContent-Type: multipart/alternative; boundary=\"{inner}\"\r\n\r\n"));
    for (kind, body) in [("plain", sample.text), ("html", sample.html)] {
        result.push_str(&format!("--{inner}\r\nContent-Type: text/{kind}; charset=utf-8\r\nContent-Transfer-Encoding: base64\r\n\r\n{}\r\n", base64_lines(body.as_bytes())));
    }
    result.push_str(&format!("--{inner}--\r\n"));
    if sample.pdf {
        result.push_str(&format!("--{outer}\r\nContent-Type: application/pdf; name=\"UI-acceptance-two-pages.pdf\"\r\nContent-Disposition: attachment; filename=\"UI-acceptance-two-pages.pdf\"\r\nContent-Transfer-Encoding: base64\r\n\r\n{}\r\n", base64_lines(pdf)));
    }
    result.push_str(&format!("--{outer}--\r\n"));
    result
}
fn encoded_header(value: &str) -> String {
    // RFC 2047 limits each encoded word to 75 ASCII characters. Keep every
    // chunk on a UTF-8 boundary and fold adjacent words without adding text.
    let mut words = Vec::new();
    let mut start = 0;
    for (offset, character) in value.char_indices() {
        if offset + character.len_utf8() - start > 42 {
            words.push(format!(
                "=?UTF-8?B?{}?=",
                base64(value[start..offset].as_bytes())
            ));
            start = offset;
        }
    }
    if start < value.len() {
        words.push(format!("=?UTF-8?B?{}?=", base64(value[start..].as_bytes())));
    }
    words.join("\r\n ")
}
fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut encoded = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = (u32::from(chunk[0]) << 16)
            | (u32::from(*chunk.get(1).unwrap_or(&0)) << 8)
            | u32::from(*chunk.get(2).unwrap_or(&0));
        encoded.push(ALPHABET[((n >> 18) & 63) as usize] as char);
        encoded.push(ALPHABET[((n >> 12) & 63) as usize] as char);
        encoded.push(if chunk.len() > 1 {
            ALPHABET[((n >> 6) & 63) as usize] as char
        } else {
            '='
        });
        encoded.push(if chunk.len() > 2 {
            ALPHABET[(n & 63) as usize] as char
        } else {
            '='
        });
    }
    encoded
}
fn base64_lines(bytes: &[u8]) -> String {
    base64(bytes)
        .as_bytes()
        .chunks(76)
        .map(|line| std::str::from_utf8(line).expect("Base64 is ASCII"))
        .collect::<Vec<_>>()
        .join("\r\n")
}

/// A complete two-page PDF, including byte-accurate stream lengths and xref.
/// Uses only standard Helvetica and ASCII page labels; no global PDF engine.
fn two_page_pdf() -> Vec<u8> {
    let mut objects = vec![
        "<< /Type /Catalog /Pages 2 0 R >>".to_owned(),
        "<< /Type /Pages /Count 2 /Kids [3 0 R 4 0 R] >>".to_owned(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 595 842] /Resources << /Font << /F1 5 0 R >> >> /Contents 6 0 R >>".to_owned(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 595 842] /Resources << /Font << /F1 5 0 R >> >> /Contents 7 0 R >>".to_owned(),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_owned(),
    ];
    for page in 1..=2 {
        let color = if page == 1 {
            "0.10 0.38 0.67"
        } else {
            "0.12 0.50 0.32"
        };
        let stream = format!(
            "{color} rg\n40 670 515 110 re f\n1 1 1 rg\nBT /F1 24 Tf 60 730 Td (Feihong UI acceptance - page {page}) Tj ET\n0 0 0 rg\nBT /F1 16 Tf 60 620 Td (OFFLINE TEST DATA - NOT REAL MAIL) Tj ET\nBT /F1 14 Tf 60 580 Td (Two pages. Open, scroll, change page, and close.) Tj ET\n"
        );
        objects.push(format!(
            "<< /Length {} >>\nstream\n{stream}endstream",
            stream.len()
        ));
    }
    let mut pdf = b"%PDF-1.4\n%\xE2\xE3\xCF\xD3\n".to_vec();
    let mut offsets = vec![0];
    for (index, object) in objects.iter().enumerate() {
        offsets.push(pdf.len());
        pdf.extend_from_slice(format!("{} 0 obj\n{object}\nendobj\n", index + 1).as_bytes());
    }
    let xref = pdf.len();
    pdf.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", offsets.len()).as_bytes());
    for offset in offsets.iter().skip(1) {
        pdf.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    pdf.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
            offsets.len()
        )
        .as_bytes(),
    );
    pdf
}
