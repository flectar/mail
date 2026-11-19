//! Translating messages through a service the user sets up (#240): DeepL,
//! LibreTranslate or any server that speaks the OpenAI chat API, local
//! models included. Nothing leaves the device until the user picks a service
//! and asks for a translation, or asks for a language to be translated
//! always. Translations stay on the device for some days and out of search.
//!
//! The reader offers to translate a message whose language, found on the
//! device, isn't one the user reads, as Gmail, Outlook and Firefox do, and
//! translates at once a language the user put on the Always list. A
//! message is translated as it is shown: its HTML body in pieces that keep
//! links and formatting (`document.rs`), or its text body a paragraph at a
//! time (`text.rs`).

use crate::Core;
use crate::accounts::credentials::{self, Slot as KeySlot};
use crate::db::repo;
use crate::error::{CoreError, Result};
use crate::models::{TranslationSettings, now_ms};
use rusqlite::{Connection, OptionalExtension, params};
use service::{Service, Target};
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

mod detect;
pub(crate) mod document;
mod languages;
mod service;
#[cfg(test)]
mod test_server;
mod text;

/// The services, as `TranslationSettings::service` names them.
pub const DEEPL: &str = "deepl";
pub const LIBRETRANSLATE: &str = "libretranslate";
pub const MODEL: &str = "model";

/// How long a translation is kept. Outlook keeps its translations 15 days.
const KEPT_FOR_MS: i64 = 15 * 24 * 60 * 60 * 1000;
/// The most characters one message sends, so a huge newsletter doesn't
/// spend a month of DeepL's free characters at once.
const MOST_CHARACTERS: usize = 100_000;
/// How long a service's list of languages is kept.
const TARGETS_KEPT_FOR: Duration = Duration::from_secs(24 * 60 * 60);

/// Where a piece's translation goes in its message.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Slot {
    /// The `n`th image description or link title.
    Attribute(usize),
    /// A whole run of text, tags and all.
    Run(usize),
    /// The `node`th text node with words in a run.
    Text { run: usize, node: usize },
    /// A block of lines in a plain-text message.
    Line(usize),
}

/// One piece of a message for the translation service. The text is escaped
/// like XML text (`&amp;`, `&lt;`, `&gt;`) and may carry numbered tags,
/// `<gN>…</gN>` around words and `<xN/>` for what stays as it is.
#[derive(Clone, Debug)]
pub(crate) struct Piece {
    pub(crate) text: String,
    /// The piece carries numbered tags the service must keep.
    pub(crate) tagged: bool,
    pub(crate) slot: Slot,
}

/// Why a translation didn't happen, for the reader to say.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TranslationError {
    /// No service is set up, or its key is missing.
    NotSetUp,
    /// The service refused the key.
    KeyRefused,
    /// The key's characters for this period are used up.
    QuotaUsed,
    /// The service is busy and asked to slow down.
    TooMany,
    /// The service couldn't be reached.
    Unreachable,
    /// The server address isn't https://, or http:// on this device.
    BadAddress,
    /// The message is too long or too complex to translate.
    TooLong,
    /// The message is in the language it would be translated into.
    SameLanguage,
    /// What the service said went wrong, in its words.
    Service(String),
}

impl std::fmt::Display for TranslationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TranslationError::NotSetUp => f.write_str("no translation service is set up"),
            TranslationError::KeyRefused => f.write_str("the translation service refused the key"),
            TranslationError::QuotaUsed => f.write_str("the translation quota is used up"),
            TranslationError::TooMany => f.write_str("the translation service is busy"),
            TranslationError::Unreachable => {
                f.write_str("the translation service can't be reached")
            }
            TranslationError::BadAddress => {
                f.write_str("the translation server address isn't allowed")
            }
            TranslationError::TooLong => f.write_str("the message is too long to translate"),
            TranslationError::SameLanguage => {
                f.write_str("the message is already in that language")
            }
            TranslationError::Service(message) => write!(f, "translation failed: {message}"),
        }
    }
}

impl From<TranslationError> for CoreError {
    fn from(error: TranslationError) -> Self {
        CoreError::Translation(error)
    }
}

