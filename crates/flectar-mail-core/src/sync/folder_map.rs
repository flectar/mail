//! Map IMAP folders to roles: RFC 6154 special-use attributes first, then
//! Gmail-style names, then common-name heuristics.

use crate::imap::RemoteFolder;
use crate::models::roles;
use crate::repo::folders::FolderPermissions;
use std::collections::{HashMap, HashSet};

/// The purpose RFC 6154 special-use attributes give the folder.
fn special_use(folder: &RemoteFolder) -> Option<&'static str> {
    for attr in &folder.attributes {
        // Attributes are Debug-formatted and lowercased. imap-proto parses
        // RFC 6154 special-use into typed variants ("sent", "junk", ...);
        // servers it doesn't recognize come through as extension("\\sent").
        if attr == "sent" || attr.contains("\\sent") {
            return Some(roles::SENT);
        }
        if attr == "drafts" || attr.contains("\\drafts") {
            return Some(roles::DRAFTS);
        }
        if attr == "trash" || attr.contains("\\trash") {
            return Some(roles::TRASH);
        }
        if attr == "junk" || attr.contains("\\junk") {
            return Some(roles::SPAM);
        }
        if attr == "archive" || attr.contains("\\archive") {
            return Some(roles::ARCHIVE);
        }
        if attr == "all" || attr.contains("\\all") {
            return Some(roles::ALL);
        }
    }
    None
}

pub fn detect_role(folder: &RemoteFolder) -> Option<&'static str> {
    if folder.name.eq_ignore_ascii_case("INBOX") {
        return Some(roles::INBOX);
    }
    if let Some(role) = special_use(folder) {
        return Some(role);
    }

    let last_segment = folder
        .delimiter
        .as_deref()
        .and_then(|d| folder.name.rsplit(d).next())
        .unwrap_or(&folder.name)
        .to_lowercase();
    let full = folder.name.to_lowercase();

    if full.starts_with("[gmail]") || full.starts_with("[google mail]") {
        return match last_segment.as_str() {
            "sent mail" => Some(roles::SENT),
            "drafts" => Some(roles::DRAFTS),
            "trash" | "bin" => Some(roles::TRASH),
            "spam" => Some(roles::SPAM),
            "all mail" => Some(roles::ALL),
            _ => None, // Starred/Important are views, not folders we sync
        };
    }

    match last_segment.as_str() {
        "sent" | "sent items" | "sent messages" | "sent-mail" => Some(roles::SENT),
        "drafts" | "draft" => Some(roles::DRAFTS),
        "trash" | "deleted" | "deleted items" | "deleted messages" | "bin" => Some(roles::TRASH),
        "spam" | "junk" | "junk mail" | "junk e-mail" => Some(roles::SPAM),
        "archive" | "archives" | "all mail" => Some(roles::ARCHIVE),
        _ => None,
    }
}

/// Should this folder be synced at all?
pub fn should_sync(folder: &RemoteFolder, role: Option<&str>) -> bool {
    if folder
        .attributes
        .iter()
        .any(|attribute| attribute.contains("noselect") || attribute.contains("nonexistent"))
    {
        return false;
    }
    let full = folder.name.to_lowercase();
    // Gmail: skip label-folders without roles to avoid duplicate downloads;
    // everything lives in All Mail + INBOX + special folders.
    let gmail_label = full.starts_with("[gmail]") || full.starts_with("[google mail]");
    !(gmail_label && role.is_none())
}

/// Project the mailbox operations that LIST can determine without issuing an
/// ACL round-trip for every mailbox. `\\NoInferiors` is authoritative for
/// child creation; rename/delete remain conservative for special-use and
/// non-selectable nodes, with the server still enforcing ACLs on mutation.
pub fn permissions(folder: &RemoteFolder, role: Option<&str>) -> FolderPermissions {
    let has_delimiter = folder
        .delimiter
        .as_deref()
        .is_some_and(|delimiter| !delimiter.is_empty());
    let no_inferiors = folder
        .attributes
        .iter()
        .any(|attribute| attribute.contains("noinferiors"));
    let selectable = should_sync(folder, role);
    FolderPermissions {
        can_create_children: has_delimiter && !no_inferiors,
        can_rename: selectable && role.is_none(),
        can_delete: selectable && role.is_none(),
    }
}

pub fn parent_name(folder: &RemoteFolder) -> Option<&str> {
    folder
        .delimiter
        .as_deref()
        .filter(|delimiter| !delimiter.is_empty())
        .and_then(|delimiter| folder.name.rsplit_once(delimiter))
        .map(|(parent, _)| parent)
        .filter(|parent| !parent.is_empty())
}

pub fn ancestor_names(folder: &RemoteFolder) -> Vec<String> {
    let Some(delimiter) = folder
        .delimiter
        .as_deref()
        .filter(|delimiter| !delimiter.is_empty())
    else {
        return Vec::new();
    };
    let mut ancestors = Vec::new();
    let mut current = folder.name.as_str();
    while let Some((parent, _)) = current.rsplit_once(delimiter) {
        if parent.is_empty() {
            break;
        }
        ancestors.push(parent.to_owned());
        current = parent;
    }
    ancestors
}

