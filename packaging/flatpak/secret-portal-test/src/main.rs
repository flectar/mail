//! Drives the Flatpak's real credential store from inside a sandbox, for the
//! Secret portal test in run.sh. Each command prints one line, "OK value" or
//! "ERR error", so the script can compare it.
#[path = "../../../../src/flatpak_secrets.rs"]
mod flatpak_secrets;

use flectar_mail_core::accounts::credentials::{CredentialStore, Slot, SystemCredentialStore};
use flectar_mail_core::config::Paths;

fn main() {
    let command = std::env::args().nth(1).expect("command");
    let data =
        std::path::PathBuf::from(std::env::var("XDG_DATA_HOME").unwrap()).join("flectar-mail");
    let cache =
        std::path::PathBuf::from(std::env::var("XDG_CACHE_HOME").unwrap()).join("flectar-mail");
    std::fs::create_dir_all(&data).unwrap();
    std::fs::create_dir_all(&cache).unwrap();
    let paths = Paths::new(data, cache);
    let result = match command.as_str() {
        "seed-keyring" => SystemCredentialStore::for_profile(&paths)
            .store(7, Slot::Password, "old-keyring-secret")
            .map(|()| "seeded".to_string()),
        "keyring-load" => SystemCredentialStore::for_profile(&paths).load(7, Slot::Password),
        "store" => flatpak_secrets::credential_store(&paths)
            .store(1, Slot::Password, "e2e-portal-secret")
            .map(|()| "stored".to_string()),
        "load" => flatpak_secrets::credential_store(&paths).load(1, Slot::Password),
        "load-moved" => flatpak_secrets::credential_store(&paths).load(7, Slot::Password),
        other => panic!("unknown command {other}"),
    };
    match result {
        Ok(value) => println!("OK {value}"),
        Err(error) => println!("ERR {error}"),
    }
}
