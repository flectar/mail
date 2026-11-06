# Memory use

This page explains how the memory figures in the [README](../README.md) and on
[flectar.com/mail](https://flectar.com/mail/) are measured, and how to check
them yourself.

## Results

Measured on 5 November 2026 in one session, on the setup below: the same
mailbox and window size, a fresh launch, the same HTML newsletter displayed and
a sample 60 seconds after launch. Each range covers two runs. Every process an
app starts is counted, and RSS is summed over them.

| Memory (MiB) | Private | PSS | RSS | Processes | Renders email with |
| --- | ---: | ---: | ---: | ---: | --- |
| Flectar Mail | 17.2–17.4 | 79.1–80.0 | 85.9–86.6 | 1 | Blitz, CPU renderer |
| Geary 46.0 | 126.1–126.2 | 309.6–311.4 | 446.4–447.3 | 8 | WebKitGTK 2.54 |
| Evolution 3.56.2¹ | 191.0–192.2 | 395.0–395.9 | 697.5–698.4 | 12 | WebKitGTK 2.54 |
| Thunderbird 159.0 | 244.1–248.1 | 424.4–427.1 | 563.2–567.8 | 3 | Gecko |
| Betterbird 153.6.0esr-bb12² | 243.9–247.8 | 411.9–419.7 | 556.8–560.4 | 3 | Gecko |
| Mailspring 1.26.2 | 311.4–314.9 | 551.1–551.7 | 1035.3–1036.4 | 10 | Electron (Chromium) |
| Aerion 0.3.6 | 421.9–437.3 | 660.4–678.0 | 844.5–858.7 | 3 | WebKitGTK 2.54 |

1. Includes the evolution-data-server services it starts: 39 MiB private and
   72 MiB PSS. Evolution's own processes used 151.8–153.0 MiB private and
   323.1–324.0 MiB PSS. GNOME desktops usually run these services already.
2. The first relaunch after its initial sync measured 277.8 MiB private and
   456.2 MiB PSS; the range covers the two runs after it.

- **Engines and sandboxes.** Geary and Evolution ran WebKit with its sandbox, so
  `bwrap` and `xdg-dbus-proxy` are counted. Aerion starts WebKit without it.
  Mailspring's count includes its `mailsync` process and Chromium's crash
  handler.
- **No GPU.** The test machine has no graphics card, so every app rendered in
  software; Aerion's WebKit processes, for example, loaded Mesa's llvmpipe.
  Results on a desktop with a GPU may differ.
- **Connections.** Flectar Mail, Geary and Evolution used IMAP over TLS.
  Thunderbird, Betterbird, Aerion and Mailspring used plain IMAP: Aerion and
  Mailspring rejected the test server's self-signed certificate.
- **Folders.** All six folders were subscribed, so every client listed the same
  folders.
- **Fonts.** The test machine has 8 font faces; a desktop usually has hundreds.
  With a 358-face font set, Flectar Mail measured 18.3–18.5 MiB private. The
  other apps weren't re-measured with it.

Geary and Evolution are the Debian 13 packages; Thunderbird, Betterbird,
Mailspring (`.deb`) and Aerion (release tarball) are the vendors' Linux builds,
each with a new profile and default settings.

### Flectar Mail in other states

The same machine and mailbox, before and after the changes in
[#191](https://github.com/flectar/mail/pull/191),
[#192](https://github.com/flectar/mail/pull/192) and
[#194](https://github.com/flectar/mail/pull/194). Each figure is private memory
from one run, or a range over two.

| Flectar Mail (MiB) | Before | After |
| --- | ---: | ---: |
| Welcome screen, no account | 8.2 | 7.9 |
| Inbox and newsletter, as in the results above | 18.5–18.6 | 17.2–17.4 |
| The same at display scale 2 | 26.1 | 20.8–20.9 |
| First sync, inbox list, 60 s after the last write | 20.2 | 14.7 |
| First sync with the newsletter open, 60 s after the last write | 25.3 | 20.5 |

- **After a sync**, SQLite's page caches stay full and the allocator keeps the
  pages they freed. Flectar Mail now releases both once its mail core has been
  quiet for 10 seconds, and again after you leave Mail, Calendar, Files or
  Contacts for another product.
- **Display scale** changes the message tiles: the email body is drawn at the
  screen's physical resolution, in tiles of about 512 device pixels with one
  tile of overscan above and below the viewport. At scale 2 the newsletter's
  tiles and other large buffers took 6.4 MiB, down from 10.6 MiB: tiles are now
  stored as RGB, without the alpha byte, and are half as tall in logical pixels
  there.
- **Threads.** The D-Bus connection for the tray no longer starts a Tokio
  runtime with one idle thread per CPU core: the idle app now runs 11 threads
  instead of 22 on the 12-core test machine.
- **Low memory.** When the system reports memory pressure, Flectar Mail drops
  what it can rebuild, such as message tiles outside the viewport, image caches
  and SQLite's page caches, and returns the freed memory
  ([#201](https://github.com/flectar/mail/pull/201)). Measured on that pull
  request's build with the newsletter open, a low-memory warning from the
  desktop portal took private memory from 17.5 to 16.2 MiB within five seconds.

On Windows 11, with 7 real accounts and about 69,000 unread messages, Task
Manager's **Details** tab showed, before these changes:

| Flectar Mail (MiB) | Private working set | Working set | Commit size |
| --- | ---: | ---: | ---: |
| Window open, idle | 37.8 | 64.5 | 53.7 |
| Closed to the tray | 4.2 | 8.3 | 49.4 |

Windows builds now also use the segment heap
([#195](https://github.com/flectar/mail/pull/195)) and decommit free heap
memory when they trim ([#191](https://github.com/flectar/mail/pull/191));
neither has been measured on Windows yet.

The README's Performance table quotes these figures as measured. Its headline
and summary round them to about 20 MB of private memory and about 85 MB with
code and libraries (1 MiB = 1.049 MB): the runs above measured 18.0–18.2 MB
and 82.9–83.9 MB, and a desktop with a full font set measures 19.2–19.4 MB of
private memory.

## What the numbers mean

| Metric | Counts | Where you see it |
| --- | --- | --- |
| Private memory | Memory that belongs to the app alone and isn't backed by a file: heap, caches, stacks. Adds up correctly across processes. | Windows Task Manager's **Memory** column (private working set); GNOME System Monitor's **Memory** column (RSS minus shared); `RssAnon` on Linux |
| PSS | Private memory plus the app's code and libraries mapped from disk, with shared pages split between the processes that use them | Plasma System Monitor's **Memory** column, `smem`, `/proc/<pid>/smaps_rollup` |
| RSS | Every resident page, shared libraries in full. Summed over several processes, it counts shared pages more than once. | `top`, `htop` (RES) |

Most of Flectar Mail's RSS is its own executable: about 60 MiB of code and
read-only data mapped from disk. Those pages are clean, so the system can drop
them under memory pressure and read them back later.

### Why Windows shows less than Linux

- **Different columns.** Task Manager's **Memory** leaves out program code,
  while `htop` (RSS) and Plasma System Monitor (PSS) count the 60 MiB
  executable. Compared like for like, the large Windows mailbox uses more
  private memory (37.8 MiB) than the one-account Linux test (17.2–17.4 MiB).
- **More of the program file counts on Linux.** Linux maps a file 64 KiB at a
  time around each page that's touched, so RSS includes code that may never
  run. That's one reason the Windows working set (64.5 MiB) is lower than the
  Linux RSS (86 MiB) despite the much larger mailbox.
- **The tray trim.** When the window closes to the tray, or stays minimized for
  10 seconds, Flectar Mail releases its rendering buffers and, on Windows,
  empties its working set. Windows moves those pages to its standby list, so the
  working set drops to a few MiB while commit size, the memory still allocated,
  barely moves. When the window reopens, the pages usually come straight back
  from RAM. Linux has no equivalent: closing to the tray frees the window's
  memory, but the program's code stays resident, and minimizing changes little.

## How to measure

Keep everything but the app identical: machine, account, mailbox, window size
and renderer. Use the same state every time: a fresh launch, the inbox open, a
message displayed and no interaction for 60 seconds. Repeat each measurement at
least twice.

### Linux

```sh
scripts/generate-benchmark-mailbox.py ~/bench/Maildir   # test mailbox; serve it over IMAP
scripts/memory-snapshot.py "$(pidof flectar-mail)"
scripts/memory-snapshot.py --exe-dir /opt/thunderbird   # every process of an app
```

### Windows

In Task Manager, open **Details**, right-click a column header and choose
**Select columns**. Add **Memory (private working set)**, **Working set
(memory)** and **Commit size**. For an app with several processes, add up all of
them; the **Processes** tab groups them for you.

Keep the window open while you read the numbers. After Flectar Mail has been
minimized for 10 seconds, or closed to the tray, it releases its rendering
buffers and trims its working set, so Task Manager then shows the background
figure. Commit size counts all the private memory the app has committed,
whether or not it is in RAM at that moment, so it doesn't drop after a trim.

### macOS

Activity Monitor's **Memory** column shows the physical footprint, which also
counts window surfaces held for the display, so it reads higher on Retina
screens. `footprint <pid>` or `vmmap --summary <pid>` gives the breakdown.

## Test setup

- **Flectar Mail:** CI AppImages with the default CPU renderer. "After" is
  `main` at 51c6aed with #191, #192, #194 and #195 merged; "before" is 81d125b,
  the same code as `main` before them. The optional GPU renderer uses more
  memory and isn't covered here.
- **Other clients:** the versions and builds listed under Results.
- **System:** Debian 13, Linux 6.12, x86-64. Xvfb at 1920 × 1080 with
  Openbox, and every window at 1320 × 800 and scale 1. The scale-2 runs used a
  second Xvfb screen at 3200 × 2000.
- **Mailbox:** Dovecot 2.4.1 on the same machine, serving the Maildir written by
  [`scripts/generate-benchmark-mailbox.py`](../scripts/generate-benchmark-mailbox.py).
  It has 600 Inbox, 200 Archive and 60 Sent messages. About 45% are HTML
  newsletters, 20% short HTML notifications and 35% plain-text conversations,
  some with PDF attachments.
- **Windows:** Windows 11 with 7 real accounts and about 69,000 unread
  messages, read from Task Manager's Details tab with the window open and again
  after closing it to the tray.
