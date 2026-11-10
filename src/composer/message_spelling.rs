//! Spell checking of one message: a check after each pause in typing, the words it finds
//! underlined in the editor, and the editor's menu that corrects them.

use super::*;
use crate::rich_compose::Followed;
use crate::spelling::Spelling;
use std::ops::Range;

/// How long typing pauses before the message is checked again.
const CHECK_DELAY: Duration = Duration::from_millis(300);
/// The most corrections the menu offers, as browsers offer.
const MAX_SUGGESTIONS: usize = 5;

/// The spelling state of one message.
#[derive(Default)]
pub(super) struct MessageSpelling {
    timer: Timer,
    /// A check is running.
    running: Cell<bool>,
    /// The revision of the text the latest check began from.
    checked: Cell<Option<u64>>,
    /// What the latest check found, and the revision it checked, to underline again after the
    /// editor lets go of its layout.
    found: RefCell<Option<(u64, Vec<Range<usize>>)>>,
    /// Words the user chose to ignore in this message.
    ignored: RefCell<Vec<String>>,
    /// The word the editor menu is open for.
    menu_word: RefCell<Option<MenuWord>>,
    /// Bumped for each menu, so corrections found for an older one are dropped.
    menu_generation: Cell<u64>,
    /// The languages the menu lists, in its order.
    menu_languages: RefCell<Vec<String>>,
}

/// A misspelled word as the editor menu found it.
struct MenuWord {
    revision: u64,
    range: Range<usize>,
    word: String,
    suggestions: Vec<String>,
}

impl MessageSpelling {
    fn clear(&self) {
        self.timer.stop();
        self.checked.set(None);
        *self.found.borrow_mut() = None;
        self.ignored.borrow_mut().clear();
        *self.menu_word.borrow_mut() = None;
        self.menu_generation
            .set(self.menu_generation.get().wrapping_add(1));
    }
}

/// Spelling settings or the personal dictionary changed: check every open message again, or
/// take its underlines away when spelling is off.
pub(crate) fn spelling_changed() {
    with_composers(|composers| {
        for session in composers.sessions.borrow().iter() {
            session.spelling.checked.set(None);
            *session.spelling.found.borrow_mut() = None;
            if let Some(host) = session.host() {
                apply_spelling_state(&host.ui());
            }
            if crate::spelling::service().is_some_and(|spelling| spelling.enabled()) {
                session.check_spelling();
            } else {
                session.show_misspellings(None);
            }
        }
    });
}

/// Whether spelling can be checked, and is, for the editor menu.
pub(super) fn apply_spelling_state(ui: &ComposerUi<'_>) {
    let spelling = crate::spelling::service();
    ui.set_spelling_available(spelling.as_ref().is_some_and(|s| s.available()));
    ui.set_spelling_enabled(spelling.as_ref().is_some_and(|s| s.enabled()));
}

impl ComposeSession {
    /// Check the message once typing pauses, unless this text was checked already.
    pub(super) fn check_spelling_soon(&self) {
        if !spelling_enabled() || self.disposed.get() {
            return;
        }
        if self.spelling.checked.get() == Some(self.document.borrow().revision()) {
            return;
        }
        let session = self.this.clone();
        self.spelling
            .timer
            .start(slint::TimerMode::SingleShot, CHECK_DELAY, move || {
                if let Some(session) = session.upgrade() {
                    session.check_spelling();
                }
            });
    }

    /// The message closed. The last one to close lets go of the checker.
    pub(super) fn clear_spelling(&self) {
        self.spelling.clear();
        let writing = with_composers(|composers| {
            composers
                .sessions
                .borrow()
                .iter()
                .any(|session| !session.disposed.get())
        });
        if writing == Some(false)
            && let Some(spelling) = crate::spelling::service()
        {
            spelling.release();
        }
    }

    /// Give a fresh editor what the latest check found, before it draws the message.
    pub(super) fn underline_found(
        &self,
        document: &RichComposeDocument,
        editor: &mut LazyComposeEditor,
    ) {
        if let Some((revision, found)) = &*self.spelling.found.borrow()
            && spelling_enabled()
        {
            editor.set_misspellings(document, *revision, found.clone());
        }
    }

