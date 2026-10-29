//! Reader for Thunderbird desktop profiles.
//!
//! Accepts a profile folder, the `.zip` written by Thunderbird's
//! Tools › Export, or a lone `prefs.js`. Account data comes from the
//! `mail.account.*`, `mail.server.*`, `mail.identity.*` and `mail.smtpserver.*`
//! preference branches; defaults for absent values follow `mailnews.js`.
//! Files are only read from inside the selected profile. Of the mail folders,
//! only those of Local Folders and of POP3 accounts are listed, since IMAP
//! mail is still on its server.

use super::model::{
    AccountCandidate, IdentityCandidate, ImportInventory, MailStore, OtherEntry, OtherEntryKind,
    ServerCandidate, ServerProtocol, SignatureCandidate, SourceAuth, SourceKind, SourceRetention,
    SourceSecurity,
};
use super::prefs::Prefs;
use super::{MAX_SOURCE_BYTES, SourceError};
use crate::mail_import::MailSource;
use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::path::{Component, Path, PathBuf};

const MAX_ACCOUNTS: usize = 100;
/// Text one inventory may hold. Accounts can share a server or an identity
/// in `prefs.js`, so a small file could otherwise expand to gigabytes; a real
/// profile's account data is a few kilobytes.
const MAX_INVENTORY_BYTES: usize = MAX_SOURCE_BYTES as usize;
/// `profiles.ini` lists a handful of profiles; anything larger is not one.
const MAX_PROFILES_INI_BYTES: u64 = 64 * 1024;
/// Matches the signature editor's limit.
const MAX_SIGNATURE_FILE_BYTES: u64 = crate::signatures::MAX_SIGNATURE_BYTES as u64;

fn malformed(reason: impl Into<String>) -> SourceError {
    SourceError::Malformed {
        format: "Thunderbird",
        reason: reason.into(),
    }
}

/// Read access to files inside one profile.
trait ProfileFiles {
    /// Reads `relative` (with `/` separators) if it exists inside the profile
    /// and is no larger than `limit`.
    fn read(&mut self, relative: &str, limit: u64) -> Option<Vec<u8>>;

    /// The size of `relative` if it exists inside the profile.
    fn size(&mut self, relative: &str) -> Option<u64>;

    /// Maps an absolute path recorded in the preferences to a path inside the
    /// profile, or `None` when it points elsewhere.
    fn relative_of(&self, _absolute: &str) -> Option<String> {
        None
    }

    /// The saved mail in the folder `relative`.
    fn mail(&mut self, _relative: &str) -> MailLookup {
        MailLookup::Unavailable
    }
}

/// What a mail folder of the profile holds.
enum MailLookup {
    Found(MailSource),
    /// Nothing to import.
    Empty,
    /// The folder isn't in what was chosen.
    Unavailable,
}

/// A lone `prefs.js`: no other profile files are available.
struct NoFiles;

impl ProfileFiles for NoFiles {
    fn read(&mut self, _relative: &str, _limit: u64) -> Option<Vec<u8>> {
        None
    }

    fn size(&mut self, _relative: &str) -> Option<u64> {
        None
    }
}

struct DirectoryFiles {
    /// The folder as chosen, for comparing paths recorded in preferences.
    chosen: PathBuf,
    /// Its canonical form, which every file read must stay below.
    root: PathBuf,
}

impl DirectoryFiles {
    /// The file `relative` names, if it is inside the profile. Links that
    /// lead outside the selected profile are not followed.
    fn file(&self, relative: &str) -> Option<PathBuf> {
        let canonical = self
            .root
            .join(safe_relative(relative)?)
            .canonicalize()
            .ok()?;
        (canonical.starts_with(&self.root) && canonical.is_file()).then_some(canonical)
    }
}

impl ProfileFiles for DirectoryFiles {
    fn read(&mut self, relative: &str, limit: u64) -> Option<Vec<u8>> {
        let file = std::fs::File::open(self.file(relative)?).ok()?;
        read_limited(file, limit)
    }

    fn size(&mut self, relative: &str) -> Option<u64> {
        Some(self.file(relative)?.metadata().ok()?.len())
    }

    fn mail(&mut self, relative: &str) -> MailLookup {
        let Some(folder) = safe_relative(relative)
            .and_then(|relative| self.root.join(relative).canonicalize().ok())
        else {
            return MailLookup::Empty;
        };
        if !folder.starts_with(&self.root) {
            return MailLookup::Unavailable;
        }
        match crate::mail_import::scan_folder(&folder) {
            Ok(mail) => MailLookup::Found(mail),
            Err(_) => MailLookup::Empty,
        }
    }

    fn relative_of(&self, absolute: &str) -> Option<String> {
        // Compare text first: resolving an arbitrary recorded path could reach
        // a network share (and send Windows credentials) or a dead mount.
        let path = Path::new(absolute);
        let relative = [&self.chosen, &self.root]
            .into_iter()
            .find_map(|base| path.strip_prefix(base).ok())?;
        let relative = safe_relative(relative.to_str()?)?;
        Some(
            relative
                .components()
                .map(|component| component.as_os_str().to_string_lossy())
                .collect::<Vec<_>>()
                .join("/"),
        )
    }
}

