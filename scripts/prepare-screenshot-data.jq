def favicon_path(address):
  ($favicon_dir + "/" + (address | split("@") | last | ascii_downcase) + ".png");
.theme_mode = $theme
| .["MotionSettings.enabled"] = false
| .workspace_layout = $workspace_layout
| .screenshot_theme_preset = $theme_preset
| .active_view = $active_view
# The app always has at least one tab; show the same strip as the frameless
# screenshots: the current view, a background conversation and the calendar.
# The tabs showcase adds more conversations and every other workspace.
| .["TabsUi.rows"] = (if $tab_strip == "showcase" then [
    {"id": 1, "workspace": $active_view, "title": "", "active": true},
    {"id": 2, "workspace": "mail", "title": .emails[1].subject, "active": false},
    {"id": 3, "workspace": "mail", "title": .emails[2].subject, "active": false},
    {"id": 4, "workspace": "calendar", "title": "", "active": false},
    {"id": 5, "workspace": "contacts", "title": "", "active": false},
    {"id": 6, "workspace": "files", "title": "", "active": false}
  ] else [
    {"id": 1, "workspace": $active_view, "title": "", "active": true},
    {"id": 2, "workspace": "mail", "title": .emails[1].subject, "active": false},
    {"id": 3, "workspace": (if $active_view == "calendar" then "mail" else "calendar" end), "title": "", "active": false}
  ] end)
| if $window_frame == "" then . else
    .["WindowChrome.platform"] = $window_frame
    | .["WindowChrome.preference"] = "integrated"
    | .["WindowChrome.integrated"] = true
  end
