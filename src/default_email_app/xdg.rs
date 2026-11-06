//! Linux desktops: the default handler of `x-scheme-handler/mailto`, as the
//! freedesktop.org MIME Applications Associations specification keeps it in
//! `mimeapps.list` files. GNOME Settings and KDE System Settings both write
//! the user's file the way `set_default` does.

use super::{Applied, Done, State, Status};
use std::{
    fs, io,
    path::{Path, PathBuf},
};

const MIME_TYPE: &str = "x-scheme-handler/mailto";
/// The desktop entry the packages and the AppImage's `--integrate` install.
const DESKTOP_ID: &str = "com.flectar.mail.desktop";
const DEFAULTS: &str = "Default Applications";
const ADDED: &str = "Added Associations";
const CACHE: &str = "MIME Cache";

pub(super) fn supported() -> bool {
    true
}

pub(super) fn state() -> io::Result<State> {
    let dirs = Dirs::from_env()?;
    let status = match dirs.own_desktop_id() {
        None => Status::NotInstalled,
        Some(id) if dirs.default_handler().as_deref() == Some(id.as_str()) => Status::Default,
        Some(_) => Status::NotDefault,
    };
    Ok(State {
        status,
        removable: false,
    })
}

pub(super) fn make_default(done: Done) {
    done(Dirs::from_env().and_then(|dirs| dirs.set_default()));
}

pub(super) fn remove() -> io::Result<()> {
    Ok(())
}

/// The directories the specification searches, most important first.
struct Dirs {
    /// `$XDG_CONFIG_HOME`, then `$XDG_CONFIG_DIRS`.
    config: Vec<PathBuf>,
    /// `$XDG_DATA_HOME`, then `$XDG_DATA_DIRS`.
    data: Vec<PathBuf>,
    /// `$XDG_CURRENT_DESKTOP`, lowercased: their own lists come first.
    desktops: Vec<String>,
    /// The AppImage this process runs from.
    appimage: Option<PathBuf>,
}