struct ZipFiles<R: Read + std::io::Seek> {
    /// The archive's file, which mail is read from separately.
    path: PathBuf,
    archive: zip::ZipArchive<R>,
    /// Entry indexes by decoded name with `/` separators, so archives with
    /// code page names or the `\` some Windows tools write are read too.
    entries: HashMap<String, usize>,
    /// Directory inside the archive that holds `prefs.js`, with a trailing
    /// `/`, or empty when it is at the root.
    prefix: String,
}

impl<R: Read + std::io::Seek> ZipFiles<R> {
    fn entry(&mut self, relative: &str) -> Option<zip::read::ZipFile<'_, R>> {
        let relative = safe_relative(relative)?
            .components()
            .map(|component| component.as_os_str().to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join("/");
        let index = *self.entries.get(&format!("{}{relative}", self.prefix))?;
        self.archive
            .by_index(index)
            .ok()
            .filter(|entry| entry.is_file())
    }
}

impl<R: Read + std::io::Seek> ProfileFiles for ZipFiles<R> {
    fn read(&mut self, relative: &str, limit: u64) -> Option<Vec<u8>> {
        let entry = self.entry(relative)?;
        if entry.size() > limit {
            return None;
        }
        read_limited(entry, limit)
    }

    fn size(&mut self, relative: &str) -> Option<u64> {
        Some(self.entry(relative)?.size())
    }

    fn mail(&mut self, relative: &str) -> MailLookup {
        let Some(relative) = safe_relative(relative) else {
            return MailLookup::Unavailable;
        };
        let folder = relative
            .components()
            .map(|component| component.as_os_str().to_string_lossy())
            .collect::<Vec<_>>()
            .join("/");
        match crate::mail_import::scan::scan_archive(
            &self.path,
            &format!("{}{folder}/", self.prefix),
        ) {
            Ok(mail) => MailLookup::Found(mail),
            Err(_) => MailLookup::Empty,
        }
    }
}

/// Accepts only plain relative paths: no roots, drive prefixes, or `..`.
fn safe_relative(relative: &str) -> Option<PathBuf> {
    let path = Path::new(relative);
    let mut safe = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(part) => safe.push(part),
            Component::CurDir => {}
            _ => return None,
        }
    }
    (!safe.as_os_str().is_empty()).then_some(safe)
}

fn read_limited(reader: impl Read, limit: u64) -> Option<Vec<u8>> {
    let mut bytes = Vec::new();
    reader.take(limit + 1).read_to_end(&mut bytes).ok()?;
    (bytes.len() as u64 <= limit).then_some(bytes)
}

/// Reads a lone `prefs.js`.
pub fn parse_prefs(bytes: &[u8]) -> Result<ImportInventory, SourceError> {
    let prefs = Prefs::parse(&String::from_utf8_lossy(bytes))
        .ok_or_else(|| malformed("no preferences were found"))?;
    inventory(&prefs, &mut NoFiles)
}

/// Reads a profile folder. Choosing Thunderbird's data folder (the one with
/// `profiles.ini`) reads its default profile, or its only one.
pub fn read_profile_dir(directory: &Path) -> Result<ImportInventory, SourceError> {
    let mut directory = directory.to_owned();
    let ini = directory.join("profiles.ini");
    if !directory.join("prefs.js").is_file() && ini.is_file() {
        let bytes = std::fs::File::open(&ini)
            .ok()
            .and_then(|file| read_limited(file, MAX_PROFILES_INI_BYTES))
            .ok_or_else(|| malformed("profiles.ini is unreadable or too large"))?;
        let profiles = parse_profiles_ini(&directory, &decode_text(&bytes));
        let chosen = profiles
            .iter()
            .find(|profile| profile.is_default)
            .or_else(|| (profiles.len() == 1).then(|| &profiles[0]));
        match chosen {
            // This folder may come from anywhere. Opening a network path it
            // names would make Windows send the user's credentials there.
            Some(profile) if !is_local(&profile.path) => {
                return Err(SourceError::NetworkProfile);
            }
            Some(profile) => directory = profile.path.clone(),
            None if profiles.len() > 1 => return Err(SourceError::MultipleProfiles),
            None => {}
        }
    }
    let root = directory
        .canonicalize()
        .map_err(|error| SourceError::Unreadable(error.to_string()))?;
    read_profile(&mut DirectoryFiles {
        chosen: directory,
        root,
    })
}

