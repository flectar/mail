//! Desktop credential storage against a stand-in Secret Service on a private
//! D-Bus. A session without a default collection gets one through the
//! provider's prompt, as libsecret creates it, and a missing provider or a
//! dismissed prompt is named in the error. The bus needs `dbus-daemon`; CI
//! fails instead of skipping without it.
#![cfg(secret_service)]

use dbus::arg::{PropMap, RefArg, Variant};
use dbus::channel::Channel;
use dbus::message::MessageType;
use dbus::strings::ErrorName;
use dbus::{Message, Path};
use flectar_mail_core::accounts::credentials::{CredentialStore, Slot, SystemCredentialStore};
use flectar_mail_core::error::CoreError;
use std::collections::HashMap;
use std::ffi::CString;
use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, mpsc};
use std::thread::JoinHandle;
use std::time::Duration;

const SESSION: &str = "/org/freedesktop/secrets/session/1";

/// The Secret Service's secret: session, parameters, value and content type.
type Secret<'a> = (Path<'a>, Vec<u8>, Vec<u8>, String);

/// A private session bus with no activatable services.
struct Bus {
    daemon: Child,
    address: String,
    _directory: tempfile::TempDir,
}

impl Bus {
    fn start() -> Option<Self> {
        let directory = tempfile::tempdir().unwrap();
        let config = directory.path().join("session.conf");
        std::fs::write(
            &config,
            format!(
                r#"<!DOCTYPE busconfig PUBLIC "-//freedesktop//DTD D-Bus Bus Configuration 1.0//EN"
 "http://www.freedesktop.org/standards/dbus/1.0/busconfig.dtd">
<busconfig>
  <type>session</type>
  <listen>unix:path={}</listen>
  <auth>EXTERNAL</auth>
  <policy context="default">
    <allow send_destination="*" eavesdrop="true"/>
    <allow eavesdrop="true"/>
    <allow own="*"/>
  </policy>
</busconfig>
"#,
                directory.path().join("bus").display()
            ),
        )
        .unwrap();
        let mut daemon = match Command::new("dbus-daemon")
            .arg(format!("--config-file={}", config.display()))
            .args(["--nofork", "--print-address=1"])
            .stdout(Stdio::piped())
            .spawn()
        {
            Ok(daemon) => daemon,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return None,
            Err(error) => panic!("could not start dbus-daemon: {error}"),
        };
        let mut address = String::new();
        BufReader::new(daemon.stdout.take().unwrap())
            .read_line(&mut address)
            .unwrap();
        let address = address.trim().to_owned();
        assert!(!address.is_empty(), "dbus-daemon printed no address");
        Some(Self {
            daemon,
            address,
            _directory: directory,
        })
    }
}

impl Drop for Bus {
    fn drop(&mut self) {
        let _ = self.daemon.kill();
        let _ = self.daemon.wait();
    }
}

/// The stand-in provider's single keyring.
#[derive(Default)]
struct Keyring {
    /// Whether the user dismisses the provider's prompts.
    dismiss_prompts: bool,
    /// The collection behind the `default` alias.
    default: Option<Collection>,
    /// The label and alias of every CreateCollection call.
    created: Vec<(String, String)>,
    /// Pending prompts, with the label and alias of the collection each creates.
    prompts: HashMap<String, (String, String)>,
    objects: usize,
}

struct Collection {
    path: String,
    label: String,
    items: Vec<Item>,
}

struct Item {
    path: String,
    attributes: HashMap<String, String>,
    secret: Vec<u8>,
}

impl Keyring {
    fn object_path(&mut self, parent: &str) -> String {
        self.objects += 1;
        format!("{parent}/o{}", self.objects)
    }