/// The languages the user reads, as the app knows them.
#[derive(Clone, Debug, Default)]
pub struct ReaderLanguages {
    /// Language tags: the app's language and the system's, most preferred
    /// first. Firefox counts the same ones.
    pub reading: Vec<String>,
    /// The language to translate into while Settings names none.
    pub fallback_target: String,
}

/// What the reader offers for a message.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TranslationOffer {
    /// The message's language as a primary language subtag, when the
    /// detector is sure of it.
    pub language: Option<String>,
    /// Offer to translate it: a service is set up, the user doesn't read the
    /// language and didn't ask never to translate it.
    pub offer: bool,
    /// Translate it as it opens: its language is on the Always list.
    pub automatic: bool,
}

/// A message's translation, of its HTML body or, when it has none, its text
/// body.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MessageTranslation {
    pub html: Option<String>,
    pub text: Option<String>,
    /// The language it was translated from, as the service or the detector
    /// found it, when either did.
    pub source: Option<String>,
    /// The language it was translated into, as the service names it.
    pub target: String,
    /// The translation is in a language written right to left.
    pub right_to_left: bool,
}

/// A language the set-up service translates into.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TranslationTarget {
    /// The language tag the service takes, such as "de" or "pt-BR".
    pub code: String,
    /// The service's English name for it.
    pub name: String,
}

/// Where the user wants a language: always translated, never offered, or
/// offered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LanguageChoice {
    Always,
    Never,
    Ask,
}

fn key_slot(service: &str) -> Result<KeySlot> {
    match service {
        DEEPL => Ok(KeySlot::DeeplKey),
        LIBRETRANSLATE => Ok(KeySlot::LibreTranslateKey),
        // A model is the AI server's, with its key.
        MODEL => Ok(KeySlot::AiApiKey),
        _ => Err(TranslationError::NotSetUp.into()),
    }
}

/// The language to translate into: the one Settings names, else the app's.
fn wanted_target(settings: &TranslationSettings, languages: &ReaderLanguages) -> String {
    [settings.target.trim(), languages.fallback_target.trim()]
        .into_iter()
        .find(|tag| !tag.is_empty())
        .unwrap_or("en")
        .to_owned()
}

fn offer_for(
    language: Option<&str>,
    settings: &TranslationSettings,
    languages: &ReaderLanguages,
) -> TranslationOffer {
    let Some(language) = language else {
        return TranslationOffer::default();
    };
    let target = wanted_target(settings, languages);
    let reads = languages
        .reading
        .iter()
        .chain(std::iter::once(&target))
        .any(|tag| detect::primary(tag) == language);
    let listed = |list: &[String]| list.iter().any(|tag| detect::primary(tag) == language);
    let set_up = !settings.service.is_empty();
    TranslationOffer {
        language: Some(language.to_owned()),
        offer: set_up && settings.offer && !reads && !listed(&settings.never),
        automatic: set_up && !reads && listed(&settings.always),
    }
}

/// A language tag's parts: primary language, script and region.
fn parts(tag: &str) -> (String, Option<String>, Option<String>) {
    let mut primary = None;
    let mut script = None;
    let mut region = None;
    for (index, part) in tag.trim().split(['-', '_']).enumerate() {
        match (index, part.len()) {
            (0, _) => primary = Some(detect::primary(part)),
            (_, 4) if part.chars().all(|c| c.is_ascii_alphabetic()) => {
                script = Some(part.to_ascii_lowercase())
            }
            (_, 2) | (_, 3) => region = Some(part.to_ascii_uppercase()),
            _ => {}
        }
    }
    (primary.unwrap_or_default(), script, region)
}

