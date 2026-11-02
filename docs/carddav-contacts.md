# Contacts and CardDAV

## Storage

Each saved contact is a row in `contact_entries`, and each row stores the
contact's complete vCard in `document`. That document is the contact. The other
columns and `contact_values` (every EMAIL and TEL property) are a projection of
it for listing, search, and autocomplete. They are rewritten in the same
transaction whenever the document changes. Embedded photos are decoded within
the existing image limits and stored once per content hash in
`contact_photos`. Photos given as URLs stay in the document and are never
fetched.

Editing changes only the properties the editor shows. Structured values,
parameters, photos, and properties the app does not model survive unchanged.
Email is optional and repeatable. It never identifies a contact, so two address
books can hold entries with the same address and they stay separate.

`contacts` keeps only per-address mail history for suggestions and ranking.
Mail never changes a saved contact.

## Synchronization

A `carddav_objects` row links an entry to a server resource. It holds the
href, the ETag, and the pending state. An entry without an object is stored
only on this device.

- An edit marks the object dirty and increments its `revision`. Uploads use
  `If-Match`, or `If-None-Match: *` for a new resource. An acknowledgement
  clears pending work only if no edit followed the uploaded revision. If the
  server normalized the upload, its version replaces the document.
- When the server version differs from a pending change, the server version is
  kept in `conflict_vcard` (or `conflict_deleted`). Nothing uploads until the
  user keeps one complete version. Keeping the local version uploads it over
  the server's current ETag.
- A server response matching the pending document is an acknowledgement whose
  original response was lost.
- A resource that cannot be downloaded or parsed records `last_error` and is
  downloaded again on the next sync, even if the collection is unchanged. It
  never blocks other resources. Multiget batches that exceed the response
  limit are split, and a single oversized resource is recorded and skipped.
- An address book missing from the server becomes unavailable. Its contacts
  stay visible but read-only until it returns.

Disconnecting CardDAV or removing the account deletes the address books. A
trigger turns their entries into local contacts and marks the ones with
unsynced changes. Contacts the user deleted while the deletion was waiting to
upload are removed. After reconnecting, a downloaded card whose UID matches a
kept contact is linked to it again instead of being added twice. If the kept
contact changed in the meantime, both versions wait for the user's choice, as
in any other conflict. Nothing is uploaded until then.

## Upgrading from earlier versions

Migration 13 converts the old email-keyed directory in one transaction:

- Contacts saved in the app become local entries, unless the old directory
  had linked them to a card. Those become that card's entry.
- Every cached card becomes its own entry, including cards the old directory
  hid because they had no email address.
- A pending edit, which lived only in the old contact row, is applied to its
  card and stays pending. A changed email or phone replaces the value the old
  version showed. An edit shared by several cards cannot be assigned
  to one of them, so each of those cards asks the user to choose a version.
- Pending deletions stay pending. Cards that cannot be parsed are retried.

Profile columns are then dropped from `contacts`, and rows without mail
history are removed.

## Release check

Mocked protocol tests do not show compatibility with a particular server.
Before release, test against a disposable Nextcloud address book:

1. Create a phone-only contact with an embedded photo, a structured name and
   address, two phone numbers, and an extension property.
2. Connect Flectar and confirm the entry and photo appear. Add two emails and
   choose the preferred one. Check that the server href and UID are
   unchanged, every other property survives, and recipient suggestions offer
   the usable emails.
3. Restart offline, check the contact and photo, edit, reconnect, and sync
   twice. The edit must arrive without duplicate contacts.
4. Edit the same contact in both clients and try each conflict choice,
   including a deletion against a server edit. Neither version may disappear
   before a choice is made.
5. Repeat from a database created before migration 13, with an unchanged ETag,
   a cached phone-only contact, a pending edit, and a pending deletion. Check
   favorites and mail counts.
6. Disconnect, or remove the account, while offline changes are pending.
   Check that the contacts stay on this device and deleted contacts stay
   deleted.