/// Reads a profile exported by Thunderbird as a `.zip` archive.
pub fn read_export(path: &Path) -> Result<ImportInventory, SourceError> {
    let file =
        std::fs::File::open(path).map_err(|error| SourceError::Unreadable(error.to_string()))?;
    let archive = zip::ZipArchive::new(std::io::BufReader::new(file))
        .map_err(|error| malformed(error.to_string()))?;
    let mut entries = HashMap::new();
    for (index, name) in archive.file_names().enumerate() {
        if let Ok(name) = name {
            entries.entry(name.replace('\\', "/")).or_insert(index);
        }
    }
    // Thunderbird's export puts the profile in one top-level folder; an
    // archive of the profile's own contents has it at the root.
    let mut prefixes = entries
        .keys()
        .filter_map(|name| name.strip_suffix("prefs.js"))
        .filter(|prefix| {
            prefix.is_empty() || (prefix.ends_with('/') && prefix.matches('/').count() == 1)
        })
        .collect::<Vec<_>>();
    prefixes.sort_unstable();
    let prefix = match prefixes.as_slice() {
        [] => return Err(SourceError::UnknownFormat),
        [prefix] => (*prefix).to_owned(),
        // A profile at the root wins over folders inside it.
        ["", ..] => String::new(),
        _ => return Err(SourceError::MultipleProfiles),
    };
    read_profile(&mut ZipFiles {
        path: path.to_owned(),
        archive,
        entries,
        prefix,
    })
}

fn read_profile(files: &mut dyn ProfileFiles) -> Result<ImportInventory, SourceError> {
    let bytes =
        files
            .read("prefs.js", MAX_SOURCE_BYTES)
            .ok_or_else(|| match files.size("prefs.js") {
                Some(size) if size > MAX_SOURCE_BYTES => SourceError::TooLarge,
                _ => SourceError::NotAProfile,
            })?;
    let mut prefs = Prefs::parse(&String::from_utf8_lossy(&bytes))
        .ok_or_else(|| malformed("no preferences were found"))?;
    // Thunderbird applies user.js over prefs.js at every start.
    if let Some(user) = files.read("user.js", MAX_SOURCE_BYTES) {
        prefs.merge(&String::from_utf8_lossy(&user));
    }
    inventory(&prefs, files)
}

fn inventory(prefs: &Prefs, files: &mut dyn ProfileFiles) -> Result<ImportInventory, SourceError> {
    let account_keys = prefs.list("mail.accountmanager.accounts");
    if account_keys.is_empty() {
        return Err(SourceError::NoAccounts);
    }
    let colors = files
        .read("folderTree.json", MAX_SOURCE_BYTES)
        .map(|bytes| folder_colors(&bytes))
        .unwrap_or_default();
    // As in Thunderbird's OutgoingServerService: the outgoing servers are the
    // keys listed in `mail.smtpservers`, and an identity without one of them
    // uses `mail.smtp.defaultserver`, or the first server when that is unset.
    let smtp_servers = prefs.list("mail.smtpservers");
    let default_smtp = match prefs
        .string("mail.smtp.defaultserver")
        .map(str::trim)
        .filter(|key| !key.is_empty())
    {
        Some(key) => smtp_servers.iter().find(|listed| *listed == key),
        None => smtp_servers.first(),
    }
    .cloned()
    .unwrap_or_default();

    let mut accounts = Vec::new();
    let mut mail = Vec::new();
    let mut other_entries = Vec::new();
    let mut seen = HashSet::new();
    let mut size = 0;
    for key in account_keys {
        if !seen.insert(key.clone()) {
            continue;
        }
        let Some(server) = prefs.string(&format!("mail.account.{key}.server")) else {
            continue;
        };
        let server_pref = |name: &str| format!("mail.server.{server}.{name}");
        // Thunderbird ignores accounts whose server was deleted.
        let Some(kind) = prefs.string(&server_pref("type")) else {
            continue;
        };
        let label = prefs
            .string(&server_pref("name"))
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .map(str::to_owned);
        let other = match kind {
            "none" => Some(OtherEntryKind::LocalFolders),
            "rss" => Some(OtherEntryKind::Feeds),
            "nntp" => Some(OtherEntryKind::Newsgroups),
            // Chat accounts are not mail accounts and Thunderbird hides them
            // from its own account lists.
            "im" => continue,
            _ => None,
        };
        if let Some(kind) = other {
            let label = label.unwrap_or_default();
            if kind == OtherEntryKind::LocalFolders {
                match mail_of(prefs, server, files) {
                    MailLookup::Found(found) => {
                        mail.push(MailStore {
                            account: None,
                            label,
                            mail: found,
                        });
                        continue;
                    }
                    MailLookup::Empty => continue,
                    MailLookup::Unavailable => {}
                }
            }
            size = spend(size, label.len())?;
            other_entries.push(OtherEntry { kind, label });
            continue;
        }
        if accounts.len() == MAX_ACCOUNTS {
            return Err(malformed(format!(
                "the profile contains more than {MAX_ACCOUNTS} accounts"
            )));
        }

        // An identity listed twice is one identity.
        let mut listed = HashSet::new();
        let identities = prefs
            .list(&format!("mail.account.{key}.identities"))
            .into_iter()
            .filter(|id| listed.insert(id.clone()))
            .filter(|id| prefs.bool(&format!("mail.identity.{id}.valid")) != Some(false))
            .take(crate::sender_identities::MAX_IDENTITIES)
            .collect::<Vec<_>>();
        let smtp_key = |id: &str| {
            prefs
                .string(&format!("mail.identity.{id}.smtpServer"))
                .filter(|key| smtp_servers.iter().any(|listed| listed == key))
                .unwrap_or(&default_smtp)
                .to_owned()
        };
        let account_smtp = identities.first().map(|id| smtp_key(id));
        let incoming = incoming_server(prefs, server, kind);
        let retention =
            (incoming.protocol == ServerProtocol::Pop3).then(|| pop3_retention(prefs, server));
        // What a POP3 account downloaded exists only in the profile.
        if incoming.protocol == ServerProtocol::Pop3
            && let MailLookup::Found(found) = mail_of(prefs, server, files)
        {
            mail.push(MailStore {
                account: Some(key.clone()),
                label: label.clone().unwrap_or_else(|| incoming.username.clone()),
                mail: found,
            });
        }
        let account = AccountCandidate {
            source_id: key.clone(),
            color: root_folder_color(&colors, prefs, server, incoming.protocol),
            label,
            identities: identities
                .iter()
                .map(|id| IdentityCandidate {
                    separate_outgoing_server: account_smtp.as_deref()
                        != Some(smtp_key(id).as_str()),
                    ..identity(prefs, id, files)
                })
                .collect(),
            incoming: Some(incoming),
            outgoing: account_smtp
                .filter(|key| !key.is_empty())
                .and_then(|key| outgoing_server(prefs, &key)),
            retention,
        };
        size = spend(size, text_size(&account))?;
        accounts.push(account);
    }
    if accounts.is_empty() && mail.is_empty() && other_entries.is_empty() {
        return Err(SourceError::NoAccounts);
    }
    Ok(ImportInventory {
        source: SourceKind::ThunderbirdProfile,
        source_version: None,
        accounts,
        mail,
        other_entries,
    })
}