/// The code a service takes for the language the user wants, from the
/// service's own list when there is one: the same tag, else the same
/// language and script or region, else the language. DeepL needs a variant
/// of English and Portuguese, LibreTranslate and DeepL a script of Chinese.
fn service_target(service: &Service, wanted: &str, targets: &[Target]) -> String {
    let (primary, script, region) = parts(wanted);
    let traditional =
        script.as_deref() == Some("hant") || matches!(region.as_deref(), Some("TW" | "HK" | "MO"));
    let fixed = match (service, primary.as_str()) {
        (Service::Model { .. }, _) => return wanted.trim().to_owned(),
        (Service::DeepL { .. }, "en") => {
            if matches!(
                region.as_deref(),
                Some("GB" | "IE" | "AU" | "NZ" | "ZA" | "IN" | "SG" | "HK" | "MT")
            ) {
                "en-GB".to_owned()
            } else {
                "en-US".to_owned()
            }
        }
        (Service::DeepL { .. }, "pt") => {
            if matches!(
                region.as_deref(),
                Some("PT" | "AO" | "MZ" | "CV" | "GW" | "ST" | "TL" | "MO")
            ) {
                "pt-PT".to_owned()
            } else {
                "pt-BR".to_owned()
            }
        }
        (Service::LibreTranslate { .. }, "pt") if region.as_deref() == Some("BR") => {
            "pt-BR".to_owned()
        }
        (_, "zh") if traditional => "zh-Hant".to_owned(),
        (_, "zh") => "zh-Hans".to_owned(),
        _ => primary.clone(),
    };
    if targets.is_empty() {
        return fixed;
    }
    let same = |code: &str| {
        targets
            .iter()
            .find(|target| target.code.eq_ignore_ascii_case(code))
            .map(|target| target.code.clone())
    };
    same(wanted.trim())
        .or_else(|| same(&fixed))
        .or_else(|| same(&primary))
        .or_else(|| {
            targets
                .iter()
                .find(|target| parts(&target.code).0 == primary)
                .map(|target| target.code.clone())
        })
        .unwrap_or(fixed)
}

/// Each service's languages and when they were asked for, by service.
type KeptTargets = Mutex<HashMap<String, (Instant, Vec<Target>)>>;

/// Each service's languages, kept for a day, as they change a few times a
/// year at most.
fn kept_targets() -> &'static KeptTargets {
    static TARGETS: std::sync::LazyLock<KeptTargets> = std::sync::LazyLock::new(Mutex::default);
    &TARGETS
}

async fn targets_of(service: &Service) -> std::result::Result<Vec<Target>, TranslationError> {
    let id = service.id();
    if let Some((at, targets)) = kept_targets()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(&id)
        && at.elapsed() < TARGETS_KEPT_FOR
    {
        return Ok(targets.clone());
    }
    let targets = service.targets().await?;
    kept_targets()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(id, (Instant::now(), targets.clone()));
    Ok(targets)
}

fn check_size(pieces: &[Piece]) -> std::result::Result<(), TranslationError> {
    let characters: usize = pieces.iter().map(|piece| piece.text.chars().count()).sum();
    if characters > MOST_CHARACTERS {
        return Err(TranslationError::TooLong);
    }
    Ok(())
}

fn joined(error: tokio::task::JoinError) -> TranslationError {
    TranslationError::Service(error.to_string())
}

/// An HTML body translated, with the language the service found it in.
/// `sanitize` runs the result through the sanitizer incoming mail goes
/// through. It is built from the message's own elements and only the text
/// comes from the service, so this is a second line of defence.
async fn translate_html(
    service: &Service,
    html: String,
    target: &str,
    context: &str,
    sanitize: bool,
) -> std::result::Result<(String, Option<String>), TranslationError> {
    let tags = service.keeps_tags();
    let (document, mut pieces) = tokio::task::spawn_blocking(move || {
        let document = document::Document::parse(&html)?;
        let pieces = document.pieces(tags);
        Some((document, pieces))
    })
    .await
    .map_err(joined)?
    .ok_or(TranslationError::TooLong)?;
    check_size(&pieces)?;
    let name = languages::english_name(target);
    let first = service.translate(&pieces, target, &name, context).await?;
    let mut texts = first.texts;
    let (document, misfits) = {
        let (pieces, texts) = (pieces.clone(), texts.clone());
        tokio::task::spawn_blocking(move || {
            let misfits = if tags {
                document.misfits(&pieces, &texts)
            } else {
                Vec::new()
            };
            (document, misfits)
        })
        .await
        .map_err(joined)?
    };
    if !misfits.is_empty() {
        let more = document.fallback_pieces(&misfits);
        let again = service.translate(&more, target, &name, context).await?;
        pieces.extend(more);
        texts.extend(again.texts);
    }
    let direction = if detect::right_to_left(target) {
        "rtl"
    } else {
        "ltr"
    };
    let html = tokio::task::spawn_blocking(move || {
        let html = document.translated(&pieces, &texts, direction);
        if sanitize {
            crate::mime::sanitize_html(&html)
        } else {
            html
        }
    })
    .await
    .map_err(joined)?;
    Ok((html, first.source))
}

