//! Linux title-bar preferences and window-manager requests.
//!
//! Button order and title-bar click actions follow the desktop. GNOME, and
//! other desktops served by xdg-desktop-portal-gtk, publish them through the
//! portal's `org.gnome.desktop.wm.preferences` namespace with live updates.
//! KDE's portal does not serve them, so Plasma sessions read KWin's own
//! configuration, which kde-gtk-config also mirrors into GTK's settings.ini.

use super::TitleBarAction;
use crate::{AppWindow, WindowChrome};
use futures_util::StreamExt;
use slint::ComponentHandle;
use slint::winit_030::{WinitWindowAccessor, winit};
use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::time::Duration;
use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};

const WM_PREFERENCES: &str = "org.gnome.desktop.wm.preferences";
const BUTTON_LAYOUT: &str = "button-layout";
const DOUBLE_CLICK: &str = "action-double-click-titlebar";
const MIDDLE_CLICK: &str = "action-middle-click-titlebar";
const RIGHT_CLICK: &str = "action-right-click-titlebar";
const PORTAL_TIMEOUT: Duration = Duration::from_secs(3);

/// Window-manager preferences expressed in GNOME's vocabulary.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Preferences {
    button_layout: Option<String>,
    double_click: Option<String>,
    middle_click: Option<String>,
    right_click: Option<String>,
}

impl Preferences {
    fn set(&mut self, key: &str, value: String) -> bool {
        let slot = match key {
            BUTTON_LAYOUT => &mut self.button_layout,
            DOUBLE_CLICK => &mut self.double_click,
            MIDDLE_CLICK => &mut self.middle_click,
            RIGHT_CLICK => &mut self.right_click,
            _ => return false,
        };
        *slot = Some(value);
        true
    }

    /// Fill unset values from a lower-precedence source.
    fn or(self, fallback: Self) -> Self {
        Self {
            button_layout: self.button_layout.or(fallback.button_layout),
            double_click: self.double_click.or(fallback.double_click),
            middle_click: self.middle_click.or(fallback.middle_click),
            right_click: self.right_click.or(fallback.right_click),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Desktop {
    Gnome,
    Kde,
    Other,
}

impl Desktop {
    fn current() -> Self {
        let desktops = std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default();
        let has = |name: &str| {
            desktops
                .split(':')
                .any(|desktop| desktop.eq_ignore_ascii_case(name))
        };
        if has("KDE") {
            Self::Kde
        } else if has("GNOME") {
            Self::Gnome
        } else {
            Self::Other
        }
    }

    /// The desktop's own default when no configuration is readable.
    fn default_preferences(self) -> Preferences {
        Preferences {
            button_layout: Some(
                match self {
                    // gsettings-desktop-schemas ships "appmenu:close".
                    Self::Gnome => "appmenu:close",
                    // KWin's default ButtonsOnLeft=MS, ButtonsOnRight=HIAX.
                    Self::Kde => "icon:minimize,maximize,close",
                    Self::Other => ":minimize,maximize,close",
                }
                .to_owned(),
            ),
            double_click: Some("toggle-maximize".to_owned()),
            middle_click: Some("none".to_owned()),
            right_click: Some("menu".to_owned()),
        }
    }
}

#[derive(Default)]
struct State {
    portal: Preferences,
    effective: Preferences,
}

thread_local! {
    static STATE: RefCell<State> = RefCell::default();
}

pub(super) fn register(app: &AppWindow, runtime: &tokio::runtime::Handle) {
    let weak = app.as_weak();
    runtime.spawn(async move {
        let Ok(Ok(settings)) =
            tokio::time::timeout(PORTAL_TIMEOUT, ashpd::desktop::settings::Settings::new()).await
        else {
            return;
        };
        let mut preferences = Preferences::default();
        for key in [BUTTON_LAYOUT, DOUBLE_CLICK, MIDDLE_CLICK, RIGHT_CLICK] {
            if let Ok(Ok(value)) =
                tokio::time::timeout(PORTAL_TIMEOUT, settings.read::<String>(WM_PREFERENCES, key))
                    .await
            {
                preferences.set(key, value);
            }
        }
        publish(&weak, preferences.clone());

        let Ok(mut changes) = settings.receive_setting_changed().await else {
            return;
        };
        while let Some(setting) = changes.next().await {
            if setting.namespace() != WM_PREFERENCES {
                continue;
            }
            let ashpd::zvariant::Value::Str(value) = &**setting.value() else {
                continue;
            };
            if preferences.set(setting.key(), value.as_str().to_owned()) {
                publish(&weak, preferences.clone());
            }
        }
    });
}

fn publish(weak: &slint::Weak<AppWindow>, preferences: Preferences) {
    let _ = weak.upgrade_in_event_loop(move |app| {
        STATE.with_borrow_mut(|state| state.portal = preferences);
        if app.global::<WindowChrome>().get_integrated() {
            refresh(&app);
        }
    });
}

/// Resolve the current preferences and update the title bar. Files are read
/// again on focus so desktops without a settings portal still pick up changes.
pub(super) fn refresh(app: &AppWindow) {
    let desktop = Desktop::current();
    let config = config_home();
    let read = |path: PathBuf| std::fs::read_to_string(path).unwrap_or_default();
    let gtk = gtk_preferences(&read(config.join("gtk-4.0/settings.ini")))
        .or(gtk_preferences(&read(config.join("gtk-3.0/settings.ini"))));
    let effective = STATE.with_borrow_mut(|state| {
        let portal = state.portal.clone();
        state.effective = match desktop {
            // KWin is the source of truth on Plasma; the GTK copies of its
            // button order are derived from it.
            Desktop::Kde => kwin_preferences(&read(config.join("kwinrc")))
                .or(portal)
                .or(gtk)
                .or(desktop.default_preferences()),
            Desktop::Gnome | Desktop::Other => {
                portal.or(gtk).or(desktop.default_preferences())
            }
        };
        state.effective.clone()
    });

    let layout = ButtonLayout::parse(effective.button_layout.as_deref().unwrap_or_default());
    let chrome = app.global::<WindowChrome>();
    // KWin draws Breeze glyph buttons; GTK desktops use Adwaita's circles.
    chrome.set_linux_style(
        match desktop {
            Desktop::Kde => "breeze",
            Desktop::Gnome | Desktop::Other => "adwaita",
        }
        .into(),
    );
    chrome.set_leading_controls(controls_model(&layout.leading));
    chrome.set_trailing_controls(controls_model(&layout.trailing));
}

fn controls_model(controls: &[Control]) -> slint::ModelRc<slint::SharedString> {
    let controls = controls
        .iter()
        .map(|control| slint::SharedString::from(control.key()))
        .collect::<Vec<_>>();
    slint::ModelRc::new(slint::VecModel::from(controls))
}

fn config_home() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|home| Path::new(&home).join(".config")))
        .unwrap_or_default()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Control {
    Minimize,
    Maximize,
    Close,
}

impl Control {
    fn parse(token: &str) -> Option<Self> {
        match token.trim() {
            "minimize" => Some(Self::Minimize),
            "maximize" => Some(Self::Maximize),
            "close" => Some(Self::Close),
            // "appmenu", "menu", "spacer" and future names are ignored, as
            // GTK and Mutter do. The window icon ("icon", KWin's menu button)
            // is left out too: in a toolbar it repeats the app's own identity,
            // and the window menu stays available on secondary click.
            _ => None,
        }
    }

