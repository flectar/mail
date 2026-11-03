//! Mail kept on this device has nothing to download or send. Its worker
//! completes the actions queued on that mail, such as emptying its Trash.

use super::engine::{PriorityFetchCmd, SyncCmd, SyncCtx};
use tokio::sync::mpsc;

pub(crate) async fn run(
    ctx: SyncCtx,
    account_id: i64,
    mut commands: mpsc::Receiver<SyncCmd>,
    mut reads: mpsc::Receiver<PriorityFetchCmd>,
) {
    loop {
        if let Err(error) = super::complete_device_actions(&ctx, account_id).await {
            tracing::warn!(account_id, %error, "completing actions on local mail failed");
        }
        tokio::select! {
            command = commands.recv() => match command {
                None | Some(SyncCmd::Shutdown) => return,
                Some(SyncCmd::SyncNow { complete }) => {
                    if let Some(complete) = complete {
                        let _ = complete.send(Ok(()));
                    }
                }
                // Bodies are stored with each message.
                Some(SyncCmd::RunActions | SyncCmd::FetchBody { .. }) => {}
            },
            read = reads.recv() => match read {
                None => return,
                Some(PriorityFetchCmd::Body(_)) => {}
                Some(PriorityFetchCmd::Attachment { complete, .. }) => {
                    let _ = complete.send(Err(
                        "attachments of local mail are read from the stored message".into(),
                    ));
                }
            },
        }
    }
}