/// A text body translated, with the language the service found it in.
async fn translate_text(
    service: &Service,
    text: &str,
    target: &str,
    context: &str,
) -> std::result::Result<(String, Option<String>), TranslationError> {
    let document = text::TextDocument::parse(text);
    let pieces = document.pieces();
    check_size(&pieces)?;
    let name = languages::english_name(target);
    let translated = service.translate(&pieces, target, &name, context).await?;
    Ok((
        document.translated(&pieces, &translated.texts),
        translated.source,
    ))
}

/// A message's subject and bodies, text and HTML.
type Body = (String, Option<String>, Option<String>);

fn body(conn: &Connection, message_id: i64) -> Result<Option<Body>> {
    Ok(conn
        .query_row(
            "SELECT m.subject, b.text_body, b.html_body FROM messages m
             LEFT JOIN message_bodies b ON b.message_id = m.id
             WHERE m.id = ?1",
            params![message_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?)
}

/// A kept translation: HTML, text and the language it came from.
type Kept = (Option<String>, Option<String>, Option<String>);

fn kept(conn: &Connection, message_id: i64, target: &str, service: &str) -> Result<Option<Kept>> {
    Ok(conn
        .query_row(
            "SELECT html, text, source FROM message_translations
             WHERE message_id = ?1 AND target = ?2 AND service = ?3 AND created_at >= ?4",
            params![message_id, target, service, now_ms() - KEPT_FOR_MS],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?)
}

#[allow(clippy::too_many_arguments)]
fn keep(
    conn: &Connection,
    message_id: i64,
    target: &str,
    service: &str,
    source: Option<&str>,
    html: Option<&str>,
    text: Option<&str>,
    now: i64,
) -> Result<()> {
    conn.execute(
        "DELETE FROM message_translations WHERE created_at < ?1",
        params![now - KEPT_FOR_MS],
    )?;
    conn.execute(
        "INSERT INTO message_translations (message_id, target, service, source, html, text, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
         ON CONFLICT(message_id, target, service) DO UPDATE SET
           source = excluded.source, html = excluded.html, text = excluded.text,
           created_at = excluded.created_at",
        params![message_id, target, service, source, html, text, now],
    )?;
    Ok(())
}

impl Core {
    pub async fn translation_settings(&self) -> Result<TranslationSettings> {
        self.db
            .read(|conn| Ok(repo::settings::get(conn)?.translation))
            .await
    }

    /// Saves how messages are translated. The Always and Never lists hold
    /// primary language subtags, each language once, and on one list at most.
    pub async fn set_translation_settings(
        &self,
        mut translation: TranslationSettings,
    ) -> Result<()> {
        let clean = |list: &mut Vec<String>| {
            let mut seen = Vec::new();
            for tag in list.drain(..) {
                let primary = detect::primary(&tag);
                if !primary.is_empty() && !seen.contains(&primary) {
                    seen.push(primary);
                }
            }
            *list = seen;
        };
        clean(&mut translation.always);
        clean(&mut translation.never);
        let never = translation.never.clone();
        translation.always.retain(|tag| !never.contains(tag));
        self.db
            .write(move |conn| {
                let tx = conn.transaction()?;
                let mut settings = repo::settings::get(&tx)?;
                settings.translation = translation;
                repo::settings::set(&tx, &settings)?;
                tx.commit()?;
                Ok(())
            })
            .await
    }

    /// Puts a language on the Always list, the Never list, or neither.
    pub async fn set_language_choice(
        &self,
        language: String,
        choice: LanguageChoice,
    ) -> Result<()> {
        let language = detect::primary(&language);
        self.db
            .write(move |conn| {
                let tx = conn.transaction()?;
                let mut settings = repo::settings::get(&tx)?;
                let translation = &mut settings.translation;
                translation
                    .always
                    .retain(|tag| detect::primary(tag) != language);
                translation
                    .never
                    .retain(|tag| detect::primary(tag) != language);
                match choice {
                    LanguageChoice::Always => translation.always.push(language),
                    LanguageChoice::Never => translation.never.push(language),
                    LanguageChoice::Ask => {}
                }
                repo::settings::set(&tx, &settings)?;
                tx.commit()?;
                Ok(())
            })
            .await
    }

    /// Saves a service's key, or removes it when `key` is empty.
    pub async fn set_translation_key(&self, service: String, key: String) -> Result<()> {
        let slot = key_slot(&service)?;
        if key.trim().is_empty() {
            return credentials::delete_async(self.credentials.clone(), 0, slot).await;
        }
        credentials::store_async(self.credentials.clone(), 0, slot, key.trim().to_owned()).await
    }

    /// Whether a service's key is saved.
    pub async fn translation_key_saved(&self, service: String) -> Result<bool> {
        let slot = key_slot(&service)?;
        match credentials::load_async(self.credentials.clone(), 0, slot).await {
            Ok(key) => Ok(!key.trim().is_empty()),
            Err(CoreError::CredentialStoreUnavailable(message)) => {
                Err(CoreError::CredentialStoreUnavailable(message))
            }
            Err(_) => Ok(false),
        }
    }

    /// The translation settings and the service they set up.
    async fn translation_setup(&self) -> Result<(TranslationSettings, Service)> {
        let settings = self.db.read(|conn| repo::settings::get(conn)).await?;
        let service = self
            .translation_service(&settings.translation, &settings.ai)
            .await?;
        Ok((settings.translation, service))
    }

    async fn translation_service(
        &self,
        settings: &TranslationSettings,
        ai: &crate::models::AiSettings,
    ) -> Result<Service> {
        let key = |slot| async move {
            match credentials::load_async(self.credentials.clone(), 0, slot).await {
                Ok(key) => Ok(key),
                Err(CoreError::CredentialStoreUnavailable(message)) => {
                    Err(CoreError::CredentialStoreUnavailable(message))
                }
                Err(_) => Ok(String::new()),
            }
        };
        let not_set_up = || CoreError::from(TranslationError::NotSetUp);
        match settings.service.as_str() {
            DEEPL => {
                let key = key(KeySlot::DeeplKey).await?;
                if key.trim().is_empty() {
                    return Err(not_set_up());
                }
                Ok(Service::deepl(key))
            }
            LIBRETRANSLATE if !settings.libretranslate_url.trim().is_empty() => {
                Ok(Service::LibreTranslate {
                    url: settings.libretranslate_url.trim().to_owned(),
                    key: key(KeySlot::LibreTranslateKey).await?,
                })
            }
            // The AI server set up for the app's AI features (#241).
            MODEL if ai.ready() => Ok(Service::Model {
                url: ai.url.trim().to_owned(),
                model: ai.model.trim().to_owned(),
                key: key(KeySlot::AiApiKey).await?,
            }),
            _ => Err(not_set_up()),
        }
    }

    /// The languages the set-up service translates into.
    pub async fn translation_targets(&self) -> Result<Vec<TranslationTarget>> {
        let (_, service) = self.translation_setup().await?;
        if matches!(service, Service::Model { .. }) {
            return Ok(languages::model_targets()
                .into_iter()
                .map(|(code, name)| TranslationTarget { code, name })
                .collect());
        }
        Ok(targets_of(&service)
            .await?
            .into_iter()
            .map(|target| TranslationTarget {
                code: target.code,
                name: target.name,
            })
            .collect())
    }

    /// What the reader offers for a message, from its language found on the
    /// device. Nothing is sent anywhere.
    pub async fn translation_offer(
        &self,
        message_id: i64,
        languages: ReaderLanguages,
    ) -> Result<TranslationOffer> {
        let (settings, body) = self
            .db
            .read(move |conn| {
                Ok((
                    repo::settings::get(conn)?.translation,
                    body(conn, message_id)?,
                ))
            })
            .await?;
        let Some((_, text, html)) = body else {
            return Ok(TranslationOffer::default());
        };
        let language =
            tokio::task::spawn_blocking(move || detect::language(text.as_deref(), html.as_deref()))
                .await
                .ok()
                .flatten();
        Ok(offer_for(language, &settings, &languages))
    }

    /// A message translated into the user's language, from the translations
    /// kept on the device when there is one.
    pub async fn translate_message(
        &self,
        message_id: i64,
        languages: ReaderLanguages,
    ) -> Result<MessageTranslation> {
        let (settings, service) = self.translation_setup().await?;
        let wanted = wanted_target(&settings, &languages);
        let targets = match &service {
            Service::Model { .. } => Vec::new(),
            other => targets_of(other).await.unwrap_or_default(),
        };
        let target = service_target(&service, &wanted, &targets);
        let id = service.id();
        let right_to_left = detect::right_to_left(&target);
        let lookup = (target.clone(), id.clone());
        let found = self
            .db
            .read(move |conn| kept(conn, message_id, &lookup.0, &lookup.1))
            .await?;
        if let Some((html, text, source)) = found {
            return Ok(MessageTranslation {
                html: self.shown_html(message_id, html).await,
                text,
                source,
                target,
                right_to_left,
            });
        }
        let (subject, text_body, html_body) = self
            .db
            .read(move |conn| body(conn, message_id))
            .await?
            .ok_or_else(|| CoreError::NotFound(format!("message {message_id}")))?;
        let (detected, whole) = {
            let (text, html) = (text_body.clone(), html_body.clone());
            tokio::task::spawn_blocking(move || {
                (
                    detect::language(text.as_deref(), html.as_deref()),
                    detect::whole_language(text.as_deref(), html.as_deref()),
                )
            })
            .await
            .unwrap_or_default()
        };
        // DeepL bills text in the language it's translated into all the
        // same. The whole message counts, so a document forwarded under a
        // note in the user's language is still translated.
        if whole.is_some_and(|language| language == detect::primary(&target)) {
            return Err(TranslationError::SameLanguage.into());
        }
        let (html, text, source) = match (html_body, text_body) {
            (Some(html), _) if !html.trim().is_empty() => {
                let (html, source) =
                    translate_html(&service, html, &target, &subject, true).await?;
                (Some(html), None, source)
            }
            (_, Some(text)) if !text.trim().is_empty() => {
                let (text, source) = translate_text(&service, &text, &target, &subject).await?;
                (None, Some(text), source)
            }
            _ => {
                return Err(CoreError::NotFound(format!(
                    "the body of message {message_id}"
                )));
            }
        };
        let source = source.or_else(|| detected.map(str::to_owned));
        let stored = (target.clone(), source.clone(), html.clone(), text.clone());
        self.db
            .write(move |conn| {
                keep(
                    conn,
                    message_id,
                    &stored.0,
                    &id,
                    stored.1.as_deref(),
                    stored.2.as_deref(),
                    stored.3.as_deref(),
                    now_ms(),
                )
            })
            .await?;
        Ok(MessageTranslation {
            html: self.shown_html(message_id, html).await,
            text,
            source,
            target,
            right_to_left,
        })
    }

    /// Kept HTML as the reader shows it, with its embedded images.
    async fn shown_html(&self, message_id: i64, html: Option<String>) -> Option<String> {
        match html {
            Some(html) => Some(
                self.inline_cid_images(message_id, html, crate::InlineImageLimits::READER)
                    .await,
            ),
            None => None,
        }
    }

    /// Part of a message being written, as HTML from the composer,
    /// translated into `target`. Nothing is kept.
    pub async fn translate_draft_html(&self, html: String, target: String) -> Result<String> {
        let (_, service) = self.translation_setup().await?;
        let targets = match &service {
            Service::Model { .. } => Vec::new(),
            other => targets_of(other).await.unwrap_or_default(),
        };
        let target = service_target(&service, &target, &targets);
        Ok(translate_html(&service, html, &target, "", false).await?.0)
    }

    /// Plain text being written, translated into `target`.
    pub async fn translate_draft_text(&self, text: String, target: String) -> Result<String> {
        let (_, service) = self.translation_setup().await?;
        let targets = match &service {
            Service::Model { .. } => Vec::new(),
            other => targets_of(other).await.unwrap_or_default(),
        };
        let target = service_target(&service, &target, &targets);
        Ok(translate_text(&service, &text, &target, "").await?.0)
    }
}

#[cfg(test)]
#[path = "mod_tests.rs"]
mod tests;
