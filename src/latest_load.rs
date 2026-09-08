//! A single in-flight read and one replaceable pending request.
//! Started operations finish so database claims cannot be left half-dispatched.
use std::future::Future;
use tokio::sync::watch;

pub async fn run<T, R, F, Read, G, Send>(
    mut requests: watch::Receiver<Option<(u64, T)>>,
    read: F,
    send: G,
) where
    T: Clone,
    F: Fn(T) -> Read,
    Read: Future<Output = R>,
    G: Fn(u64, R) -> Send,
    Send: Future<Output = ()>,
{
    while requests.changed().await.is_ok() {
        let request = requests.borrow_and_update().clone();
        let Some((generation, request)) = request else {
            continue;
        };
        let result = read(request).await;
        let current = requests.borrow().as_ref().map(|r| r.0);
        if current == Some(generation) {
            send(generation, result).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use tokio::sync::{Semaphore, mpsc};

    #[tokio::test]
    async fn rapid_selection_skips_queued_reads_and_discards_obsolete_results() {
        let (requests, rx) = watch::channel(None);
        let (started, mut starts) = mpsc::unbounded_channel();
        let (results, mut output) = mpsc::unbounded_channel();
        let release = Arc::new(Semaphore::new(0));
        let barrier = release.clone();
        let worker = tokio::spawn(run(
            rx,
            move |id| {
                started.send(id).unwrap();
                let barrier = barrier.clone();
                async move {
                    if id == 1 {
                        barrier.acquire().await.unwrap().forget();
                    }
                    id
                }
            },
            move |generation, id| {
                results.send((generation, id)).unwrap();
                async {}
            },
        ));
        requests.send_replace(Some((1, 1)));
        assert_eq!(starts.recv().await, Some(1));
        for id in 2..=100 {
            requests.send_replace(Some((id, id)));
        }
        release.add_permits(1);
        assert_eq!(starts.recv().await, Some(100));
        assert_eq!(output.recv().await, Some((100, 100)));
        assert!(output.try_recv().is_err());
        assert!(starts.try_recv().is_err());
        drop(requests);
        worker.await.unwrap();
    }
}
