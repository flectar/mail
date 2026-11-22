//! Attachments downloaded ahead of time for the accounts set to download
//! everything (#253), so they open without a connection: one at a time, the
//! newest mail first, inside each account's mail history, while its server
//! doesn't ask to wait.

use crate::Core;
use crate::db::repo;
use crate::error::Result;
use crate::events::CoreEvent;
use crate::models::{DownloadPolicy, MailProtocol, now_ms};
use std::collections::HashMap;
use std::time::Duration;

/// An attachment that didn't download waits this long before another try.
const RETRY_AFTER_MS: i64 = 60 * 60 * 1000;
/// With nothing to download, look again this often.
const IDLE: Duration = Duration::from_secs(10 * 60);

/// What woke the loop.
enum Woken {
    Mail,
    Online,
    Closed,
}

impl Core {
    /// Long-lived loop: downloads the attachments the accounts set to
    /// download everything want, and waits for new mail, a change of the
    /// setting, or coming back online when there are none.
    pub(crate) async fn prefetch_loop(&self) {
        use tokio::sync::broadcast::error::RecvError;
        let mut events = self.bus.subscribe();
        // Attachments that failed, until when they wait.
        let mut failed = HashMap::<i64, i64>::new();
        loop {
            let now = now_ms();
            failed.retain(|_, until| *until > now);
            match self.next_prefetch(&failed).await {
                Ok(Some(attachment_id)) => {
                    if let Err(error) = self.get_attachment(attachment_id).await {
                        tracing::debug!(attachment_id, %error, "an attachment didn't download ahead of time");
                        failed.insert(attachment_id, now + RETRY_AFTER_MS);
                    }
                    continue;
                }
                Ok(None) => {}
                Err(error) => tracing::warn!(%error, "finding attachments to download failed"),
            }
            let woken = async {
                loop {
                    match events.recv().await {
                        Ok(CoreEvent::MailNew { .. } | CoreEvent::MailUpdated { .. }) => {
                            return Woken::Mail;
                        }
                        Ok(CoreEvent::NetworkState { online: true }) => return Woken::Online,
                        Ok(_) => {}
                        Err(RecvError::Lagged(_)) => return Woken::Mail,
                        Err(RecvError::Closed) => return Woken::Closed,
                    }
                }
            };
            tokio::select! {
                () = self.prefetch.notified() => {}
                () = tokio::time::sleep(IDLE) => {}
                woken = woken => match woken {
                    Woken::Mail => {}
                    // What failed without a connection gets another try.
                    Woken::Online => failed.clear(),
                    Woken::Closed => return,
                },
            }
        }
    }

    /// The attachment to download next: of the accounts set to download
    /// everything and whose servers don't ask to wait, the one of the newest
    /// mail inside its account's mail history, but those in `failed`.
    async fn next_prefetch(&self, failed: &HashMap<i64, i64>) -> Result<Option<i64>> {
        let mut next = None::<(i64, i64)>;
        for config in self.list_account_configs().await? {
            let everything = config.settings.download == DownloadPolicy::Everything
                && matches!(
                    config.mail_protocol,
                    MailProtocol::Imap | MailProtocol::Jmap
                );
            if !everything || self.pace.account(config.id).paused().is_some() {
                continue;
            }
            let account_id = config.id;
            let since = config.settings.mail_history.cutoff_ms_at(now_ms());
            let skip = failed.keys().copied().collect::<Vec<_>>();
            let candidate = self
                .db
                .read(move |conn| {
                    repo::messages::next_attachment_to_prefetch(
                        conn,
                        account_id,
                        since,
                        DownloadPolicy::LARGEST_ATTACHMENT,
                        &skip,
                    )
                })
                .await?;
            if let Some((attachment_id, date)) = candidate
                && next.is_none_or(|(_, newest)| date > newest)
            {
                next = Some((attachment_id, date));
            }
        }
        Ok(next.map(|(attachment_id, _)| attachment_id))
    }
}

#[cfg(test)]
mod tests {
    use crate::models::DownloadPolicy;
    use crate::{Core, Paths, db};
    use std::collections::HashMap;

    async fn download_of(core: &Core, account_id: i64) -> DownloadPolicy {
        core.list_account_configs()
            .await
            .unwrap()
            .into_iter()
            .find(|config| config.id == account_id)
            .unwrap()
            .settings
            .download
    }

    /// An IMAP or JMAP account chooses what it downloads, the attachments of
    /// one set to download everything come next, and other accounts keep what
    /// their providers give (#253).
    #[tokio::test]
    async fn an_account_set_to_download_everything_gets_its_attachments() {
        let temp = tempfile::tempdir().unwrap();
        let core = Core::start_mail_ui(Paths::for_tests(temp.path()))
            .await
            .unwrap();
        // Mail of today, inside every mail history.
        let today = crate::models::now_ms();
        let attachment = core
            .db
            .write(move |conn| {
                db::testutil::seed_account(conn);
                conn.execute(
                    "INSERT INTO messages (id, account_id, folder_id, subject, from_addr, date,
                                           is_read, is_draft, is_outgoing)
                     VALUES (10, 1, 1, 'Files', 'a@example.com', ?1, 1, 0, 0)",
                    [today],
                )?;
                conn.execute(
                    "INSERT INTO attachments (message_id, filename, size, imap_section)
                     VALUES (10, 'a.pdf', 10, '2')",
                    [],
                )?;
                Ok(conn.last_insert_rowid())
            })
            .await
            .unwrap();
        assert_eq!(download_of(&core, 1).await, DownloadPolicy::Messages);
        assert_eq!(core.next_prefetch(&HashMap::new()).await.unwrap(), None);

        core.set_account_download(1, DownloadPolicy::Everything)
            .await
            .unwrap();
        assert_eq!(download_of(&core, 1).await, DownloadPolicy::Everything);
        assert_eq!(
            core.next_prefetch(&HashMap::new()).await.unwrap(),
            Some(attachment)
        );
        // One that failed waits.
        assert_eq!(
            core.next_prefetch(&HashMap::from([(attachment, i64::MAX)]))
                .await
                .unwrap(),
            None
        );

        // A JMAP server sends each attachment as its blob.
        let blob = core
            .db
            .write(|conn| {
                conn.execute(
                    "UPDATE accounts SET mail_protocol = 'jmap' WHERE id = 1",
                    [],
                )?;
                conn.execute(
                    "INSERT INTO attachments (message_id, filename, size, jmap_blob_id)
                     VALUES (10, 'b.pdf', 10, 'blob-b')",
                    [],
                )?;
                Ok(conn.last_insert_rowid())
            })
            .await
            .unwrap();
        core.set_account_download(1, DownloadPolicy::Everything)
            .await
            .unwrap();
        assert_eq!(
            core.next_prefetch(&HashMap::new()).await.unwrap(),
            Some(blob)
        );

        core.db
            .write(|conn| {
                conn.execute(
                    "UPDATE accounts SET mail_protocol = 'pop3' WHERE id = 1",
                    [],
                )?;
                Ok(())
            })
            .await
            .unwrap();
        assert!(
            core.set_account_download(1, DownloadPolicy::Headers)
                .await
                .is_err()
        );
        assert_eq!(core.next_prefetch(&HashMap::new()).await.unwrap(), None);
    }
}
