#!/usr/bin/env python3
"""Write the deterministic Maildir used for docs/performance.md.

The mailbox mixes HTML newsletters, short HTML notifications and plain-text
conversations, some with PDF attachments, dated over the last 60 days. Serve
it with any IMAP server that reads Maildir (the measurements used Dovecot).

    scripts/generate-benchmark-mailbox.py ~/bench/Maildir
"""

from __future__ import annotations

import argparse
import email.utils
import os
import random
import time
from email.message import EmailMessage
from pathlib import Path

SENDERS = [
    ("Ana Ruiz", "ana@northwind.example"),
    ("Ben Ortiz", "ben@contoso.example"),
    ("Chloe Martin", "chloe@fabrikam.example"),
    ("Dmitri Ivanov", "dmitri@adatum.example"),
    ("Elena Rossi", "elena@litware.example"),
    ("Farah Khan", "farah@proseware.example"),
    ("Gwen Lee", "gwen@tailspin.example"),
    ("Hugo Silva", "hugo@wingtip.example"),
]
LISTS = [
    ("Rust Weekly", "news@rustweekly.example"),
    ("Design Digest", "hello@designdigest.example"),
    ("Cloud Status", "status@cloudhost.example"),
    ("Shop Deals", "deals@shop.example"),
    ("Dev Events", "events@devconf.example"),
    ("Bank Notices", "notice@bank.example"),
]
WORDS = (
    "release roadmap invoice meeting sprint review design budget travel report "
    "draft summary feedback launch update contract quarterly agenda notes metrics "
    "deploy migration customer support partner workshop hiring planning"
).split()