    fn key(self) -> &'static str {
        match self {
            Self::Minimize => "minimize",
            Self::Maximize => "maximize",
            Self::Close => "close",
        }
    }
}

#[derive(Debug, Default, PartialEq, Eq)]
struct ButtonLayout {
    leading: Vec<Control>,
    trailing: Vec<Control>,
}

impl ButtonLayout {
    /// Parse a GTK decoration layout such as "icon:minimize,maximize,close".
    /// Text before the first colon is the leading side. A layout without a
    /// colon places its buttons on the trailing side, and duplicate buttons
    /// are dropped because a window has only one of each control.
    fn parse(layout: &str) -> Self {
        let (leading, trailing) = layout.split_once(':').unwrap_or(("", layout));
        let mut seen = Vec::new();
        let mut side = |value: &str| {
            value
                .split(',')
                .filter_map(Control::parse)
                .filter(|control| {
                    let new = !seen.contains(control);
                    seen.push(*control);
                    new
                })
                .collect()
        };
        let leading = side(leading);
        let trailing = side(trailing);
        Self { leading, trailing }
    }
}

/// Read title-bar preferences from a GTK settings.ini `[Settings]` group.
fn gtk_preferences(contents: &str) -> Preferences {
    let mut preferences = Preferences::default();
    for (key, value) in ini_group(contents, "Settings") {
        let key = match key {
            "gtk-decoration-layout" => BUTTON_LAYOUT,
            "gtk-titlebar-double-click" => DOUBLE_CLICK,
            "gtk-titlebar-middle-click" => MIDDLE_CLICK,
            "gtk-titlebar-right-click" => RIGHT_CLICK,
            _ => continue,
        };
        preferences.set(key, value.trim_matches('"').to_owned());
    }
    preferences
}