    fn search(&self, query: &HashMap<String, String>) -> Vec<Path<'static>> {
        self.default
            .iter()
            .flat_map(|collection| &collection.items)
            .filter(|item| {
                query
                    .iter()
                    .all(|(key, value)| item.attributes.get(key) == Some(value))
            })
            .map(|item| Path::from(item.path.clone()))
            .collect()
    }

    fn item(&mut self, path: &str) -> &mut Item {
        self.default
            .as_mut()
            .and_then(|collection| collection.items.iter_mut().find(|item| item.path == path))
            .expect("a stored item")
    }

    /// Answer a method call, followed by any signal it raises.
    fn answer(&mut self, call: &Message) -> Vec<Message> {
        let path = call.path().unwrap().to_string();
        let interface = call.interface().map(|name| name.to_string());
        let member = call.member().unwrap().to_string();
        let reply = call.method_return();
        match (interface.as_deref().unwrap_or_default(), member.as_str()) {
            ("org.freedesktop.Secret.Service", "OpenSession") => {
                vec![reply.append2(Variant(String::new()), Path::from(SESSION))]
            }
            ("org.freedesktop.Secret.Service", "ReadAlias") => {
                let alias: &str = call.read1().unwrap();
                let collection = match (&self.default, alias) {
                    (Some(collection), "default") => collection.path.clone(),
                    _ => "/".into(),
                };
                vec![reply.append1(Path::from(collection))]
            }
            ("org.freedesktop.Secret.Service", "SearchItems") => {
                let query: HashMap<String, String> = call.read1().unwrap();
                vec![reply.append2(self.search(&query), Vec::<Path>::new())]
            }
            ("org.freedesktop.Secret.Service", "CreateCollection") => {
                let (properties, alias): (PropMap, String) = call.read2().unwrap();
                let label = properties["org.freedesktop.Secret.Collection.Label"]
                    .0
                    .as_str()
                    .unwrap()
                    .to_owned();
                self.created.push((label.clone(), alias.clone()));
                let prompt = self.object_path("/org/freedesktop/secrets/prompt");
                self.prompts.insert(prompt.clone(), (label, alias));
                vec![reply.append2(Path::from("/"), Path::from(prompt))]
            }
            ("org.freedesktop.Secret.Prompt", "Prompt") => {
                let (label, alias) = self.prompts.remove(&path).unwrap();
                let completed =
                    Message::new_signal(path, "org.freedesktop.Secret.Prompt", "Completed")
                        .unwrap();
                let completed = if self.dismiss_prompts {
                    completed.append2(true, Variant(String::new()))
                } else {
                    assert_eq!(alias, "default");
                    let collection = self.object_path("/org/freedesktop/secrets/collection");
                    self.default = Some(Collection {
                        path: collection.clone(),
                        label,
                        items: Vec::new(),
                    });
                    completed.append2(false, Variant(Path::from(collection)))
                };
                vec![reply, completed]
            }
            ("org.freedesktop.DBus.Properties", "Get") => {
                let (_, property): (&str, &str) = call.read2().unwrap();
                assert_eq!(property, "Locked");
                vec![reply.append1(Variant(false))]
            }
            ("org.freedesktop.Secret.Collection", "SearchItems") => {
                let query: HashMap<String, String> = call.read1().unwrap();
                vec![reply.append1(self.search(&query))]
            }
            ("org.freedesktop.Secret.Collection", "CreateItem") => {
                let (properties, (_, _, secret, _), replace): (PropMap, Secret, bool) =
                    call.read3().unwrap();
                let attributes =
                    string_map(&*properties["org.freedesktop.Secret.Item.Attributes"].0);
                let new_item = self.object_path(&path);
                let collection = self
                    .default
                    .as_mut()
                    .filter(|collection| collection.path == path)
                    .expect("items go into the default collection");
                let existing = collection
                    .items
                    .iter_mut()
                    .find(|item| replace && item.attributes == attributes);
                let item = match existing {
                    Some(item) => {
                        item.secret = secret;
                        item.path.clone()
                    }
                    None => {
                        collection.items.push(Item {
                            path: new_item.clone(),
                            attributes,
                            secret,
                        });
                        new_item
                    }
                };
                vec![reply.append2(Path::from(item), Path::from("/"))]
            }
            ("org.freedesktop.Secret.Item", "GetSecret") => {
                let secret = self.item(&path).secret.clone();
                vec![reply.append1((Path::from(SESSION), Vec::<u8>::new(), secret, "text/plain"))]
            }
            ("org.freedesktop.Secret.Item", "SetSecret") => {
                let (_, _, secret, _): Secret = call.read1().unwrap();
                self.item(&path).secret = secret;
                vec![reply]
            }
            ("org.freedesktop.Secret.Item", "Delete") => {
                if let Some(collection) = self.default.as_mut() {
                    collection.items.retain(|item| item.path != path);
                }
                vec![reply.append1(Path::from("/"))]
            }
            (interface, member) => vec![call.error(
                &ErrorName::from("org.freedesktop.DBus.Error.UnknownMethod"),
                &CString::new(format!("the stand-in has no {interface}.{member}")).unwrap(),
            )],
        }
    }
}

/// The string pairs of a dictionary inside a variant.
fn string_map(dictionary: &dyn RefArg) -> HashMap<String, String> {
    let mut entries = dictionary.as_iter().expect("a dictionary");
    let mut map = HashMap::new();
    while let (Some(key), Some(value)) = (entries.next(), entries.next()) {
        map.insert(
            key.as_str().unwrap().to_owned(),
            value.as_str().unwrap().to_owned(),
        );
    }
    map
}