/// The mail a server keeps in the profile, found through the folder its
/// `directory-rel`, or else `directory`, names.
fn mail_of(prefs: &Prefs, server: &str, files: &mut dyn ProfileFiles) -> MailLookup {
    let relative = prefs
        .string(&format!("mail.server.{server}.directory-rel"))
        .and_then(|path| path.strip_prefix("[ProfD]"))
        .map(|path| path.replace('\\', "/"))
        .or_else(|| {
            prefs
                .string(&format!("mail.server.{server}.directory"))
                .and_then(|path| files.relative_of(path))
        });
    match relative {
        Some(relative) => files.mail(&relative),
        None => MailLookup::Unavailable,
    }
}

/// Adds `bytes` of text to what the inventory holds.
fn spend(size: usize, bytes: usize) -> Result<usize, SourceError> {
    let size = size.saturating_add(bytes);
    if size > MAX_INVENTORY_BYTES {
        return Err(SourceError::TooLarge);
    }
    Ok(size)
}

fn text_size(account: &AccountCandidate) -> usize {
    let optional = |value: &Option<String>| value.as_ref().map_or(0, String::len);
    let server = |server: &Option<ServerCandidate>| {
        server
            .as_ref()
            .map_or(0, |server| server.host.len() + server.username.len())
    };
    let identities = account
        .identities
        .iter()
        .map(|identity| {
            let signature = match &identity.signature {
                Some(SignatureCandidate::Text(text) | SignatureCandidate::Html(text)) => text.len(),
                _ => 0,
            };
            identity.email.len()
                + optional(&identity.display_name)
                + optional(&identity.reply_to)
                + signature
        })
        .sum::<usize>();
    account.source_id.len()
        + optional(&account.label)
        + server(&account.incoming)
        + server(&account.outgoing)
        + identities
}

fn incoming_server(prefs: &Prefs, server: &str, kind: &str) -> ServerCandidate {
    let pref = |name: &str| format!("mail.server.{server}.{name}");
    // Profiles from before Thunderbird 60 keep an edited host or user name
    // in `realhostname`/`realuserName`; the plain value is the original.
    let host = prefs
        .string(&pref("realhostname"))
        .or_else(|| prefs.string(&pref("hostname")))
        .unwrap_or_default();
    let username = prefs
        .string(&pref("realuserName"))
        .or_else(|| prefs.string(&pref("userName")))
        .unwrap_or_default();
    let security = match prefs.int(&pref("socketType")) {
        Some(socket_type) => socket_security(socket_type),
        None if prefs.bool(&pref("isSecure")) == Some(true) => SourceSecurity::Tls,
        None => SourceSecurity::None,
    };
    ServerCandidate {
        protocol: match kind {
            "imap" => ServerProtocol::Imap,
            "pop3" => ServerProtocol::Pop3,
            "ews" => ServerProtocol::Ews,
            _ => ServerProtocol::Other,
        },
        host: host.trim().to_owned(),
        port: port(prefs.int(&pref("port"))),
        security,
        auth: auth_method(prefs.int(&pref("authMethod")).unwrap_or(3)),
        username: username.to_owned(),
    }
}