    fn check_spelling(&self) {
        let Some(spelling) = crate::spelling::service().filter(|s| s.enabled()) else {
            return;
        };
        // A check that is running checks again when it ends, if the text changed since.
        if self.disposed.get() || self.spelling.running.get() {
            return;
        }
        let (text, skip, revision) = {
            let document = self.document.borrow();
            let skip = crate::spelling::unchecked_ranges(
                document.text(),
                document.style_runs(),
                self.signature.borrow().as_deref(),
            );
            (document.text().to_owned(), skip, document.revision())
        };
        // Each message's first check lists the dictionaries installed now.
        if self.spelling.checked.get().is_none() {
            spelling.refresh_languages();
        }
        let ignored = self.spelling.ignored.borrow().clone();
        let Some(task) = spelling.check(text, skip, ignored) else {
            return;
        };
        self.spelling.running.set(true);
        self.spelling.checked.set(Some(revision));
        let abort = task.abort_handle();
        let session = self.this.clone();
        if slint::spawn_local(async move {
            let result = task.await;
            let Some(session) = session.upgrade() else {
                return;
            };
            session.spelling.running.set(false);
            if session.disposed.get() || session.spelling.checked.get() != Some(revision) {
                // Spelling changed while this ran; another check is under way.
                session.check_spelling_soon();
                return;
            }
            match result {
                Ok(Ok(found)) => {
                    *session.spelling.found.borrow_mut() = Some((revision, found.clone()));
                    session.show_misspellings(Some((revision, found)));
                }
                Ok(Err(error)) => tracing::warn!(%error, "spell check failed"),
                Err(error) => tracing::warn!(%error, "spell check stopped"),
            }
            // Text typed while it ran is checked next.
            session.check_spelling_soon();
        })
        .is_err()
        {
            abort.abort();
            self.spelling.running.set(false);
        }
    }

    /// Underline `found`, the misspelled words of the text at a revision, or none. A window
    /// that isn't drawn gets them when it is drawn again, from `found`, without building an
    /// editor for them now.
    fn show_misspellings(&self, found: Option<(u64, Vec<Range<usize>>)>) {
        let Some(host) = self.host() else {
            return;
        };
        let view = host.view();
        if self.disposed.get() || !view.ui.get_open() || view.suspended {
            return;
        }
        let document = self.document.borrow();
        let mut editor = self.editor.borrow_mut();
        let (revision, ranges) = found.unwrap_or((document.revision(), Vec::new()));
        editor.set_misspellings(&document, revision, ranges);
        apply_compose_editor_surface(&view, &document, document.selection(), &mut editor);
    }

    /// The user asked for the editor menu at a point of the editor, or with the keyboard at
    /// the caret. It opens once the corrections for the word there, if misspelled, are ready.
    pub(super) fn open_editor_menu(&self, point: Option<(f32, f32)>) {
        let Some(host) = self.host() else {
            return;
        };
        let ui = host.ui();
        let generation = self.spelling.menu_generation.get().wrapping_add(1);
        self.spelling.menu_generation.set(generation);
        *self.spelling.menu_word.borrow_mut() = None;
        ui.set_spelling_word("".into());
        ui.set_spelling_suggestions(ModelRc::default());
        let spelling = crate::spelling::service();
        if let Some(spelling) = &spelling {
            spelling.refresh_languages();
            self.list_spelling_languages(&ui, spelling);
        }
        apply_spelling_state(&ui);
        let word = spelling.as_ref().filter(|s| s.enabled()).and_then(|_| {
            let document = self.document.borrow();
            let mut editor = self.editor.borrow_mut();
            let range = match point {
                Some((x, y)) => editor.misspelling_at(x, y, host.window().scale_factor()),
                None => editor.misspelling_at_caret(&document),
            }?;
            let word = document.text().get(range.clone())?.to_owned();
            Some((document.revision(), range, word))
        });
        let (Some(spelling), Some((revision, range, word))) = (spelling, word) else {
            ui.set_editor_menu_serial(ui.get_editor_menu_serial().wrapping_add(1));
            return;
        };
        ui.set_spelling_word(word.as_str().into());
        let Some(task) = spelling.suggest(word.clone()) else {
            ui.set_editor_menu_serial(ui.get_editor_menu_serial().wrapping_add(1));
            return;
        };
        let session = self.this.clone();
        let abort = task.abort_handle();
        if slint::spawn_local(async move {
            let suggestions = match task.await {
                Ok(Ok(mut suggestions)) => {
                    suggestions.truncate(MAX_SUGGESTIONS);
                    suggestions
                }
                Ok(Err(error)) => {
                    tracing::warn!(%error, "no spelling suggestions");
                    Vec::new()
                }
                Err(_) => Vec::new(),
            };
            let Some(session) = session.upgrade() else {
                return;
            };
            if session.spelling.menu_generation.get() != generation {
                return;
            }
            let Some(host) = session.host() else {
                return;
            };
            let ui = host.ui();
            let model: Vec<SharedString> = suggestions.iter().map(|s| s.as_str().into()).collect();
            ui.set_spelling_suggestions(ModelRc::new(VecModel::from(model)));
            *session.spelling.menu_word.borrow_mut() = Some(MenuWord {
                revision,
                range,
                word,
                suggestions,
            });
            ui.set_editor_menu_serial(ui.get_editor_menu_serial().wrapping_add(1));
        })
        .is_err()
        {
            abort.abort();
        }
    }