class Generator:
    def __init__(self, seed: int, now: float) -> None:
        self.rng = random.Random(seed)
        self.now = now

    def sentence(self, words: int = 12) -> str:
        return " ".join(self.rng.choice(WORDS) for _ in range(words)).capitalize() + "."

    def paragraph(self, sentences: int = 4) -> str:
        return " ".join(self.sentence(self.rng.randint(8, 18)) for _ in range(sentences))

    def newsletter(self, title: str) -> str:
        stories = []
        for index in range(self.rng.randint(4, 9)):
            stories.append(f"""
      <tr><td style="padding:16px 24px;border-bottom:1px solid #e5e7eb">
        <h2 style="font:600 18px/1.3 Helvetica,Arial,sans-serif;color:#111827;margin:0 0 8px">{self.sentence(6)}</h2>
        <p style="font:15px/1.6 Helvetica,Arial,sans-serif;color:#374151;margin:0 0 12px">{self.paragraph(3)}</p>
        <a href="https://example.com/story/{index}" style="display:inline-block;background:#2563eb;color:#fff;
           padding:8px 14px;border-radius:6px;text-decoration:none;font:600 14px Helvetica,Arial">Read more</a>
      </td></tr>""")
        return f"""<!doctype html><html><head><meta charset="utf-8">
<style>@media (max-width:600px){{.wrap{{width:100%!important}}}} a{{color:#2563eb}}</style></head>
<body style="margin:0;background:#f3f4f6">
<table role="presentation" width="100%" cellpadding="0" cellspacing="0" style="background:#f3f4f6"><tr><td align="center">
  <table class="wrap" role="presentation" width="600" cellpadding="0" cellspacing="0" style="background:#ffffff;margin:24px 0">
    <tr><td style="padding:24px;background:#111827;color:#fff;font:700 22px Helvetica,Arial">{title}</td></tr>
    {''.join(stories)}
    <tr><td style="padding:16px 24px;font:12px/1.5 Helvetica,Arial;color:#6b7280">
      You are receiving this because you subscribed. <a href="https://example.com/unsubscribe">Unsubscribe</a>.
      <img src="https://tracker.example.com/open.gif" width="1" height="1" alt=""></td></tr>
  </table></td></tr></table></body></html>"""

    def notification(self) -> str:
        return f"""<html><body style="font-family:Arial,sans-serif">
<div style="max-width:520px;margin:0 auto;padding:20px;border:1px solid #ddd;border-radius:8px">
<p style="font-size:16px"><b>{self.sentence(5)}</b></p><p>{self.paragraph(2)}</p>
<p><a href="https://example.com/view">View details</a></p></div></body></html>"""

    def message(self, index: int) -> tuple[float, bytes, str]:
        kind = self.rng.random()
        sent = self.now - self.rng.uniform(0, 60 * 86400)
        message = EmailMessage()
        message["Date"] = email.utils.formatdate(sent)
        message["To"] = "Test User <testuser@localhost>"
        message["Message-ID"] = f"<ram-{index}@flectar.test>"
        if kind < 0.45:
            name, address = self.rng.choice(LISTS)
            message["From"] = f"{name} <{address}>"
            message["Subject"] = f"{name}: {self.sentence(5)[:-1]}"
            message["List-Unsubscribe"] = "<https://example.com/unsubscribe>"
            message.set_content(self.paragraph(6))
            message.add_alternative(self.newsletter(name), subtype="html")
        elif kind < 0.65:
            name, address = self.rng.choice(LISTS)
            message["From"] = f"{name} <{address}>"
            message["Subject"] = self.sentence(6)[:-1]
            message.set_content(self.paragraph(2))
            message.add_alternative(self.notification(), subtype="html")
        else:
            name, address = self.rng.choice(SENDERS)
            message["From"] = f"{name} <{address}>"
            subject = self.sentence(5)[:-1]
            if self.rng.random() < 0.35 and index > 10:
                parent = self.rng.randint(0, index - 1)
                message["In-Reply-To"] = f"<ram-{parent}@flectar.test>"
                message["References"] = f"<ram-{parent}@flectar.test>"
                subject = "Re: " + subject
            message["Subject"] = subject
            first = name.split()[0]
            message.set_content(f"Hi,\n\n{self.paragraph(3)}\n\n{self.paragraph(2)}\n\nThanks,\n{first}\n")
            if self.rng.random() < 0.12:
                size = self.rng.randint(20_000, 120_000)
                data = bytes(self.rng.getrandbits(8) for _ in range(size))
                message.add_attachment(
                    data, maintype="application", subtype="pdf", filename=f"{self.rng.choice(WORDS)}-{index}.pdf"
                )
        flags = "S" if self.rng.random() < 0.8 else ""
        if self.rng.random() < 0.05:
            flags = "F" + flags
        return sent, message.as_bytes(), flags

    def write_folder(self, folder: Path, first: int, count: int) -> None:
        for name in ("cur", "new", "tmp"):
            (folder / name).mkdir(parents=True, exist_ok=True)
        for index in range(first, first + count):
            sent, raw, flags = self.message(index)
            path = folder / "cur" / f"{int(sent)}.M{index}P1.flectar-bench,S={len(raw)}:2,{flags}"
            path.write_bytes(raw)
            # Maildir servers report the file time as the received date.
            os.utime(path, (sent, sent))


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("maildir", type=Path, help="Maildir to create; must not contain mail yet")
    parser.add_argument("--inbox", type=int, default=600, help="Inbox messages (default 600)")
    parser.add_argument("--seed", type=int, default=20261103)
    args = parser.parse_args()
    if args.inbox < 1 or any((args.maildir / "cur").glob("*")):
        parser.error("use an empty Maildir and at least one Inbox message")
    # The email package draws MIME boundaries from the global generator.
    random.seed(args.seed)
    generator = Generator(args.seed, time.time())
    archive, sent = args.inbox // 3, args.inbox // 10
    generator.write_folder(args.maildir, 0, args.inbox)
    generator.write_folder(args.maildir / ".Archive", args.inbox, archive)
    generator.write_folder(args.maildir / ".Sent", args.inbox + archive, sent)
    for folder in (".Drafts", ".Trash", ".Junk"):
        for name in ("cur", "new", "tmp"):
            (args.maildir / folder / name).mkdir(parents=True, exist_ok=True)
    print(f"Wrote {args.inbox + archive + sent} messages to {args.maildir}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