/// What Thunderbird does with mail a POP3 server delivers. A server falls
/// back to `mail.server.default` as Thunderbird resolves its settings.
fn pop3_retention(prefs: &Prefs, server: &str) -> SourceRetention {
    let setting = |name: &str| {
        [
            format!("mail.server.{server}.{name}"),
            format!("mail.server.default.{name}"),
        ]
    };
    let flag = |name: &str| {
        setting(name)
            .iter()
            .find_map(|pref| prefs.bool(pref))
            .unwrap_or(false)
    };
    let days = setting("num_days_to_leave_on_server")
        .iter()
        .find_map(|pref| prefs.int(pref))
        .unwrap_or(7);
    SourceRetention {
        keep: flag("leave_on_server"),
        remove_after_days: flag("delete_by_age_from_server")
            .then(|| u32::try_from(days.max(0)).unwrap_or(u32::MAX)),
        remove_deleted: flag("delete_mail_left_on_server"),
    }
}

fn outgoing_server(prefs: &Prefs, key: &str) -> Option<ServerCandidate> {
    let pref = |name: &str| format!("mail.smtpserver.{key}.{name}");
    // Thunderbird 127 added `type`; older servers are SMTP.
    let protocol = match prefs.string(&pref("type")).unwrap_or("smtp") {
        "smtp" => ServerProtocol::Smtp,
        "ews" => ServerProtocol::Ews,
        _ => ServerProtocol::Other,
    };
    if protocol != ServerProtocol::Smtp {
        // Exchange servers keep their settings in their own branch.
        let pref = |name: &str| format!("mail.outgoingserver.{key}.{name}");
        let url = prefs
            .string(&pref("ews_url"))
            .and_then(|url| url::Url::parse(url.trim()).ok());
        return Some(ServerCandidate {
            protocol,
            host: url
                .as_ref()
                .and_then(url::Url::host_str)
                .unwrap_or_default()
                .to_owned(),
            port: url.as_ref().and_then(url::Url::port),
            security: match url.as_ref().map(url::Url::scheme) {
                Some("https") => SourceSecurity::Tls,
                Some("http") => SourceSecurity::None,
                _ => SourceSecurity::Unknown,
            },
            auth: auth_method(prefs.int(&pref("auth_method")).unwrap_or(3)),
            username: prefs
                .string(&pref("username"))
                .unwrap_or_default()
                .to_owned(),
        });
    }
    let host = prefs.string(&pref("hostname"))?;
    Some(ServerCandidate {
        protocol,
        host: host.trim().to_owned(),
        port: port(prefs.int(&pref("port"))),
        security: socket_security(prefs.int(&pref("try_ssl")).unwrap_or(0)),
        auth: auth_method(prefs.int(&pref("authMethod")).unwrap_or(3)),
        username: prefs
            .string(&pref("username"))
            .unwrap_or_default()
            .to_owned(),
    })
}

/// Thunderbird stores −1 (or omits the value) for the protocol default.
fn port(value: Option<i64>) -> Option<u16> {
    value
        .and_then(|port| u16::try_from(port).ok())
        .filter(|port| *port != 0)
}

/// `nsMsgSocketType`. Value 1 ("try STARTTLS") was removed and Thunderbird
/// migrates it to required STARTTLS.
fn socket_security(value: i64) -> SourceSecurity {
    match value {
        0 => SourceSecurity::None,
        1 | 2 => SourceSecurity::Starttls,
        3 => SourceSecurity::Tls,
        _ => SourceSecurity::Unknown,
    }
}

/// `nsMsgAuthMethod`.
fn auth_method(value: i64) -> SourceAuth {
    match value {
        1 => SourceAuth::None,
        2 | 3 => SourceAuth::Password,
        // 8 ("secure") is a deprecated alias Thunderbird migrates to 4.
        4 | 8 => SourceAuth::EncryptedPassword,
        5 => SourceAuth::Kerberos,
        6 => SourceAuth::Ntlm,
        7 => SourceAuth::ClientCertificate,
        9 => SourceAuth::Automatic,
        10 => SourceAuth::OAuth2,
        _ => SourceAuth::Unknown,
    }
}

fn identity(prefs: &Prefs, id: &str, files: &mut dyn ProfileFiles) -> IdentityCandidate {
    let pref = |name: &str| format!("mail.identity.{id}.{name}");
    let text = |name: &str| {
        prefs
            .string(&pref(name))
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
    };
    let signature = if prefs.bool(&pref("attach_signature")) == Some(true) {
        Some(signature_file(prefs, id, files))
    } else {
        text("htmlSigText").map(|signature| {
            if prefs.bool(&pref("htmlSigFormat")) == Some(true) {
                SignatureCandidate::Html(signature)
            } else {
                SignatureCandidate::Text(signature)
            }
        })
    };
    IdentityCandidate {
        email: text("useremail").unwrap_or_default(),
        display_name: text("fullName"),
        reply_to: text("reply_to"),
        signature,
        separate_outgoing_server: false,
    }
}