/// The folders an account that shows only subscribed folders lists, in LIST
/// order: those subscribed, the Inbox, the folders in `kept` (those rules
/// file into), and, for each purpose such as Sent or Trash that no subscribed
/// folder serves, one folder that does. Mail is filed into those, so without
/// one deleting would have no Trash to move mail to. That folder is one the
/// server marks for the purpose (RFC 6154) before one only named for it, and
/// the least nested, so another user's shared Trash doesn't stand in.
pub fn subscribed_only(
    remote: Vec<RemoteFolder>,
    subscribed: &HashSet<String>,
    kept: &HashSet<String>,
) -> Vec<RemoteFolder> {
    let purpose =
        |folder: &RemoteFolder| detect_role(folder).filter(|role| should_sync(folder, Some(role)));
    let listed = |folder: &RemoteFolder| {
        folder.name.eq_ignore_ascii_case("INBOX")
            || subscribed.contains(&folder.name)
            || kept.contains(&folder.name)
    };
    let served = remote
        .iter()
        .filter(|folder| listed(folder))
        .filter_map(purpose)
        .collect::<HashSet<_>>();
    // For each purpose left: (not marked for it, levels, LIST position).
    let mut stand_ins = HashMap::<&str, (bool, usize, usize)>::new();
    for (index, folder) in remote.iter().enumerate() {
        let Some(role) = purpose(folder).filter(|role| !served.contains(role)) else {
            continue;
        };
        let levels = folder
            .delimiter
            .as_deref()
            .filter(|delimiter| !delimiter.is_empty())
            .map_or(0, |delimiter| folder.name.matches(delimiter).count());
        let rank = (special_use(folder).is_none(), levels, index);
        stand_ins
            .entry(role)
            .and_modify(|best| *best = (*best).min(rank))
            .or_insert(rank);
    }
    let stand_ins = stand_ins
        .into_values()
        .map(|(_, _, index)| index)
        .collect::<HashSet<_>>();
    remote
        .into_iter()
        .enumerate()
        .filter(|(index, folder)| listed(folder) || stand_ins.contains(index))
        .map(|(_, folder)| folder)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn folder(name: &str, attributes: &[&str]) -> RemoteFolder {
        RemoteFolder {
            name: name.into(),
            delimiter: Some("/".into()),
            attributes: attributes
                .iter()
                .map(|attribute| (*attribute).into())
                .collect(),
        }
    }

    #[test]
    fn noselect_nodes_remain_valid_hierarchy_parents() {
        let year = folder("Archive/2025", &["noselect"]);
        let leaf = folder("Archive/2025/GitHub", &[]);

        assert!(!should_sync(&year, None));
        assert_eq!(parent_name(&year), Some("Archive"));
        assert_eq!(parent_name(&leaf), Some("Archive/2025"));
        assert!(permissions(&year, None).can_create_children);
        assert!(!permissions(&year, None).can_rename);
    }

    #[test]
    fn no_inferiors_disables_only_child_creation() {
        let leaf = folder("Projects", &["noinferiors"]);
        let capabilities = permissions(&leaf, None);
        assert!(!capabilities.can_create_children);
        assert!(capabilities.can_rename);
        assert!(capabilities.can_delete);
    }

    #[test]
    fn nonexistent_nodes_inherit_noselect_semantics() {
        let missing_parent = folder("Archive/2024", &["nonexistent", "haschildren"]);

        assert!(!should_sync(&missing_parent, None));
        assert_eq!(parent_name(&missing_parent), Some("Archive"));
    }

    #[test]
    fn top_level_and_flat_names_have_no_parent() {
        assert_eq!(parent_name(&folder("Archive", &[])), None);
        let mut flat = folder("Archive/2025", &[]);
        flat.delimiter = None;
        assert_eq!(parent_name(&flat), None);
    }

    #[test]
    fn missing_hierarchy_elements_can_be_reconstructed() {
        assert_eq!(
            ancestor_names(&folder("Archive/2025/GitHub", &[])),
            ["Archive/2025", "Archive"]
        );
    }

    #[test]
    fn subscribed_only_lists_subscriptions_and_what_mail_is_filed_into() {
        let names = |folders: Vec<RemoteFolder>| {
            folders
                .into_iter()
                .map(|folder| folder.name)
                .collect::<Vec<_>>()
        };
        let set = |names: &[&str]| names.iter().map(|name| (*name).to_owned()).collect();
        let remote = vec![
            folder("INBOX", &[]),
            folder("Projects", &[]),
            folder("Projects/Old", &[]),
            folder("Shared", &["\\noselect"]),
            folder("Shared/alice/Sent", &[]),
            folder("Sent", &["sent"]),
            folder("Shared/alice/Trash", &[]),
            folder("Deleted Items", &[]),
            folder("Archives", &[]),
            folder("Mail/Archive", &["archive"]),
            folder("Junk", &["junk"]),
            folder("Receipts", &[]),
        ];
        assert_eq!(
            names(subscribed_only(
                remote,
                &set(&["Projects", "Sent", "Gone"]),
                &set(&["Receipts"]),
            )),
            [
                // Unsubscribed, the Inbox still lists.
                "INBOX",
                "Projects",
                // A subscribed Sent serves sent mail, so another one found
                // first in LIST order isn't needed.
                "Sent",
                // Nothing subscribed serves deleted mail, archived mail or
                // junk, so one folder for each does: the least nested,
                // after one the server marks for the purpose.
                "Deleted Items",
                "Mail/Archive",
                "Junk",
                // A rule files into it.
                "Receipts",
            ]
        );
    }

    #[test]
    fn subscribed_only_skips_containers_for_a_purpose() {
        let kept = subscribed_only(
            vec![
                folder("Archive", &["\\noselect"]),
                folder("Archive/2024", &["archive"]),
            ],
            &HashSet::new(),
            &HashSet::new(),
        );
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].name, "Archive/2024");
    }
}