impl Dirs {
    fn from_env() -> io::Result<Self> {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .filter(|home| home.is_absolute());
        let user = |variable: &str, fallback: &str| {
            std::env::var_os(variable)
                .map(PathBuf::from)
                .filter(|path| path.is_absolute())
                .or_else(|| home.as_ref().map(|home| home.join(fallback)))
                .ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::NotFound,
                        format!("neither {variable} nor HOME is set"),
                    )
                })
        };
        let system = |variable: &str, fallback: &str| {
            let value = std::env::var(variable)
                .ok()
                .filter(|value| !value.is_empty())
                .unwrap_or_else(|| fallback.to_owned());
            value
                .split(':')
                .map(PathBuf::from)
                .filter(|path| path.is_absolute())
                .collect::<Vec<_>>()
        };
        let mut config = vec![user("XDG_CONFIG_HOME", ".config")?];
        config.extend(system("XDG_CONFIG_DIRS", "/etc/xdg"));
        let mut data = vec![user("XDG_DATA_HOME", ".local/share")?];
        data.extend(system("XDG_DATA_DIRS", "/usr/local/share:/usr/share"));
        let desktops = std::env::var("XDG_CURRENT_DESKTOP")
            .unwrap_or_default()
            .split(':')
            .filter(|desktop| !desktop.is_empty())
            .map(str::to_lowercase)
            .collect();
        let appimage = std::env::var_os("APPIMAGE")
            .map(PathBuf::from)
            .filter(|path| path.is_absolute());
        Ok(Self {
            config,
            data,
            desktops,
            appimage,
        })
    }

    /// The `mimeapps.list` files in lookup order: in each configuration and
    /// then data directory, the desktops' own lists before the common one.
    fn lists(&self) -> Vec<PathBuf> {
        let directories = self.config.iter().cloned().chain(
            self.data
                .iter()
                .map(|directory| directory.join("applications")),
        );
        let mut lists = Vec::new();
        for directory in directories {
            for desktop in &self.desktops {
                lists.push(directory.join(format!("{desktop}-mimeapps.list")));
            }
            lists.push(directory.join("mimeapps.list"));
        }
        lists
    }

    fn installed(&self, id: &str) -> bool {
        !id.contains('/')
            && self
                .data
                .iter()
                .any(|directory| directory.join("applications").join(id).is_file())
    }

    fn first_installed(&self, ids: &str) -> Option<String> {
        ids.split(';')
            .map(str::trim)
            .find(|id| !id.is_empty() && self.installed(id))
            .map(str::to_owned)
    }

    /// The desktop entry that starts this copy for a mail link. An AppImage
    /// that an AppImage manager added gets an entry of another name whose
    /// command is the image itself. An entry written before this release
    /// passes no link, so it does not count until the AppImage is added
    /// again.
    fn own_desktop_id(&self) -> Option<String> {
        // The first entry of that name is the one the desktop uses.
        if let Some(path) = self
            .data
            .iter()
            .map(|directory| directory.join("applications").join(DESKTOP_ID))
            .find(|path| path.is_file())
        {
            return opens_mail_links(&KeyFile::read(&path).ok()??).then(|| DESKTOP_ID.to_owned());
        }
        let image = self.appimage.as_deref()?;
        let applications = self.data.first()?.join("applications");
        fs::read_dir(applications)
            .ok()?
            .flatten()
            .filter(|entry| entry.file_name().to_string_lossy().ends_with(".desktop"))
            .find(|entry| {
                fs::read_to_string(entry.path()).is_ok_and(|text| {
                    let entry = KeyFile::parse(&text);
                    entry
                        .get("Desktop Entry", "Exec")
                        .and_then(exec_program)
                        .is_some_and(|program| Path::new(&program) == image)
                        && opens_mail_links(&entry)
                })
            })
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
    }

    /// The application that opens mail links: the first installed default
    /// in lookup order. Without one, desktops take the first associated
    /// application, from the lists and then from the installed entries.
    fn default_handler(&self) -> Option<String> {
        let lists = self
            .lists()
            .into_iter()
            .filter_map(|path| KeyFile::read(&path).ok().flatten())
            .collect::<Vec<_>>();
        let caches = self.data.iter().filter_map(|directory| {
            KeyFile::read(&directory.join("applications/mimeinfo.cache"))
                .ok()
                .flatten()
        });
        lists
            .iter()
            .find_map(|list| self.first_installed(list.get(DEFAULTS, MIME_TYPE)?))
            .or_else(|| {
                lists
                    .iter()
                    .find_map(|list| self.first_installed(list.get(ADDED, MIME_TYPE)?))
            })
            .or_else(|| {
                caches
                    .into_iter()
                    .find_map(|cache| self.first_installed(cache.get(CACHE, MIME_TYPE)?))
            })
    }

    fn set_default(&self) -> io::Result<Applied> {
        let id = self.own_desktop_id().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                "Flectar Mail is not in the applications menu",
            )
        })?;
        let user = &self.config[0];
        edit(&user.join("mimeapps.list"), |list| {
            list.set(DEFAULTS, MIME_TYPE, &format!("{id};"));
            // Also first among the applications offered for mail links.
            let mut added = format!("{id};");
            for other in list.get(ADDED, MIME_TYPE).unwrap_or_default().split(';') {
                let other = other.trim();
                if !other.is_empty() && other != id {
                    added.push_str(other);
                    added.push(';');
                }
            }
            list.set(ADDED, MIME_TYPE, &added);
        })?;
        // A desktop's own list in the same directory is read first.
        for desktop in &self.desktops {
            let path = user.join(format!("{desktop}-mimeapps.list"));
            if path.exists() {
                edit(&path, |list| list.remove(DEFAULTS, MIME_TYPE))?;
            }
        }
        Ok(Applied::Done)
    }
}

/// Whether a desktop entry declares mail links and passes them to the
/// program (`%u` or `%U`).
fn opens_mail_links(entry: &KeyFile) -> bool {
    let declared = entry.get("Desktop Entry", "MimeType").is_some_and(|types| {
        types
            .split(';')
            .any(|mime_type| mime_type.trim() == MIME_TYPE)
    });
    let passes = entry.get("Desktop Entry", "Exec").is_some_and(|exec| {
        exec.split_whitespace()
            .any(|argument| argument.eq_ignore_ascii_case("%u"))
    });
    declared && passes
}

/// The program an `Exec` line starts, unquoted.
fn exec_program(exec: &str) -> Option<String> {
    let exec = exec.trim_start();
    let Some(quoted) = exec.strip_prefix('"') else {
        return exec.split_whitespace().next().map(str::to_owned);
    };
    let mut program = String::new();
    let mut characters = quoted.chars();
    while let Some(character) = characters.next() {
        match character {
            '"' => return Some(program),
            '\\' => program.push(characters.next()?),
            character => program.push(character),
        }
    }
    None
}