/// Signatures attached from a file are imported only when the file lives in
/// the selected profile; other locations were not part of the user's choice.
fn signature_file(prefs: &Prefs, id: &str, files: &mut dyn ProfileFiles) -> SignatureCandidate {
    let relative = prefs
        .string(&format!("mail.identity.{id}.sig_file-rel"))
        .and_then(|path| path.strip_prefix("[ProfD]"))
        .map(|path| path.replace('\\', "/"))
        .or_else(|| {
            prefs
                .string(&format!("mail.identity.{id}.sig_file"))
                .and_then(|path| files.relative_of(path))
        });
    let Some(relative) = relative else {
        return SignatureCandidate::ExternalFile;
    };
    let file_name = relative.rsplit('/').next().unwrap_or_default();
    let extension = file_name
        .rsplit_once('.')
        .map(|(_, extension)| extension.to_ascii_lowercase())
        .unwrap_or_default();
    let html = matches!(extension.as_str(), "htm" | "html");
    if !html && !matches!(extension.as_str(), "txt" | "sig" | "") {
        // Image signatures and other formats have no text equivalent.
        return SignatureCandidate::ExternalFile;
    }
    let Some(bytes) = files.read(&relative, MAX_SIGNATURE_FILE_BYTES) else {
        return SignatureCandidate::ExternalFile;
    };
    if html {
        SignatureCandidate::Html(decode_html(&bytes))
    } else {
        SignatureCandidate::Text(decode_text(&bytes))
    }
}

/// Text files are whatever the user's editor saved: UTF-8 or UTF-16 with a
/// byte order mark, UTF-8, or else the Windows code page.
fn decode_text(bytes: &[u8]) -> String {
    decode(bytes, None)
}

/// HTML signatures may declare a legacy encoding, which Thunderbird honors.
fn decode_html(bytes: &[u8]) -> String {
    decode(bytes, declared_charset(bytes))
}

fn decode(bytes: &[u8], declared: Option<&'static encoding_rs::Encoding>) -> String {
    if let Some((encoding, bom)) = encoding_rs::Encoding::for_bom(bytes) {
        return encoding
            .decode_without_bom_handling(&bytes[bom..])
            .0
            .into_owned();
    }
    if let Some(encoding) = declared {
        return encoding.decode_without_bom_handling(bytes).0.into_owned();
    }
    match std::str::from_utf8(bytes) {
        Ok(text) => text.to_owned(),
        Err(_) => encoding_rs::WINDOWS_1252.decode(bytes).0.into_owned(),
    }
}

/// The `charset` an HTML document declares in its first kilobyte. A UTF-16
/// declaration in a document without a byte order mark cannot be right.
fn declared_charset(bytes: &[u8]) -> Option<&'static encoding_rs::Encoding> {
    let head = String::from_utf8_lossy(&bytes[..bytes.len().min(1024)]).to_ascii_lowercase();
    let start = head.find("charset=")? + "charset=".len();
    let label = head[start..]
        .trim_start_matches(['"', '\''])
        .split(|character: char| !(character.is_ascii_alphanumeric() || "-_:.".contains(character)))
        .next()?;
    encoding_rs::Encoding::for_label(label.as_bytes())
        .filter(|encoding| *encoding != encoding_rs::UTF_16LE && *encoding != encoding_rs::UTF_16BE)
}

/// Root folder colors by URI scheme, lowercase host and user name.
type FolderColors = HashMap<(String, String, String), String>;

/// Account colors from `folderTree.json`. Only its `colors` map is kept in
/// memory; the folder state around it is skipped as it is parsed.
fn folder_colors(bytes: &[u8]) -> FolderColors {
    #[derive(serde::Deserialize)]
    struct FolderTree {
        colors: serde_json::Map<String, serde_json::Value>,
    }
    let Ok(tree) = serde_json::from_slice::<FolderTree>(bytes) else {
        return FolderColors::new();
    };
    let mut colors = FolderColors::new();
    // The map is ordered, so the same file always yields the same color.
    for (uri, color) in tree.colors {
        let (Some(color), Ok(url)) = (color.as_str().and_then(hex_color), url::Url::parse(&uri))
        else {
            continue;
        };
        let Some(host) = url.host_str().filter(|_| matches!(url.path(), "" | "/")) else {
            continue;
        };
        let user = percent_encoding::percent_decode_str(url.username())
            .decode_utf8_lossy()
            .into_owned();
        colors
            .entry((url.scheme().to_owned(), host.to_ascii_lowercase(), user))
            .or_insert(color);
    }
    colors
}

