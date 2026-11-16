//! Requested-live allocation probe for a large outgoing MIME attachment.
//! Run a fresh process per sample so allocator state does not cross runs.

use flectar_mail_core::{
    mime::{OutgoingAttachment, OutgoingMessage, build_message_owned},
    models::Address,
};
use std::{
    alloc::{GlobalAlloc, Layout, System},
    sync::atomic::{AtomicUsize, Ordering},
    time::Instant,
};

struct CountingAllocator;
static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

fn added(bytes: usize) {
    let live = LIVE.fetch_add(bytes, Ordering::Relaxed) + bytes;
    PEAK.fetch_max(live, Ordering::Relaxed);
}

fn proc_kib(field: &str, rollup: &str) -> usize {
    rollup
        .lines()
        .find_map(|line| {
            line.strip_prefix(field)
                .and_then(|value| value.split_whitespace().next())
                .and_then(|value| value.parse().ok())
        })
        .expect("memory field in smaps_rollup")
}

unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let pointer = unsafe { System.alloc(layout) };
        if !pointer.is_null() {
            added(layout.size());
        }
        pointer
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let pointer = unsafe { System.alloc_zeroed(layout) };
        if !pointer.is_null() {
            added(layout.size());
        }
        pointer
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        let replacement = unsafe { System.realloc(pointer, layout, size) };
        if !replacement.is_null() {
            if size >= layout.size() {
                added(size - layout.size());
            } else {
                LIVE.fetch_sub(layout.size() - size, Ordering::Relaxed);
            }
        }
        replacement
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        unsafe { System.dealloc(pointer, layout) };
        LIVE.fetch_sub(layout.size(), Ordering::Relaxed);
    }
}

fn main() {
    let size_mib: usize = std::env::args()
        .nth(1)
        .map(|value| value.parse().expect("integer attachment MiB"))
        .unwrap_or(24);
    assert!((1..=25).contains(&size_mib));
    let attachment_bytes = size_mib * 1024 * 1024;
    let to = [Address {
        name: None,
        email: "recipient@example.test".into(),
    }];
    let message = OutgoingMessage {
        from: Address {
            name: None,
            email: "sender@example.test".into(),
        },
        reply_to: None,
        to: &to,
        cc: &[],
        bcc: &[],
        subject: "Memory probe",
        body_text: "Attachment payload",
        body_html: None,
        in_reply_to: None,
        references: &[],
        message_id: Some("mime-memory-probe@example.test"),
        message_id_domain: "example.test",
        attachments: vec![OutgoingAttachment {
            filename: "sample.bin".into(),
            mime_type: "application/octet-stream".into(),
            bytes: vec![0x5a; attachment_bytes],
        }],
        receipt_to: &[],
        importance: Default::default(),
    };
    let baseline = LIVE.load(Ordering::Relaxed);
    PEAK.store(baseline, Ordering::Relaxed);
    let started = Instant::now();
    let (_, raw) = build_message_owned(message).expect("build MIME message");
    let elapsed_ms = started.elapsed().as_secs_f64() * 1000.0;
    let peak_extra = PEAK.load(Ordering::Relaxed).saturating_sub(baseline);
    let settled_extra = LIVE.load(Ordering::Relaxed).saturating_sub(baseline);
    let rollup = std::fs::read_to_string("/proc/self/smaps_rollup").expect("Linux smaps_rollup");
    println!(
        "attachment_bytes={attachment_bytes} raw_bytes={} raw_capacity={} peak_extra_bytes={peak_extra} settled_extra_bytes={settled_extra} elapsed_ms={elapsed_ms:.2} rss_kib={} pss_kib={} swap_kib={}",
        raw.len(),
        raw.capacity(),
        proc_kib("Rss:", &rollup),
        proc_kib("Pss:", &rollup),
        proc_kib("Swap:", &rollup),
    );
}
