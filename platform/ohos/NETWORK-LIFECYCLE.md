# Native network activity gate

`Core::set_platform_active(bool)` is an async host API. `false` updates a
retained, per-Core watch value before waking account actors with `SyncCmd::Suspend`.
Workers spawned later inherit that value. Desktop hosts keep the default `true`.
`Core::platform_is_active()` reports the retained permission, not a count of
connections or an acknowledgement that all work has drained.

The native host must set `true` for an active UI or an OS-authorized headless
work window, and set `false` when both end. The headless timeout/cleanup path
must close the gate even if readiness or synchronization failed. Rendering
suspension alone does not stop network actors.

The gate prevents a new IMAP/Gmail/JMAP cycle, reader command, SMTP action claim,
calendar/contact cycle, scheduler tick, and history/body backfill step or chunk.
Pending durable actions stay pending. A foreground `sync_now` request while
paused returns an explicit suspended error.

IMAP IDLE receives `Suspend`, exits through the existing protocol-safe IDLE
completion, and drops its connection at the next loop boundary. JMAP push waits
also wake through the account command channel and drop the event stream.
Backfill connections close at their next safe boundary. An unused reader may
retain an ordinary TCP connection while blocked on its command channel; it
does not send polling/IDLE traffic in that state.

Already-started operations and a current provider/DAV cycle complete naturally.
SMTP is never aborted by this API: a relay may already have accepted a message,
so its acknowledgement and durable local result must finish before pausing.
No subsequent action is claimed after the retained gate is observed false.
There is no immediate cancellation guarantee or drained-worker barrier. The
host must describe this state as “suspend requested / new work gated”, rather
than “all sockets stopped”. Existing transport deadlines still apply.

The gate concerns automatic actor work. Explicit API calls outside those actors
(account discovery, OAuth setup, direct file transfers, etc.) remain their
caller's responsibility. The native foreground/headless ownership code must
not start those operations after its permitted window ends.

Regression checks cover retained suspension, a late worker, resume before
subscription, independent Core instances, readiness errors/timeouts, and a due
SMTP queue entry remaining unclaimed while paused. Device background execution
still needs real-account validation before being marked accepted.