/// `#RRGGBB`, the only form Flectar stores; anything else is ignored.
fn hex_color(value: &str) -> Option<String> {
    let hex = value.trim().strip_prefix('#')?;
    (hex.len() == 6 && hex.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .then(|| format!("#{}", hex.to_ascii_uppercase()))
}

/// Matches `imap://user@host` (or `mailbox://` for POP3) without rebuilding
/// Thunderbird's exact escaping of the user name. Root folder URIs keep the
/// original host and user name even when old profiles edited them.
fn root_folder_color(
    colors: &FolderColors,
    prefs: &Prefs,
    server: &str,
    protocol: ServerProtocol,
) -> Option<String> {
    let scheme = match protocol {
        ServerProtocol::Imap => "imap",
        ServerProtocol::Pop3 => "mailbox",
        _ => return None,
    };
    let host = prefs.string(&format!("mail.server.{server}.hostname"))?;
    let user = prefs
        .string(&format!("mail.server.{server}.userName"))
        .unwrap_or_default();
    colors
        .get(&(
            scheme.to_owned(),
            host.trim().to_ascii_lowercase(),
            user.to_owned(),
        ))
        .cloned()
}

/// A Thunderbird profile found in a standard location.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DetectedProfile {
    pub name: String,
    pub path: PathBuf,
    /// The profile Thunderbird opens by default.
    pub is_default: bool,
}

/// Finds Thunderbird profiles in the standard locations of this platform.
/// Unreadable locations, such as those hidden by a sandbox, are skipped.
#[cfg(desktop)]
pub fn discover_profiles() -> Vec<DetectedProfile> {
    let mut roots = Vec::new();
    if cfg!(target_os = "windows") {
        roots.extend(dirs::config_dir().map(|dir| dir.join("Thunderbird")));
    } else if cfg!(target_os = "macos") {
        roots.extend(dirs::home_dir().map(|home| home.join("Library/Thunderbird")));
    } else if let Some(home) = dirs::home_dir() {
        roots.push(home.join(".thunderbird"));
        // Thunderbird builds on Gecko 147+ use XDG when ~/.thunderbird does
        // not exist.
        roots.extend(dirs::config_dir().map(|dir| dir.join("thunderbird")));
        roots.push(home.join(".var/app/org.mozilla.Thunderbird/.thunderbird"));
        roots.push(home.join(".var/app/org.mozilla.thunderbird/.thunderbird"));
        roots.push(home.join("snap/thunderbird/common/.thunderbird"));
    }
    let mut seen = HashSet::new();
    let mut profiles = Vec::new();
    for root in roots {
        let Some(ini) = std::fs::File::open(root.join("profiles.ini"))
            .ok()
            .filter(|file| file.metadata().is_ok_and(|metadata| metadata.is_file()))
            .and_then(|file| read_limited(file, MAX_PROFILES_INI_BYTES))
        else {
            continue;
        };
        for profile in parse_profiles_ini(&root, &decode_text(&ini)) {
            if !profile.path.join("prefs.js").is_file() {
                continue;
            }
            let key = profile
                .path
                .canonicalize()
                .unwrap_or_else(|_| profile.path.clone());
            if seen.insert(key) {
                profiles.push(profile);
            }
        }
    }
    profiles
}

/// Parses `profiles.ini`. An `[Install…]` section's `Default` names the
/// profile a dedicated installation opens and wins over `Default=1`.
fn parse_profiles_ini(root: &Path, text: &str) -> Vec<DetectedProfile> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut sections: HashMap<String, HashMap<String, String>> = HashMap::new();
    let mut install_defaults = Vec::new();
    let mut current = None::<String>;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with(';') || line.starts_with('#') {
            continue;
        }
        if let Some(name) = line
            .strip_prefix('[')
            .and_then(|line| line.strip_suffix(']'))
        {
            current = Some(name.to_owned());
        } else if let (Some((key, value)), Some(section)) = (line.split_once('='), &current) {
            let (key, value) = (key.trim(), value.trim());
            if section.starts_with("Install") && key == "Default" {
                install_defaults.push(value.to_owned());
            }
            // Only profile sections are kept, and only the keys read below.
            if section.starts_with("Profile")
                && matches!(key, "Name" | "Path" | "IsRelative" | "Default")
            {
                sections
                    .entry(section.clone())
                    .or_default()
                    .insert(key.to_owned(), value.to_owned());
            }
        }
    }
    let resolve = |path: &str, relative: bool| {
        if relative {
            safe_relative(path).map(|path| root.join(path))
        } else {
            // A path written by another system (a Windows drive path read on
            // Linux) would otherwise resolve against the working directory.
            Some(PathBuf::from(path)).filter(|path| path.is_absolute())
        }
    };
    let install_defaults = install_defaults
        .iter()
        .filter_map(|path| resolve(path, !Path::new(path).is_absolute()))
        .collect::<Vec<_>>();
    let mut profiles = Vec::new();
    let mut marked = Vec::new();
    // Mozilla stops at the first missing ProfileN section.
    for index in 0.. {
        let Some(values) = sections.get(&format!("Profile{index}")) else {
            break;
        };
        let Some(path) = values.get("Path").and_then(|path| {
            resolve(
                path,
                values.get("IsRelative").map(String::as_str) != Some("0"),
            )
        }) else {
            continue;
        };
        marked.push(values.get("Default").is_some_and(|value| value == "1"));
        profiles.push(DetectedProfile {
            name: values.get("Name").cloned().unwrap_or_default(),
            is_default: install_defaults.contains(&path),
            path,
        });
    }
    // Without a dedicated installation default, `Default=1` marks it.
    if !profiles.iter().any(|profile| profile.is_default) {
        for (profile, marked) in profiles.iter_mut().zip(marked) {
            profile.is_default = marked;
        }
    }
    profiles
}