/// Rewrite a list in place, keeping everything this does not change. A
/// symbolic link, as dotfile managers use, keeps pointing at its file.
fn edit(path: &Path, change: impl FnOnce(&mut KeyFile)) -> io::Result<()> {
    let path = match fs::canonicalize(path) {
        Ok(target) => target,
        Err(error) if error.kind() == io::ErrorKind::NotFound => path.to_owned(),
        Err(error) => return Err(error),
    };
    let mut list = KeyFile::read(&path)?.unwrap_or_default();
    let before = list.text();
    change(&mut list);
    let text = list.text();
    if text == before {
        return Ok(());
    }
    let directory = path
        .parent()
        .ok_or_else(|| io::Error::other("a list path has no directory"))?;
    fs::create_dir_all(directory)?;
    // Keep the file's permissions, as the desktops' own settings do; a new
    // list is readable like any other configuration file.
    let permissions = match fs::metadata(&path) {
        Ok(metadata) => metadata.permissions(),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            std::os::unix::fs::PermissionsExt::from_mode(0o644)
        }
        Err(error) => return Err(error),
    };
    let mut temporary = tempfile::NamedTempFile::new_in(directory)?;
    io::Write::write_all(&mut temporary, text.as_bytes())?;
    temporary.as_file().set_permissions(permissions)?;
    temporary.as_file().sync_all()?;
    temporary.persist(&path).map_err(|error| error.error)?;
    Ok(())
}

/// A desktop key file, edited line by line so comments, other sections and
/// unknown keys survive.
#[derive(Default)]
struct KeyFile {
    lines: Vec<String>,
}

impl KeyFile {
    fn parse(text: &str) -> Self {
        Self {
            lines: text.lines().map(str::to_owned).collect(),
        }
    }

