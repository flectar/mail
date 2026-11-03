# Memory use

This page explains how the memory figures in the [README](../README.md) and on
[flectar.com/mail](https://flectar.com/mail/) are measured, and how to check
them yourself.

## Results

A fresh launch with one IMAP account (860 messages), with the inbox open and an
HTML newsletter displayed, sampled 60 seconds after launch (one Thunderbird run
at 100 seconds). Both apps ran on the same Linux machine against the same
mailbox, in a 1320 × 800 window. Each range covers two runs.

| Memory (MiB) | Flectar Mail | Thunderbird 159 |
| --- | ---: | ---: |
| Private memory | 19.0–19.1 | 239.7–242.3 |
| PSS | 80.9–81.3 | 414.3–417.4 |
| RSS | 87.2–87.3 | 556.3–560.4 |
| Processes | 1 | 4 |

Thunderbird's RSS is the sum over its processes. Its first launch, which also
downloaded the mailbox, measured 293.8 MiB private and 479.4 MiB PSS.

The same Flectar Mail build in other states:

| Flectar Mail (MiB) | Private | PSS | RSS |
| --- | ---: | ---: | ---: |
| Welcome screen, no account | 8.2 | 55.1 | 61.1 |
| Inbox idle after the first sync | 20.2 | 79.9 | 86.6 |
| After reading two messages | 25.8 | 93.0 | 100.0 |
| Then minimized for 25 seconds | 24.4 | 91.8 | 98.9 |

On Windows 11, with 7 real accounts and about 69,000 unread messages, Task
Manager's **Details** tab showed:

| Flectar Mail (MiB) | Private working set | Working set | Commit size |
| --- | ---: | ---: | ---: |
| Window open, idle | 37.8 | 64.5 | 53.7 |
| Closed to the tray | 4.2 | 8.3 | 49.4 |

The README rounds these to megabytes (1 MiB = 1.049 MB). It quotes about 20 MB
of private memory and about 85 MB PSS for Flectar Mail, about 250 MB and 435 MB
for Thunderbird, and about 40 MB and 4 MB for the large Windows mailbox.

## What the numbers mean

| Metric | Counts | Where you see it |
| --- | --- | --- |
| Private memory | Memory that belongs to the app alone and isn't backed by a file: heap, caches, stacks. Adds up correctly across processes. | Windows Task Manager's **Memory** column (private working set); GNOME System Monitor's **Memory** column (RSS minus shared); `RssAnon` on Linux |
| PSS | Private memory plus the app's code and libraries mapped from disk, with shared pages split between the processes that use them | Plasma System Monitor's **Memory** column, `smem`, `/proc/<pid>/smaps_rollup` |
| RSS | Every resident page, shared libraries in full. Summed over several processes, it counts shared pages more than once. | `top`, `htop` (RES) |

Most of Flectar Mail's RSS is its own executable: about 59 MiB of code and
read-only data mapped from disk. Those pages are clean, so the system can drop
them under memory pressure and read them back later.

### Why Windows shows less than Linux

- **Different columns.** Task Manager's **Memory** leaves out program code,
  while `htop` (RSS) and Plasma System Monitor (PSS) count the 59 MiB
  executable. Compared like for like, the large Windows mailbox uses more
  private memory (37.8 MiB) than the one-account Linux test (19–20 MiB).
- **More of the program file counts on Linux.** Linux maps a file 64 KiB at a
  time around each page that's touched, so RSS includes code that may never
  run. That's one reason the Windows working set (64.5 MiB) is lower than the
  Linux RSS (87 MiB) despite the much larger mailbox.
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

- **Flectar Mail:** `main` at 2cdc2a2 (2 November 2026), the CI AppImage, with
  the default CPU renderer. The optional GPU renderer uses more memory and
  isn't covered here.
- **Thunderbird:** 159.0, Mozilla's Linux x86-64 build, a new profile with
  default settings.
- **System:** Debian 13, Linux 6.12, x86-64. Xvfb at 1920 × 1080 with
  Openbox, and both windows at 1320 × 800 and scale 1.
- **Mailbox:** Dovecot 2.4.1 on the same machine, serving the Maildir written by
  [`scripts/generate-benchmark-mailbox.py`](../scripts/generate-benchmark-mailbox.py).
  It has 600 Inbox, 200 Archive and 60 Sent messages. About 45% are HTML
  newsletters, 20% short HTML notifications and 35% plain-text conversations,
  some with PDF attachments.
- **Connections:** Flectar Mail used IMAP over TLS; Thunderbird used plain IMAP.
  Thunderbird lists only subscribed folders, Inbox and Trash here, so it synced
  fewer folders than Flectar Mail.
- **Windows:** Windows 11 with 7 real accounts and about 69,000 unread
  messages, read from Task Manager's Details tab with the window open and again
  after closing it to the tray.