/// Serves a [`Keyring`] as `org.freedesktop.secrets` until dropped.
struct Provider {
    keyring: Arc<Mutex<Keyring>>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Provider {
    fn start(address: &str, keyring: Keyring) -> Self {
        let keyring = Arc::new(Mutex::new(keyring));
        let stop = Arc::new(AtomicBool::new(false));
        let (ready, started) = mpsc::channel();
        let thread = std::thread::spawn({
            let address = address.to_owned();
            let keyring = keyring.clone();
            let stop = stop.clone();
            move || {
                let mut channel = Channel::open_private(&address).unwrap();
                channel.register().unwrap();
                let request = Message::new_method_call(
                    "org.freedesktop.DBus",
                    "/org/freedesktop/DBus",
                    "org.freedesktop.DBus",
                    "RequestName",
                )
                .unwrap()
                // DBUS_NAME_FLAG_DO_NOT_QUEUE
                .append2("org.freedesktop.secrets", 4u32);
                let owner = channel
                    .send_with_reply_and_block(request, Duration::from_secs(5))
                    .unwrap();
                assert_eq!(owner.read1::<u32>().unwrap(), 1, "primary owner");
                ready.send(()).unwrap();
                while !stop.load(Ordering::Relaxed) {
                    let Some(message) = channel
                        .blocking_pop_message(Duration::from_millis(10))
                        .unwrap()
                    else {
                        continue;
                    };
                    if message.msg_type() != MessageType::MethodCall {
                        continue;
                    }
                    for answer in keyring.lock().unwrap().answer(&message) {
                        channel.send(answer).unwrap();
                    }
                    channel.flush();
                }
            }
        });
        started.recv().expect("the stand-in provider started");
        Self {
            keyring,
            stop,
            thread: Some(thread),
        }
    }

    fn keyring(&self) -> MutexGuard<'_, Keyring> {
        self.keyring.lock().unwrap()
    }
}

impl Drop for Provider {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn nothing_stored(result: Result<String, CoreError>) -> bool {
    matches!(result, Err(CoreError::Auth(message)) if message == "no stored credential")
}

#[test]
fn account_setup_creates_a_missing_default_keyring() {
    let Some(bus) = Bus::start() else {
        assert!(
            std::env::var_os("CI").is_none(),
            "CI needs dbus-daemon for this test"
        );
        eprintln!("skipped: dbus-daemon is not installed");
        return;
    };
    // keyring connects to the session bus named here. This binary has no other
    // test reading the environment meanwhile.
    unsafe {
        std::env::set_var("DBUS_SESSION_BUS_ADDRESS", &bus.address);
        std::env::remove_var("FLECTAR_MAIL_CREDENTIALS_INSECURE_FILE");
    }
    // keyring waits as long as a prompt stays open. Fail instead of stalling
    // the run when one never completes; dropping the bus ends the stuck call.
    let (finished, outcome) = mpsc::channel();
    let setup = std::thread::spawn({
        let address = bus.address.clone();
        move || {
            set_up_accounts(&address);
            let _ = finished.send(());
        }
    });
    if let Err(mpsc::RecvTimeoutError::Timeout) = outcome.recv_timeout(Duration::from_secs(60)) {
        panic!("the credential store was still waiting after a minute");
    }
    if let Err(panic) = setup.join() {
        std::panic::resume_unwind(panic);
    }
}

fn set_up_accounts(address: &str) {
    let store = SystemCredentialStore;

    let error = store.check_available().unwrap_err().to_string();
    assert!(
        error.starts_with(
            "secure credential storage is unavailable: no keyring service is running;"
        ),
        "{error}"
    );
    assert!(
        error.contains("(org.freedesktop.DBus.Error.ServiceUnknown: "),
        "{error}"
    );

    let provider = Provider::start(
        address,
        Keyring {
            dismiss_prompts: true,
            ..Keyring::default()
        },
    );
    let error = store.check_available().unwrap_err().to_string();
    assert_eq!(
        error,
        "secure credential storage is unavailable: the keyring prompt was dismissed or could not \
         be shown; try again and unlock or create the keyring when asked"
    );
    assert_eq!(
        provider.keyring().created,
        [("Default keyring".to_owned(), "default".to_owned())]
    );
    // Reading or removing credentials finds nothing and creates no keyring.
    assert!(nothing_stored(store.load(7, Slot::Password)));
    store.delete_all(7).unwrap();
    assert_eq!(provider.keyring().created.len(), 1);
    assert!(provider.keyring().default.is_none());

    provider.keyring().dismiss_prompts = false;
    store.check_available().unwrap();
    store.store(7, Slot::Password, "hunter2").unwrap();
    assert_eq!(store.load(7, Slot::Password).unwrap(), "hunter2");
    {
        let keyring = provider.keyring();
        assert_eq!(keyring.created.len(), 2);
        let collection = keyring.default.as_ref().unwrap();
        assert_eq!(collection.label, "Default keyring");
        assert_eq!(collection.items.len(), 1, "the probe removed its entry");
    }

    store.store(7, Slot::Password, "correct horse").unwrap();
    assert_eq!(store.load(7, Slot::Password).unwrap(), "correct horse");
    store.delete_all(7).unwrap();
    assert!(nothing_stored(store.load(7, Slot::Password)));
    assert_eq!(
        provider.keyring().created.len(),
        2,
        "one keyring was created"
    );
}
