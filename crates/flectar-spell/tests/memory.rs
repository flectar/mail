//! Checking keeps nothing in memory: the heap is back where it was after every check.

use std::alloc::{GlobalAlloc, Layout, System};
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};

use flectar_spell::{Dictionary, IndexCache, PersonalWords, Speller};

struct Counting;

static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let pointer = unsafe { System.alloc(layout) };
        if !pointer.is_null() {
            let live = LIVE.fetch_add(layout.size(), Relaxed) + layout.size();
            PEAK.fetch_max(live, Relaxed);
        }
        pointer
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        unsafe { System.dealloc(pointer, layout) };
        LIVE.fetch_sub(layout.size(), Relaxed);
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        let moved = unsafe { System.realloc(pointer, layout, size) };
        if !moved.is_null() {
            if size > layout.size() {
                let live = LIVE.fetch_add(size - layout.size(), Relaxed) + size - layout.size();
                PEAK.fetch_max(live, Relaxed);
            } else {
                LIVE.fetch_sub(layout.size() - size, Relaxed);
            }
        }
        moved
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

#[test]
fn checks_and_suggestions_keep_nothing() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../spellbook/vendor/en_US");
    let en_us = Dictionary {
        name: "en_US".into(),
        aff: dir.join("en_US.aff"),
        dic: dir.join("en_US.dic"),
    };
    let cache = tempfile::tempdir().unwrap();
    let config = tempfile::tempdir().unwrap();
    let speller = Speller::new(
        IndexCache::new(cache.path()),
        Some(PersonalWords::new(config.path())),
    );
    speller.cache().build(&en_us).unwrap();
    speller.add_word("Flectar", &en_us).unwrap();
    let dictionaries = [en_us];
    let text = "Thanks for the numbers. We can recieve the final figures by Friday, \
                and Flectar should seperate events from travel.";

    let check = || {
        let found = speller.check(text, &dictionaries, &[], |_| false).unwrap();
        assert_eq!(found.len(), 2);
        let suggestions = speller.suggest("recieve", &dictionaries).unwrap();
        assert_eq!(suggestions.first().map(String::as_str), Some("receive"));
    };
    // Statics the first check sets up, such as the standard library's, stay.
    check();
    let before = LIVE.load(Relaxed);
    PEAK.store(before, Relaxed);
    for _ in 0..5 {
        check();
        assert_eq!(LIVE.load(Relaxed), before, "a check left memory behind");
    }
    // The English rules and a suggestion's scratch space, not the 2 MiB word list.
    let peak = PEAK.load(Relaxed) - before;
    assert!(peak < 512 * 1024, "a check peaked at {peak} bytes");
}