/// Translate KWin's decoration and title-bar mouse settings.
fn kwin_preferences(contents: &str) -> Preferences {
    let mut left = None;
    let mut right = None;
    for (key, value) in ini_group(contents, "org.kde.kdecoration2") {
        match key {
            "ButtonsOnLeft" => left = Some(value),
            "ButtonsOnRight" => right = Some(value),
            _ => {}
        }
    }
    let mut preferences = Preferences::default();
    if left.is_some() || right.is_some() {
        // Same letters kde-gtk-config maps: X close, I minimize,
        // A maximize, M the window-menu icon. Others have no GTK equivalent.
        let gtk = |buttons: &str| {
            buttons
                .chars()
                .filter_map(|button| match button {
                    'X' => Some("close"),
                    'I' => Some("minimize"),
                    'A' => Some("maximize"),
                    'M' => Some("icon"),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join(",")
        };
        preferences.button_layout = Some(format!(
            "{}:{}",
            gtk(left.unwrap_or("MS")),
            gtk(right.unwrap_or("HIAX"))
        ));
    }
    for (key, value) in ini_group(contents, "Windows") {
        if key == "TitlebarDoubleClickCommand" {
            preferences.double_click = Some(
                match value {
                    "Minimize" => "minimize",
                    "Lower" => "lower",
                    "Close" => "close",
                    "Nothing" | "Shade" | "OnAllDesktops" | "KeepAbove" | "KeepBelow" => "none",
                    // "Maximize" and its horizontal/vertical variants.
                    _ => "toggle-maximize",
                }
                .to_owned(),
            );
        }
    }
    for (key, value) in ini_group(contents, "MouseBindings") {
        let slot = match key {
            "CommandActiveTitlebar2" => &mut preferences.middle_click,
            "CommandActiveTitlebar3" => &mut preferences.right_click,
            _ => continue,
        };
        *slot = Some(
            match value {
                "Operations menu" => "menu",
                "Minimize" => "minimize",
                "Lower" => "lower",
                "Close" => "close",
                _ => "none",
            }
            .to_owned(),
        );
    }
    preferences
}

/// Entries of one group in a freedesktop/GLib key file.
fn ini_group<'a>(contents: &'a str, group: &str) -> impl Iterator<Item = (&'a str, &'a str)> {
    let mut in_group = false;
    contents.lines().filter_map(move |line| {
        let line = line.trim();
        if let Some(name) = line.strip_prefix('[').and_then(|line| line.strip_suffix(']')) {
            in_group = name == group;
            return None;
        }
        if !in_group || line.starts_with('#') || line.starts_with(';') {
            return None;
        }
        let (key, value) = line.split_once('=')?;
        Some((key.trim(), value.trim()))
    })
}

pub(super) fn title_bar_action(app: &AppWindow, gesture: &str) -> Option<TitleBarAction> {
    let x11 = x11_window(app.window()).is_some();
    let action = STATE.with_borrow(|state| match gesture {
        "double" => state.effective.double_click.clone(),
        "middle" => state.effective.middle_click.clone(),
        // Wayland compositors validate the serial of the button press. X11
        // window managers must grab the pointer, which the implicit press
        // grab prevents until release.
        "secondary-press" if !x11 => state.effective.right_click.clone(),
        "secondary-release" if x11 => state.effective.right_click.clone(),
        _ => None,
    })?;
    match action.as_str() {
        // GTK treats the horizontal and vertical variants as a plain toggle.
        action if action.starts_with("toggle-maximize") => Some(TitleBarAction::ToggleMaximize),
        "minimize" => Some(TitleBarAction::Minimize),
        "close" => Some(TitleBarAction::Close),
        "menu" => Some(TitleBarAction::Menu),
        "lower" => Some(TitleBarAction::Lower),
        _ => None,
    }
}

/// Request the window menu through X11. Returns false on Wayland, where
/// winit implements the request itself.
pub(super) fn show_x11_window_menu(window: &slint::Window) -> bool {
    let Some(id) = x11_window(window) else {
        return false;
    };
    if let Err(error) = x11::show_window_menu(id) {
        eprintln!("could not open window menu: {error}");
    }
    true
}

/// Wayland has no request to lower a window, so this is X11 only.
pub(super) fn lower(window: &slint::Window) {
    if let Some(id) = x11_window(window)
        && let Err(error) = x11::lower(id)
    {
        eprintln!("could not lower window: {error}");
    }
}

fn x11_window(window: &slint::Window) -> Option<u32> {
    window
        .with_winit_window(|window| match window.window_handle().ok()?.as_raw() {
            RawWindowHandle::Xlib(handle) => u32::try_from(handle.window).ok(),
            RawWindowHandle::Xcb(handle) => Some(handle.window.get()),
            _ => None,
        })
        .flatten()
}

mod x11 {
    //! Window-manager requests that winit does not expose on X11. A short
    //! separate connection is enough: both requests address the window by id.

    use x11rb::connection::Connection;
    use x11rb::protocol::xproto::{
        ClientMessageEvent, ConfigureWindowAux, ConnectionExt, EventMask, StackMode,
    };

    type Result = std::result::Result<(), Box<dyn std::error::Error>>;

    /// Ask the window manager for the window menu at the pointer, as GTK does
    /// for client-side decorations. Mutter and KWin implement this message.
    pub(super) fn show_window_menu(window: u32) -> Result {
        let (connection, screen) = x11rb::connect(None)?;
        let root = connection.setup().roots[screen].root;
        let message = connection
            .intern_atom(false, b"_GTK_SHOW_WINDOW_MENU")?
            .reply()?
            .atom;
        let pointer = connection.query_pointer(window)?.reply()?;
        connection.ungrab_pointer(x11rb::CURRENT_TIME)?;
        // XIAllMasterDevices: the window manager uses the current pointer.
        let device = 1;
        let event = ClientMessageEvent::new(
            32,
            window,
            message,
            [
                device,
                pointer.root_x as u32,
                pointer.root_y as u32,
                0,
                0,
            ],
        );
        connection.send_event(
            false,
            root,
            EventMask::SUBSTRUCTURE_REDIRECT | EventMask::SUBSTRUCTURE_NOTIFY,
            event,
        )?;
        connection.flush()?;
        Ok(())
    }

    pub(super) fn lower(window: u32) -> Result {
        let (connection, _) = x11rb::connect(None)?;
        connection.configure_window(
            window,
            &ConfigureWindowAux::new().stack_mode(StackMode::BELOW),
        )?;
        connection.flush()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use Control::{Close, Maximize, Minimize};

    #[test]
    fn button_layout_follows_gtk_sides_and_ignores_unknown_names() {
        let layout = |value| ButtonLayout::parse(value);
        assert_eq!(
            layout("appmenu:minimize,maximize,close"),
            ButtonLayout {
                leading: vec![],
                trailing: vec![Minimize, Maximize, Close],
            }
        );
        assert_eq!(
            layout("close,minimize,maximize:"),
            ButtonLayout {
                leading: vec![Close, Minimize, Maximize],
                trailing: vec![],
            }
        );
        assert_eq!(
            layout("icon:minimize,spacer,maximize, close"),
            ButtonLayout {
                leading: vec![],
                trailing: vec![Minimize, Maximize, Close],
            }
        );
        assert_eq!(layout("appmenu:close").trailing, vec![Close]);
        assert_eq!(layout("close").trailing, vec![Close]);
        assert_eq!(layout(":"), ButtonLayout::default());
        assert_eq!(layout(""), ButtonLayout::default());
        assert_eq!(
            layout("close:maximize,close,minimize"),
            ButtonLayout {
                leading: vec![Close],
                trailing: vec![Maximize, Minimize],
            }
        );
    }

    #[test]
    fn gtk_settings_are_read_from_the_settings_group() {
        let preferences = gtk_preferences(
            "[Other]\ngtk-decoration-layout=close:\n\n[Settings]\n# comment\n\
             gtk-decoration-layout = icon:minimize,maximize,close\n\
             gtk-titlebar-middle-click=lower\n",
        );
        assert_eq!(
            preferences,
            Preferences {
                button_layout: Some("icon:minimize,maximize,close".into()),
                middle_click: Some("lower".into()),
                ..Default::default()
            }
        );
    }

    #[test]
    fn kwin_configuration_maps_to_gtk_vocabulary() {
        let preferences = kwin_preferences(
            "[org.kde.kdecoration2]\nButtonsOnLeft=XIA\nButtonsOnRight=HSF\n\
             [Windows]\nTitlebarDoubleClickCommand=Minimize\n\
             [MouseBindings]\nCommandActiveTitlebar2=Lower\n\
             CommandActiveTitlebar3=Operations menu\n",
        );
        assert_eq!(
            preferences,
            Preferences {
                button_layout: Some("close,minimize,maximize:".into()),
                double_click: Some("minimize".into()),
                middle_click: Some("lower".into()),
                right_click: Some("menu".into()),
            }
        );
        assert_eq!(kwin_preferences(""), Preferences::default());
        assert_eq!(
            kwin_preferences("[org.kde.kdecoration2]\nButtonsOnLeft=M\n").button_layout,
            Some("icon:minimize,maximize,close".into())
        );
    }

    #[test]
    fn higher_precedence_sources_win_per_key() {
        let portal = Preferences {
            button_layout: Some(":close".into()),
            ..Default::default()
        };
        let resolved = portal.or(Desktop::Gnome.default_preferences());
        assert_eq!(resolved.button_layout.as_deref(), Some(":close"));
        assert_eq!(resolved.double_click.as_deref(), Some("toggle-maximize"));
        assert_eq!(resolved.right_click.as_deref(), Some("menu"));
    }
}
