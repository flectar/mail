//! Auto Labels: deterministic header/subject classification of incoming mail
//! into Marketing / News / Social / Pitch. Runs inside the sync transaction so
//! categories exist before the thread first renders. Categories materialize as
//! rows in `labels` flagged `is_auto`; memberships are local-only (never pushed
//! to IMAP - see `labels::reconcile_keywords`).

use crate::error::Result;
use rusqlite::{Connection, params};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Category {
    Marketing,
    News,
    Social,
    Pitch,
}

impl Category {
    /// Stable keyword the seed rows carry; rows are resolved by keyword, not
    /// name, so users can rename the labels freely.
    pub fn keyword(self) -> &'static str {
        match self {
            Category::Marketing => "FlectarMailAutoMarketing",
            Category::News => "FlectarMailAutoNews",
            Category::Social => "FlectarMailAutoSocial",
            Category::Pitch => "FlectarMailAutoPitch",
        }
    }

    /// Inverse of [`Category::keyword`]; `None` for any other string (including
    /// the empty "no category" cache value).
    pub fn from_keyword(kw: &str) -> Option<Category> {
        match kw {
            "FlectarMailAutoMarketing" => Some(Category::Marketing),
            "FlectarMailAutoNews" => Some(Category::News),
            "FlectarMailAutoSocial" => Some(Category::Social),
            "FlectarMailAutoPitch" => Some(Category::Pitch),
            _ => None,
        }
    }
}

/// Everything the classifier looks at; kept plain so tests can fabricate it.
pub struct MessageFacts<'a> {
    pub from_addr: &'a str,
    pub subject: &'a str,
    pub is_automated: bool,
    /// List-Unsubscribe or List-Id present.
    pub has_list_headers: bool,
    /// Sender already appears in the harvested contacts table.
    pub sender_known: bool,
}

const SOCIAL_DOMAINS: &[&str] = &[
    "facebookmail.com",
    "linkedin.com",
    "x.com",
    "twitter.com",
    "instagram.com",
    "redditmail.com",
    "discord.com",
    "discordapp.com",
    "pinterest.com",
    "tiktok.com",
    "youtube.com",
    "nextdoor.com",
    "quora.com",
];

const NEWSLETTER_DOMAINS: &[&str] = &[
    "substack.com",
    "beehiiv.com",
    "buttondown.email",
    "ghost.io",
    "mailchimp.com",
    "mcsv.net",
    "convertkit.com",
    "kit.com",
    "revue.email",
];

const ESP_DOMAINS: &[&str] = &[
    "sendgrid.net",
    "klaviyomail.com",
    "braze.com",
    "hubspotemail.net",
    "mailgun.org",
    "cmail19.com",
    "cmail20.com",
    "rsgsv.net",
];

fn domain_of(addr: &str) -> &str {
    addr.rsplit('@').next().unwrap_or("")
}

fn local_of(addr: &str) -> &str {
    addr.split('@').next().unwrap_or("")
}

fn domain_matches(domain: &str, list: &[&str]) -> bool {
    list.iter()
        .any(|d| domain == *d || domain.ends_with(&format!(".{d}")))
}

fn subject_has(subject: &str, needles: &[&str]) -> bool {
    needles.iter().any(|n| subject.contains(n))
}

/// Precedence: Social > News > Marketing > Pitch; at most one category.
pub fn classify(f: &MessageFacts) -> Option<Category> {
    let from = f.from_addr.to_lowercase();
    let domain = domain_of(&from);
    let local = local_of(&from);
    let subject = f.subject.to_lowercase();

    if domain_matches(domain, SOCIAL_DOMAINS) {
        return Some(Category::Social);
    }

    let newsy_local = ["news", "newsletter", "digest", "weekly", "daily"]
        .iter()
        .any(|p| local.starts_with(p));
    if (f.is_automated || f.has_list_headers)
        && (domain_matches(domain, NEWSLETTER_DOMAINS)
            || newsy_local
            || subject_has(
                &subject,
                &["issue #", "digest", "this week in", "weekly roundup"],
            ))
    {
        return Some(Category::News);
    }

    let promo_local = ["marketing", "promo", "offers", "deals", "sale"]
        .iter()
        .any(|p| local.starts_with(p));
    if (f.is_automated || f.has_list_headers)
        && (domain_matches(domain, ESP_DOMAINS)
            || promo_local
            || subject_has(
                &subject,
                &[
                    "% off",
                    "sale",
                    "last chance",
                    "free shipping",
                    "limited time",
                    "discount",
                ],
            ))
    {
        return Some(Category::Marketing);
    }

    if !f.is_automated
        && !f.sender_known
        && subject_has(
            &subject,
            &[
                "quick call",
                "partnership",
                "sponsor",
                "demo",
                "collab",
                "guest post",
            ],
        )
    {
        return Some(Category::Pitch);
    }

    None
}

fn label_id_for(conn: &Connection, category: Category) -> Result<Option<i64>> {
    use rusqlite::OptionalExtension;
    Ok(conn
        .query_row(
            "SELECT id FROM labels WHERE keyword = ?1 AND is_auto = 1",
            params![category.keyword()],
            |r| r.get(0),
        )
        .optional()?)
}