    fn read(path: &Path) -> io::Result<Option<Self>> {
        match fs::read_to_string(path) {
            Ok(text) => Ok(Some(Self::parse(&text))),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }

    fn text(&self) -> String {
        let mut text = self.lines.join("\n");
        if !text.is_empty() {
            text.push('\n');
        }
        text
    }

    /// The lines of `section` after its header, as a range.
    fn section(&self, section: &str) -> Option<std::ops::Range<usize>> {
        let header = format!("[{section}]");
        let start = self.lines.iter().position(|line| line.trim() == header)? + 1;
        let end = self.lines[start..]
            .iter()
            .position(|line| line.trim_start().starts_with('['))
            .map_or(self.lines.len(), |offset| start + offset);
        Some(start..end)
    }

    fn find(&self, section: &str, key: &str) -> Option<usize> {
        self.section(section)?.find(|&index| {
            self.lines[index]
                .split_once('=')
                .is_some_and(|(name, _)| name.trim() == key)
        })
    }

    fn get(&self, section: &str, key: &str) -> Option<&str> {
        let line = &self.lines[self.find(section, key)?];
        line.split_once('=').map(|(_, value)| value.trim())
    }

    fn set(&mut self, section: &str, key: &str, value: &str) {
        let line = format!("{key}={value}");
        if let Some(index) = self.find(section, key) {
            self.lines[index] = line;
        } else if let Some(range) = self.section(section) {
            // After the section's last entry, before blank lines that
            // separate it from the next one.
            let end = (range.start..range.end)
                .rev()
                .find(|&index| !self.lines[index].trim().is_empty())
                .map_or(range.start, |index| index + 1);
            self.lines.insert(end, line);
        } else {
            if self
                .lines
                .last()
                .is_some_and(|last| !last.trim().is_empty())
            {
                self.lines.push(String::new());
            }
            self.lines.push(format!("[{section}]"));
            self.lines.push(line);
        }
    }

    fn remove(&mut self, section: &str, key: &str) {
        if let Some(index) = self.find(section, key) {
            self.lines.remove(index);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The entry this release installs, which opens mail links.
    const ENTRY: &str =
        "[Desktop Entry]\nExec=flectar-mail %U\nMimeType=x-scheme-handler/mailto;\n";

    struct Fixture {
        _root: tempfile::TempDir,
        dirs: Dirs,
    }

    impl Fixture {
        fn new(desktops: &[&str]) -> Self {
            let root = tempfile::tempdir().unwrap();
            let dirs = Dirs {
                config: vec![root.path().join("config"), root.path().join("etc/xdg")],
                data: vec![root.path().join("data"), root.path().join("usr/share")],
                desktops: desktops
                    .iter()
                    .map(|desktop| (*desktop).to_owned())
                    .collect(),
                appimage: None,
            };
            Self { _root: root, dirs }
        }

        fn install(&self, data: usize, id: &str, contents: &str) {
            let path = self.dirs.data[data].join("applications").join(id);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, contents).unwrap();
        }

        fn write(&self, path: &Path, contents: &str) {
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, contents).unwrap();
        }
    }

    #[test]
    fn key_files_change_one_entry_and_keep_the_rest() {
        let mut list = KeyFile::parse(
            "# mine\n[Default Applications]\ntext/html=firefox.desktop\n\n[Added Associations]\nx-scheme-handler/mailto = thunderbird.desktop;\n",
        );
        assert_eq!(list.get(ADDED, MIME_TYPE), Some("thunderbird.desktop;"));
        list.set(DEFAULTS, MIME_TYPE, "com.flectar.mail.desktop;");
        list.set(
            ADDED,
            MIME_TYPE,
            "com.flectar.mail.desktop;thunderbird.desktop;",
        );
        list.set("Removed Associations", "text/plain", "x.desktop;");
        assert_eq!(
            list.text(),
            "# mine\n[Default Applications]\ntext/html=firefox.desktop\nx-scheme-handler/mailto=com.flectar.mail.desktop;\n\n[Added Associations]\nx-scheme-handler/mailto=com.flectar.mail.desktop;thunderbird.desktop;\n\n[Removed Associations]\ntext/plain=x.desktop;\n"
        );
        list.remove(DEFAULTS, MIME_TYPE);
        assert_eq!(list.get(DEFAULTS, MIME_TYPE), None);
        assert_eq!(list.get(DEFAULTS, "text/html"), Some("firefox.desktop"));
    }

    #[test]
    fn the_default_follows_the_specification_lookup_order() {
        let fixture = Fixture::new(&["gnome"]);
        let dirs = &fixture.dirs;
        for id in ["thunderbird.desktop", "geary.desktop", DESKTOP_ID] {
            fixture.install(1, id, "[Desktop Entry]\n");
        }
        assert_eq!(dirs.default_handler(), None);

        // Without a default, the installed entries decide.
        fixture.write(
            &dirs.data[1].join("applications/mimeinfo.cache"),
            "[MIME Cache]\nx-scheme-handler/mailto=geary.desktop;thunderbird.desktop;\n",
        );
        assert_eq!(dirs.default_handler().as_deref(), Some("geary.desktop"));
        // A system default comes before them, skipping missing entries.
        fixture.write(
            &dirs.config[1].join("mimeapps.list"),
            "[Default Applications]\nx-scheme-handler/mailto=gone.desktop;thunderbird.desktop;\n",
        );
        assert_eq!(
            dirs.default_handler().as_deref(),
            Some("thunderbird.desktop")
        );
        // The user's choice wins, and the desktop's own list over it.
        fixture.write(
            &dirs.config[0].join("mimeapps.list"),
            "[Default Applications]\nx-scheme-handler/mailto=com.flectar.mail.desktop;\n",
        );
        assert_eq!(dirs.default_handler().as_deref(), Some(DESKTOP_ID));
        fixture.write(
            &dirs.config[0].join("gnome-mimeapps.list"),
            "[Default Applications]\nx-scheme-handler/mailto=geary.desktop\n",
        );
        assert_eq!(dirs.default_handler().as_deref(), Some("geary.desktop"));
    }

    #[test]
    fn choosing_flectar_mail_writes_the_users_list() {
        let fixture = Fixture::new(&["kde"]);
        let dirs = &fixture.dirs;
        fixture.install(1, "org.kde.kmail2.desktop", "[Desktop Entry]\n");
        fixture.install(1, DESKTOP_ID, ENTRY);
        let user = dirs.config[0].join("mimeapps.list");
        fixture.write(
            &user,
            "[Added Associations]\nx-scheme-handler/mailto=org.kde.kmail2.desktop;com.flectar.mail.desktop;\n",
        );
        let desktop = dirs.config[0].join("kde-mimeapps.list");
        fixture.write(
            &desktop,
            "[Default Applications]\nx-scheme-handler/mailto=org.kde.kmail2.desktop;\ntext/html=firefox.desktop;\n",
        );
        assert_eq!(
            dirs.default_handler().as_deref(),
            Some("org.kde.kmail2.desktop")
        );

        assert_eq!(dirs.set_default().unwrap(), Applied::Done);
        assert_eq!(dirs.default_handler().as_deref(), Some(DESKTOP_ID));
        assert_eq!(
            fs::read_to_string(&user).unwrap(),
            "[Added Associations]\nx-scheme-handler/mailto=com.flectar.mail.desktop;org.kde.kmail2.desktop;\n\n[Default Applications]\nx-scheme-handler/mailto=com.flectar.mail.desktop;\n"
        );
        assert_eq!(
            fs::read_to_string(&desktop).unwrap(),
            "[Default Applications]\ntext/html=firefox.desktop;\n"
        );
    }

    #[test]
    fn a_linked_list_keeps_its_link() {
        let fixture = Fixture::new(&[]);
        let dirs = &fixture.dirs;
        fixture.install(0, DESKTOP_ID, ENTRY);
        let dotfiles = dirs.data[0].join("dotfiles/mimeapps.list");
        fixture.write(&dotfiles, "[Default Applications]\n");
        fs::create_dir_all(&dirs.config[0]).unwrap();
        std::os::unix::fs::symlink(&dotfiles, dirs.config[0].join("mimeapps.list")).unwrap();
        dirs.set_default().unwrap();
        assert!(
            fs::symlink_metadata(dirs.config[0].join("mimeapps.list"))
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert!(
            fs::read_to_string(dotfiles)
                .unwrap()
                .contains("x-scheme-handler/mailto=com.flectar.mail.desktop;")
        );
    }

    #[test]
    fn an_appimage_is_found_under_its_managers_entry() {
        let mut fixture = Fixture::new(&[]);
        assert_eq!(fixture.dirs.own_desktop_id(), None);
        assert!(fixture.dirs.set_default().is_err());

        fixture.dirs.appimage = Some(PathBuf::from("/home/me/Apps/Flectar \"Mail\".AppImage"));
        fixture.install(
            0,
            "appimagekit_0123-Flectar_Mail.desktop",
            "[Desktop Entry]\nExec=\"/home/me/Apps/Flectar \\\"Mail\\\".AppImage\" %U\nMimeType=x-scheme-handler/mailto;\n",
        );
        fixture.install(
            0,
            "other.desktop",
            "[Desktop Entry]\nExec=/usr/bin/other %U\n",
        );
        assert_eq!(
            fixture.dirs.own_desktop_id().as_deref(),
            Some("appimagekit_0123-Flectar_Mail.desktop")
        );
        // The entry the packages install is preferred when there is one.
        fixture.install(1, DESKTOP_ID, ENTRY);
        assert_eq!(fixture.dirs.own_desktop_id().as_deref(), Some(DESKTOP_ID));
    }

    #[test]
    fn an_entry_that_passes_no_link_does_not_count() {
        let fixture = Fixture::new(&[]);
        // Written by an AppImage integration before this release.
        fixture.install(
            0,
            DESKTOP_ID,
            "[Desktop Entry]\nExec=\"/home/me/Applications/flectar-mail.AppImage\"\n",
        );
        fixture.install(1, DESKTOP_ID, ENTRY);
        assert_eq!(
            fixture.dirs.own_desktop_id(),
            None,
            "the user's entry shadows the system one"
        );
        assert!(fixture.dirs.set_default().is_err());
        fixture.install(
            0,
            DESKTOP_ID,
            "[Desktop Entry]\nExec=flectar-mail %U\nMimeType=text/html;\n",
        );
        assert_eq!(fixture.dirs.own_desktop_id(), None);
        fixture.install(0, DESKTOP_ID, ENTRY);
        assert_eq!(fixture.dirs.own_desktop_id().as_deref(), Some(DESKTOP_ID));
    }

    #[test]
    fn rewritten_lists_keep_their_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let fixture = Fixture::new(&[]);
        let dirs = &fixture.dirs;
        fixture.install(0, DESKTOP_ID, ENTRY);
        let user = dirs.config[0].join("mimeapps.list");
        dirs.set_default().unwrap();
        assert_eq!(
            fs::metadata(&user).unwrap().permissions().mode() & 0o777,
            0o644
        );
        fixture.write(&user, "[Default Applications]\n");
        fs::set_permissions(&user, fs::Permissions::from_mode(0o640)).unwrap();
        dirs.set_default().unwrap();
        assert_eq!(
            fs::metadata(&user).unwrap().permissions().mode() & 0o777,
            0o640
        );
    }

    #[test]
    fn exec_programs_are_unquoted() {
        assert_eq!(
            exec_program("flectar-mail %U").as_deref(),
            Some("flectar-mail")
        );
        assert_eq!(
            exec_program("\"/a b/c\\\\d\" %U").as_deref(),
            Some("/a b/c\\d")
        );
        assert_eq!(exec_program("\"unterminated"), None);
        assert_eq!(exec_program(""), None);
    }
}
