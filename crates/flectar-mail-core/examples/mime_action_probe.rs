//! Production MIME action requested-heap probe. Fresh process per condition.
//! cargo run --locked --offline -j 1 -p flectar-mail-core --example mime_action_probe -- quote|build|inline|sanitize [bytes]
//! Input construction and warmup are excluded. This does not measure app RAM.
use flectar_mail_core::{mime, models::Address};
use std::{
    alloc::{GlobalAlloc, Layout, System},
    sync::atomic::{AtomicUsize, Ordering},
    time::Instant,
};
struct Counter;
static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);
static CALLS: AtomicUsize = AtomicUsize::new(0);
#[global_allocator]
static ALLOCATOR: Counter = Counter;
fn grow(bytes: usize) {
    let live = LIVE.fetch_add(bytes, Ordering::Relaxed) + bytes;
    PEAK.fetch_max(live, Ordering::Relaxed);
}
unsafe impl GlobalAlloc for Counter {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let pointer = unsafe { System.alloc(layout) };
        if !pointer.is_null() {
            grow(layout.size());
            CALLS.fetch_add(1, Ordering::Relaxed);
        }
        pointer
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let pointer = unsafe { System.alloc_zeroed(layout) };
        if !pointer.is_null() {
            grow(layout.size());
            CALLS.fetch_add(1, Ordering::Relaxed);
        }
        pointer
    }
    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        let next = unsafe { System.realloc(pointer, layout, size) };
        if !next.is_null() {
            if size >= layout.size() {
                grow(size - layout.size());
            } else {
                LIVE.fetch_sub(layout.size() - size, Ordering::Relaxed);
            }
            CALLS.fetch_add(1, Ordering::Relaxed);
        }
        next
    }
    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        unsafe { System.dealloc(pointer, layout) };
        LIVE.fetch_sub(layout.size(), Ordering::Relaxed);
    }
}

fn main() {
    let args = std::env::args().collect::<Vec<_>>();
    let action = args.get(1).map(String::as_str).unwrap_or("quote");
    assert!(matches!(action, "quote" | "build" | "inline" | "sanitize"));
    let size = args
        .get(2)
        .map(|value| value.parse::<usize>().expect("input bytes"))
        .unwrap_or(1024 * 1024);
    assert!((1024..=8 * 1024 * 1024).contains(&size));
    let text = "x\n".repeat(size / 2);
    let html = match action {
        "inline" => {
            use base64::Engine;
            let encoded = base64::engine::general_purpose::STANDARD.encode(vec![7; size]);
            format!("<img src=\"data:image/png;base64,{encoded}\">")
        }
        "sanitize" => format!(
            "<HTML><HEAD><TITLE>metadata</TITLE></HEAD><BoDy><p>{}</p></bOdY></HTML>",
            "x".repeat(size)
        ),
        _ => format!("<p>{}</p>", "x".repeat(size)),
    };
    let from = Address {
        name: Some("é界".into()),
        email: "sender@example.test".into(),
    };
    let recipients = [Address {
        name: None,
        email: "recipient@example.test".into(),
    }];
    let perform = || -> Vec<u8> {
        match action {
            "quote" => mime::quote_body(&text, &from, 1_700_000_000_000).into_bytes(),
            "sanitize" => mime::sanitize_html(&html).into_bytes(),
            _ => {
                let message = mime::OutgoingMessage {
                    from: from.clone(),
                    reply_to: None,
                    to: &recipients,
                    cc: &[],
                    bcc: &[],
                    subject: "Probe",
                    body_text: "plain",
                    body_html: Some(&html),
                    in_reply_to: None,
                    references: &[],
                    message_id: Some("probe@example.test"),
                    message_id_domain: "example.test",
                    attachments: Vec::new(),
                    receipt_to: &[],
                    importance: Default::default(),
                };
                mime::build_message_owned(message).unwrap().1
            }
        }
    };
    drop(perform());
    let before = LIVE.load(Ordering::Relaxed);
    PEAK.store(before, Ordering::Relaxed);
    CALLS.store(0, Ordering::Relaxed);
    let start = Instant::now();
    let output = perform();
    let elapsed_us = start.elapsed().as_micros();
    let peak_extra_bytes = PEAK.load(Ordering::Relaxed).saturating_sub(before);
    let settled_extra_bytes = LIVE.load(Ordering::Relaxed) as i128 - before as i128;
    let allocation_calls = CALLS.load(Ordering::Relaxed);
    let fingerprint = output.iter().fold(0xcbf29ce484222325_u64, |hash, &byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3)
    });
    if matches!(action, "build" | "inline") {
        let parsed = mime::parse_message(&output).unwrap();
        assert_eq!(
            parsed.headers.message_id.as_deref(),
            Some("probe@example.test")
        );
        assert_eq!(parsed.headers.to[0].email, "recipient@example.test");
        if action == "inline" {
            let (bytes, _) = mime::extract_attachment(&output, "0").unwrap();
            assert_eq!(bytes, vec![7; size]);
        }
    }
    println!(
        "{{\"scope\":\"mime_requested_heap\",\"action\":\"{action}\",\"input_bytes\":{size},\"peak_extra_bytes\":{peak_extra_bytes},\"settled_extra_bytes\":{settled_extra_bytes},\"allocation_calls\":{allocation_calls},\"elapsed_us\":{elapsed_us},\"output_bytes\":{},\"output_fingerprint\":{fingerprint}}}",
        output.len()
    );
    std::hint::black_box(output);
}