pub fn sender_known(conn: &Connection, from_addr: &str) -> bool {
    conn.query_row(
        "SELECT 1 FROM contacts WHERE email = ?1 COLLATE NOCASE UNION ALL SELECT 1 FROM contact_values WHERE kind='EMAIL' AND value=?1 COLLATE NOCASE LIMIT 1",
        params![from_addr],
        |_| Ok(()),
    )
    .is_ok()
}

/// Classify one stored message and write its membership. Returns true when a
/// category was applied.
pub fn apply(conn: &Connection, msg_id: i64, facts: &MessageFacts) -> Result<bool> {
    let Some(category) = classify(facts) else {
        return Ok(false);
    };
    let Some(label_id) = label_id_for(conn, category)? else {
        return Ok(false);
    };
    crate::db::repo::labels::add_to_message(conn, msg_id, label_id)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts<'a>(
        from_addr: &'a str,
        subject: &'a str,
        is_automated: bool,
        has_list_headers: bool,
    ) -> MessageFacts<'a> {
        MessageFacts {
            from_addr,
            subject,
            is_automated,
            has_list_headers,
            sender_known: false,
        }
    }

    #[test]
    fn keyword_roundtrips() {
        for cat in [
            Category::Marketing,
            Category::News,
            Category::Social,
            Category::Pitch,
        ] {
            assert_eq!(Category::from_keyword(cat.keyword()), Some(cat));
        }
        assert_eq!(Category::from_keyword(""), None);
        // The display name is NOT the keyword (regression: the AI cache stores
        // keywords, so reading them back as display names loses the category).
        assert_eq!(Category::from_keyword("Marketing"), None);
    }

    #[test]
    fn social_by_domain() {
        assert_eq!(
            classify(&facts(
                "notify@linkedin.com",
                "You appeared in searches",
                true,
                true
            )),
            Some(Category::Social)
        );
        // subdomains too
        assert_eq!(
            classify(&facts(
                "x@e.facebookmail.com",
                "New friend request",
                true,
                false
            )),
            Some(Category::Social)
        );
    }

    #[test]
    fn news_by_platform_and_subject() {
        assert_eq!(
            classify(&facts("author@substack.com", "My essay", true, true)),
            Some(Category::News)
        );
        assert_eq!(
            classify(&facts(
                "hello@somecorp.com",
                "Issue #42: the roundup",
                false,
                true
            )),
            Some(Category::News)
        );
    }

    #[test]
    fn marketing_by_promo_signals() {
        assert_eq!(
            classify(&facts(
                "offers@shop.example",
                "50% off everything",
                true,
                true
            )),
            Some(Category::Marketing)
        );
        assert_eq!(
            classify(&facts(
                "bounce@em1234.rsgsv.net",
                "Your cart misses you",
                true,
                true
            )),
            Some(Category::Marketing)
        );
    }

    #[test]
    fn pitch_only_for_unknown_human_senders() {
        assert_eq!(
            classify(&facts(
                "bd@agency.io",
                "Quick call this week?",
                false,
                false
            )),
            Some(Category::Pitch)
        );
        // known sender: not a pitch
        let mut f = facts("bd@agency.io", "Quick call this week?", false, false);
        f.sender_known = true;
        assert_eq!(classify(&f), None);
    }

    #[test]
    fn plain_human_mail_unlabeled() {
        assert_eq!(
            classify(&facts("alice@example.com", "Lunch tomorrow?", false, false)),
            None
        );
    }

    #[test]
    fn apply_writes_membership_against_seeded_db() {
        use crate::db::testutil;
        let c = testutil::conn();
        testutil::seed_account(&c);
        let (_t, msg) =
            testutil::seed_message(&c, "offers@shop.example", "50% off everything", true);

        let f = MessageFacts {
            from_addr: "offers@shop.example",
            subject: "50% off everything",
            is_automated: true,
            has_list_headers: true,
            sender_known: false,
        };
        assert!(apply(&c, msg, &f).unwrap());
        // idempotent (INSERT OR IGNORE)
        assert!(apply(&c, msg, &f).unwrap());

        let n: i64 = c
            .query_row(
                "SELECT COUNT(*) FROM message_labels ml JOIN labels l ON l.id = ml.label_id
                 WHERE ml.message_id = ?1 AND l.keyword = 'FlectarMailAutoMarketing'",
                rusqlite::params![msg],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn sender_known_consults_contacts() {
        use crate::db::testutil;
        let c = testutil::conn();
        testutil::seed_account(&c);
        assert!(!sender_known(&c, "bd@agency.io"));
        crate::db::repo::contacts::harvest(
            &c,
            1,
            &crate::models::Address {
                name: None,
                email: "bd@agency.io".into(),
            },
            true,
            100,
        )
        .unwrap();
        assert!(
            sender_known(&c, "BD@AGENCY.IO"),
            "contact match must be case-insensitive"
        );
    }

    #[test]
    fn apply_without_category_is_noop() {
        use crate::db::testutil;
        let c = testutil::conn();
        testutil::seed_account(&c);
        let (_t, msg) = testutil::seed_message(&c, "alice@example.com", "Lunch tomorrow?", false);
        let f = MessageFacts {
            from_addr: "alice@example.com",
            subject: "Lunch tomorrow?",
            is_automated: false,
            has_list_headers: false,
            sender_known: false,
        };
        assert!(!apply(&c, msg, &f).unwrap());
        let n: i64 = c
            .query_row("SELECT COUNT(*) FROM message_labels", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 0);
    }
}
