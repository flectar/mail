use crate::error::Result;
use crate::models::Settings;
use rusqlite::{Connection, OptionalExtension, params};

pub fn get(conn: &Connection) -> Result<Settings> {
    let json: Option<String> = conn
        .query_row(
            "SELECT value FROM app_settings WHERE key = 'settings'",
            [],
            |r| r.get(0),
        )
        .optional()?;
    match json {
        Some(json) => Ok(serde_json::from_str(&json)?),
        None => Ok(Settings::default()),
    }
}

pub fn set(conn: &Connection, settings: &Settings) -> Result<()> {
    conn.execute(
        "INSERT INTO app_settings (key, value) VALUES ('settings', ?1)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![serde_json::to_string(settings)?],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::testutil;

    #[test]
    fn defaults_when_unset_and_roundtrip() {
        let c = testutil::conn();
        let d = get(&c).unwrap();
        assert!(d.notifications_enabled);
        assert_eq!(d.sync_interval_minutes, 5);
        assert!(d.auto_advance);
        assert!(d.auto_labels_enabled);
        assert!(d.mark_read_on_open);
        assert!(!d.load_remote_images);
        assert!(!d.monochrome_sidebar_icons);
        assert!(d.show_avatars);
        assert!(d.collect_outgoing_contacts);
        assert!(!d.collect_incoming_contacts);
        assert!(d.suggest_learned_contacts);
        assert_eq!(d.workspace_layout, "default");
        assert_eq!(d.calendar_week_start, "monday");
        assert_eq!(d.theme_preset, "default");
        assert_eq!(d.custom_theme.light_primary, "#0969DA");
        assert!(d.mail_profiles.is_empty());
        assert!(!d.show_account_badges);

        let mut s = d.clone();
        s.theme = "carbon".into();
        s.calendar_week_start = "sunday".into();
        s.notifications_enabled = false;
        s.monochrome_sidebar_icons = true;
        s.show_avatars = false;
        s.workspace_layout = "minimal".into();
        s.theme_preset = "teal".into();
        s.custom_theme.light_primary = "#006B7A".into();
        s.signature_list.push(crate::models::Signature {
            id: "a".into(),
            account_id: 1,
            name: "Work".into(),
            html: "<b>Dean</b>".into(),
        });
        s.signature_defaults.insert(
            "1".into(),
            crate::models::SignatureDefaults {
                new_id: Some("a".into()),
                reply_id: None,
            },
        );
        s.ai_automation_rules.push(crate::models::AiAutomationRule {
            id: "invoice".into(),
            name: "Invoices".into(),
            source_prompt: "For vendor invoices, move to Important".into(),
            instruction: "It is a vendor invoice".into(),
            enabled: true,
            actions: vec![crate::models::AiAutomationAction {
                kind: "route_to".into(),
                value: "important".into(),
            }],
        });
        s.mail_profiles.push(crate::models::MailProfile {
            id: "work".into(),
            name: "Work".into(),
            color: "#3B82F6".into(),
            account_ids: vec![1, 2],
        });
        set(&c, &s).unwrap();

        let back = get(&c).unwrap();
        assert_eq!(back.theme, "carbon");
        assert_eq!(back.calendar_week_start, "sunday");
        assert!(!back.notifications_enabled);
        assert!(back.monochrome_sidebar_icons);
        assert!(!back.show_avatars);
        assert_eq!(back.workspace_layout, "minimal");
        assert_eq!(back.theme_preset, "teal");
        assert_eq!(back.custom_theme.light_primary, "#006B7A");
        assert_eq!(back.signature_list.len(), 1);
        assert_eq!(back.signature_list[0].html, "<b>Dean</b>");
        assert_eq!(back.ai_automation_rules, s.ai_automation_rules);
        assert_eq!(back.mail_profiles, s.mail_profiles);
        assert_eq!(
            back.signature_defaults
                .get("1")
                .and_then(|d| d.new_id.as_deref()),
            Some("a")
        );
    }

    /// Blobs written before new fields existed must deserialize with defaults.
    #[test]
    fn old_blob_gets_field_defaults() {
        let c = testutil::conn();
        c.execute(
            "INSERT INTO app_settings (key, value) VALUES ('settings',
             '{\"theme\":\"snow\",\"undoSendSeconds\":20,\"loadRemoteImages\":true}')",
            [],
        )
        .unwrap();
        let s = get(&c).unwrap();
        assert_eq!(s.theme, "snow");
        assert_eq!(s.undo_send_seconds, 20);
        assert!(s.load_remote_images);
        assert!(s.mark_read_on_open);
        assert!(!s.monochrome_sidebar_icons);
        assert!(s.show_avatars);
        assert!(s.collect_outgoing_contacts);
        assert!(!s.collect_incoming_contacts);
        assert!(s.suggest_learned_contacts);
        assert_eq!(s.workspace_layout, "default");
        assert_eq!(s.calendar_week_start, "monday");
        assert_eq!(s.theme_preset, "default");
        assert_eq!(s.custom_theme, crate::models::CustomTheme::default());
        assert!(s.notifications_enabled);
        assert_eq!(s.sync_interval_minutes, 5);
        assert!(s.auto_advance);
        assert!(s.auto_labels_enabled);
        assert!(s.ai_automation_rules.is_empty());
        assert!(s.mail_profiles.is_empty());
        assert!(!s.show_account_badges);
        assert!(!s.ai_model_instant.is_empty());
        assert!(!s.ai_model_cheap.is_empty());
        assert!(!s.ai_model_intelligent.is_empty());
    }

    #[test]
    fn malformed_settings_are_reported_instead_of_silently_reset() {
        let c = testutil::conn();
        c.execute(
            "INSERT INTO app_settings (key, value) VALUES ('settings', '{not-json')",
            [],
        )
        .unwrap();

        assert!(get(&c).is_err());
    }
}