/// Whether opening `path` stays on this computer. Windows connects to a
/// network share or device path, sending the user's credentials.
fn is_local(path: &Path) -> bool {
    match path.components().next() {
        Some(Component::Prefix(prefix)) => matches!(
            prefix.kind(),
            std::path::Prefix::Disk(_) | std::path::Prefix::VerbatimDisk(_)
        ),
        _ => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An absolute path on the platform running the test.
    const OLD: &str = if cfg!(windows) {
        r"C:\srv\old"
    } else {
        "/srv/old"
    };

    #[test]
    fn profiles_ini_prefers_the_installation_default() {
        let root = Path::new("/home/me/.thunderbird");
        let profiles = parse_profiles_ini(
            root,
            &format!(
                "[Install4F96D1932A9F858E]\nDefault=abcd.default-release\nLocked=1\n\n\
                 [Profile1]\nName=old\nIsRelative=0\nPath={OLD}\nDefault=1\n\n\
                 [Profile0]\nName=default-release\nIsRelative=1\nPath=abcd.default-release\n\n\
                 [Profile3]\nName=unreachable\nIsRelative=1\nPath=x\n\n\
                 [General]\nStartWithLastProfile=1\nVersion=2\n"
            ),
        );
        assert_eq!(
            profiles,
            [
                DetectedProfile {
                    name: "default-release".into(),
                    path: root.join("abcd.default-release"),
                    is_default: true,
                },
                DetectedProfile {
                    name: "old".into(),
                    path: PathBuf::from(OLD),
                    is_default: false,
                },
            ]
        );
    }

    #[test]
    fn profiles_ini_default_flags_stay_with_their_profiles() {
        let root = Path::new("/home/me/.thunderbird");
        // The first profile's path cannot be used, so only two are listed.
        let profiles = parse_profiles_ini(
            root,
            "\u{feff}[Profile0]\nName=gone\nIsRelative=1\nPath=../x\n\n\
             [Profile1]\nName=work\nIsRelative=1\nPath=w.work\n\n\
             [Profile2]\nName=main\nIsRelative=1\nPath=m.main\nDefault=1\n",
        );
        assert_eq!(
            profiles
                .iter()
                .map(|profile| (profile.name.as_str(), profile.is_default))
                .collect::<Vec<_>>(),
            [("work", false), ("main", true)]
        );
    }

    #[cfg(windows)]
    #[test]
    fn network_locations_are_not_local() {
        for path in [
            r"\\server\share\p",
            r"\\?\UNC\server\share\p",
            r"\\.\pipe\p",
        ] {
            assert!(!is_local(Path::new(path)), "{path}");
        }
        for path in [r"C:\Users\me\p", r"\\?\C:\Users\me\p"] {
            assert!(is_local(Path::new(path)), "{path}");
        }
    }

    #[test]
    fn relative_paths_cannot_escape() {
        assert_eq!(safe_relative("a/./b.txt"), Some(PathBuf::from("a/b.txt")));
        for path in ["../x", "a/../../x", "/etc/passwd", "", "."] {
            assert_eq!(safe_relative(path), None, "{path}");
        }
        let ini = "[Profile0]\nName=evil\nIsRelative=1\nPath=../../etc\n";
        assert!(parse_profiles_ini(Path::new("/r"), ini).is_empty());
    }

    #[test]
    fn choosing_the_thunderbird_data_folder_reads_its_default_profile() {
        let root = tempfile::tempdir().unwrap();
        let profile = root.path().join("abcd.default");
        std::fs::create_dir(&profile).unwrap();
        std::fs::write(
            profile.join("prefs.js"),
            include_str!("../../tests/fixtures/migration/prefs.js"),
        )
        .unwrap();
        std::fs::write(
            root.path().join("profiles.ini"),
            "[Profile0]\nName=default\nIsRelative=1\nPath=abcd.default\nDefault=1\n",
        )
        .unwrap();
        let inventory = read_profile_dir(root.path()).unwrap();
        assert_eq!(inventory.accounts.len(), 2);
    }
}