    fn list_spelling_languages(&self, ui: &ComposerUi<'_>, spelling: &Spelling) {
        let languages = spelling.languages();
        let rows: Vec<SpellingLanguage> = languages
            .iter()
            .map(|(language, checked)| SpellingLanguage {
                name: language.name.as_str().into(),
                checked: *checked,
            })
            .collect();
        *self.spelling.menu_languages.borrow_mut() = languages
            .into_iter()
            .map(|(language, _)| language.id)
            .collect();
        ui.set_spelling_languages(ModelRc::new(VecModel::from(rows)));
    }

    /// Where the menu's word is now, if it is still there as the menu found it.
    fn menu_word_now(&self) -> Option<(Range<usize>, String, Vec<String>)> {
        let menu = self.spelling.menu_word.borrow_mut().take()?;
        let document = self.document.borrow();
        let range = match document.follow(menu.revision, menu.range) {
            Followed::Kept(range) | Followed::Bordered(range) => range,
            Followed::Lost => return None,
        };
        (document.text().get(range.clone()) == Some(menu.word.as_str())).then_some((
            range,
            menu.word,
            menu.suggestions,
        ))
    }

    /// Replace the menu's word with its correction at `index`.
    pub(super) fn correct_spelling(&self, index: i32) {
        let Some((range, _, suggestions)) = self.menu_word_now() else {
            return;
        };
        let Some(correction) = usize::try_from(index)
            .ok()
            .and_then(|index| suggestions.get(index))
        else {
            return;
        };
        self.edit(|document| document.replace_word(range, correction));
    }

    /// Accept the menu's word in this message.
    pub(super) fn ignore_spelling(&self) {
        let Some((_, word, _)) = self.menu_word_now() else {
            return;
        };
        self.spelling.ignored.borrow_mut().push(word.clone());
        self.accept_word(&word);
    }

    /// Accept the menu's word everywhere from now on.
    pub(super) fn learn_spelling(&self) {
        let Some((_, word, _)) = self.menu_word_now() else {
            return;
        };
        let Some(spelling) = crate::spelling::service() else {
            return;
        };
        spelling.add_word(word.clone());
        with_composers(|composers| {
            for session in composers.sessions.borrow().iter() {
                session.accept_word(&word);
            }
        });
    }

    fn accept_word(&self, word: &str) {
        let Some(host) = self.host() else {
            return;
        };
        let view = host.view();
        if self.disposed.get() || !view.ui.get_open() {
            return;
        }
        let document = self.document.borrow();
        if let Some((revision, found)) = &mut *self.spelling.found.borrow_mut() {
            found.retain(|range| match document.follow(*revision, range.clone()) {
                Followed::Kept(now) | Followed::Bordered(now) => {
                    document.text().get(now) != Some(word)
                }
                Followed::Lost => true,
            });
        }
        if view.suspended {
            return;
        }
        let mut editor = self.editor.borrow_mut();
        editor.accept_word(&document, word);
        apply_compose_editor_surface(&view, &document, document.selection(), &mut editor);
    }

    /// Keep spelling settings the menu changed.
    pub(super) fn save_spelling(&self, enabled: bool, languages: Option<Vec<String>>) {
        let Some(composers) = self.composers() else {
            return;
        };
        let Some(app) = composers.app.upgrade() else {
            return;
        };
        crate::settings_controller::save_spelling(
            &app,
            &composers.state,
            &composers.runtime,
            enabled,
            languages,
        );
    }

    /// The languages after the menu turns the one at `index` on or off.
    pub(super) fn toggled_spelling_language(&self, index: i32) -> Option<Vec<String>> {
        let id = usize::try_from(index)
            .ok()
            .and_then(|index| self.spelling.menu_languages.borrow().get(index).cloned())?;
        Some(crate::spelling::service()?.toggled(&id))
    }
}

fn spelling_enabled() -> bool {
    crate::spelling::service().is_some_and(|spelling| spelling.enabled())
}