| .startup_hydrated = true
| .calendar_view_mode = $calendar_mode
| .calendar_period_title = (if $calendar_mode == "agenda" then .calendar_month_title else .calendar_period_title end)
| .settings_open = ($settings_tab != "")
| .settings_tab = (if $settings_tab == "" then (.settings_tab // "General") else $settings_tab end)
| .text_mode = false
| .show_account_markers = $show_account_markers
| .storage_loading = false
| .storage_loaded = true
| .storage_total = "18.42 GB"
| .storage_error = ""
| .storage_usage = [
    {"kind": "mail", "size": "9.84 GB", "fraction": 0.5342, "offset": 0, "color": "#3b82f6"},
    {"kind": "attachments", "size": "5.21 GB", "fraction": 0.2828, "offset": 0.5342, "color": "#f59e0b"},
    {"kind": "files", "size": "1.76 GB", "fraction": 0.0955, "offset": 0.817, "color": "#22c55e"},
    {"kind": "databases", "size": "1.34 GB", "fraction": 0.0727, "offset": 0.9125, "color": "#a855f7"},
    {"kind": "other", "size": "279.6 MB", "fraction": 0.0148, "offset": 0.9852, "color": "#94a3b8"}
  ]
# The reader's rendering of demo-email.html for this preset and scheme.
| if $email == null then . else . + $email end
| .selected_favicon = favicon_path(.selected_address)
| .selected_has_favicon = true
| .contacts |= map(.suggested = (.suggested // false)
    | .["has-avatar"] = false
    | .source = "Personal")
| .["contact-source"] = "Personal"
| .["contact-sync"] = "synced"
| .["contact-emails"] = [{key: "", value: .contact_email, label: "WORK", preference: 1}]
| .["contact-phones"] = [{key: "", value: .contact_phone, label: "CELL", preference: 100}]
| del(.contact_email, .contact_phone)
| .connected_accounts |= map(
    .mail_protocol = (.mail_protocol // "")
    | .profile_id = (if .id == 2 then "support" else "work" end)
    | .profile_name = (if .id == 2 then "Support" else "Work" end)
    | .profile_color = (if .id == 2 then "#ea580c" else "#8b5cf6" end)
    | .has_profile = true
    | .has_account_color = false
    | .jmap_url = (.jmap_url // "")
    | .incoming_security = (.incoming_security // "tls")
    | .smtp_security = (.smtp_security // "tls")
    | .trusted_certificate_pem = (.trusted_certificate_pem // "")
    | .mail_history = (.mail_history // "sixMonths")
    | .keep_on_server = (.keep_on_server // "always")
    | .server_keeps_mail_days = (.server_keeps_mail_days // -1)
    | .caldav_url = (.caldav_url // "")
    | .caldav_username = (.caldav_username // "")
    | .carddav_connected = (.carddav_connected // false)
    | .carddav_enabled = (.carddav_enabled // false)
    | .carddav_status = (.carddav_status // "")
    | .carddav_url = (.carddav_url // "")
    | .carddav_username = (.carddav_username // "")
  )
| .mail_profiles = [
    {"id": "work", "name": "Work", "color": "#8b5cf6", "account_count": 1},
    {"id": "support", "name": "Support", "color": "#ea580c", "account_count": 1}
  ]
| .sidebar_rows |= map(
    .show_row = true
    | .reveal_row = false
    | .mailbox.is_selectable = (if .mailbox.is_selectable == null then true else .mailbox.is_selectable end)
    | .mailbox.can_create_children = (.mailbox.can_create_children // false)
    | .mailbox.can_rename = (.mailbox.can_rename // false)
    | .mailbox.can_delete = (.mailbox.can_delete // false)
    | .label.account_id = (.label.account_id // -1)
    | .label.account_name = (.label.account_name // "")
    | .label.display_name = (.label.display_name // .label.name // "")
    | .label.depth = (.label.depth // 0)
    | .label.has_children = (.label.has_children // false)
    | .label.expanded = (if .label.expanded == null then true else .label.expanded end)
    | .label.can_create_children = (.label.can_create_children // false)
    | .label.is_global = (.label.is_global // false)
  )
| .mail_labels |= map(
    .account_id = (.account_id // -1)
    | .account_name = (.account_name // "")
    | .display_name = (.display_name // .name // "")
    | .depth = (.depth // 0)
    | .has_children = (.has_children // false)
    | .expanded = (if .expanded == null then true else .expanded end)
    | .can_create_children = (.can_create_children // false)
    | .is_global = (.is_global // false)
  )
| .mail_label_results |= map(
    .account_id = (.account_id // -1)
    | .account_name = (.account_name // "")
    | .display_name = (.display_name // .name // "")
    | .depth = (.depth // 0)
    | .has_children = (.has_children // false)
    | .expanded = (if .expanded == null then true else .expanded end)
    | .can_create_children = (.can_create_children // false)
    | .is_global = (.is_global // false)
  )
| (.thread_messages // [] | length) as $thread_count
| .emails |= map(
    .has_replied = (.has_replied // false)
    | .message_count = (if $show_threads and .selected then ([1, $thread_count] | max) else (.message_count // 1) end)
    | .checked = (.checked // false)
    | .account_id = (if .account == "Flectar Support" then 2 else 1 end)
    | .account_color = (if .account == "Flectar Support" then "#ea580c" else "#8b5cf6" end)
    | .account_profile = (if .account == "Flectar Support" then "Support" else "Work" end)
    | .show_account_marker = $show_account_markers
    | .labels |= map(
        .account_id = (.account_id // -1)
        | .account_name = (.account_name // "")
        | .display_name = (.display_name // .name // "")
        | .depth = (.depth // 0)
        | .has_children = (.has_children // false)
        | .expanded = (if .expanded == null then true else .expanded end)
        | .can_create_children = (.can_create_children // false)
        | .is_global = (.is_global // false)
      )
    | .favicon = favicon_path(.address)
    | .favicon_small = favicon_path(.address)
    | .has_favicon = true
  )
# The viewer runs no Rust projection or accordion initialization.
# Build the ungrouped list from the fully prepared email rows.
| .mail_list_entries = (.emails | to_entries | map({
    is_header: false,
    group_key: "",
    group_kind: "",
    group_count: 0,
    expanded: false,
    show_row: true,
    reveal_row: false,
    email_index: .key,
    email: .value
  }))
| .selected_address as $selected_address
| .thread_messages = (if $show_threads then (.thread_messages // []) else [] end)
| .thread_messages |= map(
    .favicon = favicon_path(if .outgoing then $selected_address else .address end)
    | .favicon_small = favicon_path(if .outgoing then $selected_address else .address end)
    | .has_favicon = (.outgoing | not)
  )
| .selected_thread_index = (if $show_threads then (.selected_thread_index // 0) else 0 end)
