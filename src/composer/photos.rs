//! Photos sent shrunk, when the person writing asks for it: the JPEG photos larger than
//! [`photo_shrink::SHRUNK_SIDE`], in the text and attached. The message keeps the originals
//! while it's open, so they come back when shrinking is turned off again.

use super::*;
use crate::photo_shrink;
use crate::rich_compose::InlineImage;
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::Path;

/// The photos of one message, as they go out.
#[derive(Default)]
pub(super) struct MessagePhotos {
    /// The message's photos go out shrunk.
    on: Cell<bool>,
    /// Photos in the text, shrunk, by image, or `None` for those that stay as they are.
    images: RefCell<HashMap<u64, Option<Arc<[u8]>>>>,
    /// Attached photos that stay as they are.
    kept: RefCell<HashSet<PathBuf>>,
    /// Photos waiting to be shrunk, one at a time, and the one being shrunk.
    queue: RefCell<VecDeque<Photo>>,
    running: RefCell<Option<Photo>>,
    /// Photos that are attached once they're shrunk.
    waiting: RefCell<Vec<ComposeFile>>,
    /// Of those, the ones attached and the ones that didn't fit, since the last notice.
    attached: Cell<usize>,
    skipped: Cell<usize>,
    /// Tests shrink the photos waiting themselves.
    #[cfg(test)]
    held: Cell<bool>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Photo {
    /// A photo in the text, by its image.
    Image(u64),
    /// An attached photo, or one waiting to be, by its file.
    File(PathBuf),
}

/// What the message's photo row shows.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct PhotoSummary {
    pub(super) on: bool,
    pub(super) count: usize,
    /// The photos' size as they go out, and at full size.
    pub(super) size: u64,
    pub(super) full_size: u64,
    pub(super) working: bool,
}

impl MessagePhotos {
    pub(super) fn on(&self) -> bool {
        self.on.get()
    }

    /// Photos are being shrunk, or wait to be.
    pub(super) fn busy(&self) -> bool {
        self.running.borrow().is_some() || !self.queue.borrow().is_empty()
    }

    /// The file a photo in the text goes out as, when it goes out shrunk.
    pub(super) fn shrunk(&self, image: &InlineImage) -> Option<Arc<[u8]>> {
        if !self.on.get() {
            return None;
        }
        self.images.borrow().get(&image.id).cloned().flatten()
    }

    /// The size the images in `document` go out at.
    pub(super) fn image_bytes(&self, document: &RichComposeDocument) -> u64 {
        document
            .images()
            .map(|image| {
                self.shrunk(image)
                    .map_or(image.data.len(), |data| data.len()) as u64
            })
            .sum()
    }

    /// Whether `path` waits to be attached once it's shrunk.
    pub(super) fn waiting_for(&self, path: &Path) -> bool {
        self.waiting.borrow().iter().any(|file| file.path == path)
    }

    /// Attach `files`, photos, once they're shrunk.
    pub(super) fn attach_when_shrunk(&self, files: Vec<ComposeFile>) {
        let mut queue = self.queue.borrow_mut();
        let mut waiting = self.waiting.borrow_mut();
        for file in files {
            queue.push_back(Photo::File(file.path.clone()));
            waiting.push(file);
        }
    }

    /// Forget the message's photos, which removes their shrunk copies.
    pub(super) fn clear(&self) {
        self.on.set(false);
        self.images.borrow_mut().clear();
        self.kept.borrow_mut().clear();
        self.queue.borrow_mut().clear();
        *self.running.borrow_mut() = None;
        self.waiting.borrow_mut().clear();
        self.attached.set(0);
        self.skipped.set(0);
    }

    fn queued(&self, photo: &Photo) -> bool {
        self.running.borrow().as_ref() == Some(photo) || self.queue.borrow().contains(photo)
    }
}

impl ComposeFile {
    /// Whether this is the file at `path`, or a shrunk copy of it.
    pub(super) fn holds(&self, path: &Path) -> bool {
        self.path == path
            || self
                .original
                .as_ref()
                .is_some_and(|original| original.path == path)
    }

    /// This photo, shrunk to `size` bytes in the file at `copy`. It keeps this one, to come
    /// back when photos no longer go out shrunk.
    fn shrunk(self, copy: tempfile::TempPath, size: u64) -> ComposeFile {
        let path = copy.to_path_buf();
        ComposeFile {
            path: path.clone(),
            filename: self.filename.clone(),
            size,
            // The copy is removed along with the attachment.
            _import: Some(Rc::new(documents::ImportedDocument::temporary(
                path,
                move |_| drop(copy),
            ))),
            shrinkable: false,
            original: Some(Box::new(self)),
        }
    }
}

/// A photo to shrink, off the UI thread.
enum Shrinking {
    Image(Arc<[u8]>),
    File(PathBuf),
}

/// A photo shrunk, or `None` when it stays as it is.
enum Shrunk {
    Image(Option<Arc<[u8]>>),
    File(Option<(tempfile::TempPath, u64)>),
}

impl Shrinking {
    fn run(self, temp_dir: &Path) -> Shrunk {
        match self {
            Self::Image(data) => Shrunk::Image(photo_shrink::shrink(&data).map(Arc::from)),
            Self::File(path) => Shrunk::File(photo_shrink::shrink_file(&path, temp_dir)),
        }
    }
}

impl ComposeSession {
    /// Show the message's photos, and shrink those that should be and aren't yet.
    pub(super) fn refresh_photos(&self) {
        if self.photos.on() {
            self.queue_photos();
        }
        if let Some(host) = self.host() {
            apply_photos(&host.ui(), &self.photo_summary());
        }
        self.shrink_next_photo();
    }

    /// Have the message's photos go out shrunk or at full size, from its switch.
    pub(super) fn set_shrink_photos(&self, on: bool) {
        let Some(host) = self.host() else {
            return;
        };
        let ui = host.ui();
        if on == self.photos.on() {
            return;
        }
        if !on {
            // The originals come back while they fit.
            let files = self
                .files
                .borrow()
                .iter()
                .map(|file| file.original.as_deref().unwrap_or(file).size)
                .sum::<u64>();
            let images = self
                .document
                .borrow()
                .images()
                .map(|image| image.data.len() as u64)
                .sum::<u64>();
            if files.saturating_add(images) > MAX_COMPOSE_ATTACHMENT_BYTES {
                ui.set_shrink_photos(true);
                ui.set_notice(
                    UiMessage::plain("At full size, the photos would take the message over 25 MB."),
                    true,
                );
                return;
            }
        }
        self.photos.on.set(on);
        if !on {
            self.photos.queue.borrow_mut().clear();
            for file in self.files.borrow_mut().iter_mut() {
                if let Some(original) = file.original.take() {
                    *file = *original;
                }
            }
            // Photos waiting to go in shrunk go in as they are.
            let waiting = std::mem::take(&mut *self.photos.waiting.borrow_mut());
            for file in waiting {
                self.attach_photo(file);
            }
            apply_compose_files(&ui, &self.files.borrow());
            self.report_attached_photos();
        }
        self.refresh_photos();
    }

    fn photo_summary(&self) -> PhotoSummary {
        // A signature's pictures are small, and it has no files.
        if self.signature_target.borrow().is_some() {
            return PhotoSummary::default();
        }
        let mut summary = PhotoSummary {
            on: self.photos.on(),
            working: self.photos.on() && self.photos.busy(),
            ..PhotoSummary::default()
        };
        for image in self.document.borrow().images() {
            if photo_shrink::can_shrink(image.mime_type, image.width, image.height) {
                summary.count += 1;
                summary.full_size += image.data.len() as u64;
                summary.size +=
                    self.photos
                        .shrunk(image)
                        .map_or(image.data.len(), |data| data.len()) as u64;
            }
        }
        for file in self.files.borrow().iter() {
            let original = file.original.as_deref().unwrap_or(file);
            if original.shrinkable {
                summary.count += 1;
                summary.full_size += original.size;
                summary.size += file.size;
            }
        }
        summary
    }

    /// Line up the message's photos that aren't shrunk yet.
    fn queue_photos(&self) {
        if self.signature_target.borrow().is_some() {
            return;
        }
        let mut photos = Vec::new();
        {
            let shrunk = self.photos.images.borrow();
            for image in self.document.borrow().images() {
                if photo_shrink::can_shrink(image.mime_type, image.width, image.height)
                    && !shrunk.contains_key(&image.id)
                {
                    photos.push(Photo::Image(image.id));
                }
            }
            let kept = self.photos.kept.borrow();
            for file in self.files.borrow().iter() {
                if file.shrinkable && !kept.contains(&file.path) {
                    photos.push(Photo::File(file.path.clone()));
                }
            }
        }
        for photo in photos {
            if !self.photos.queued(&photo) {
                self.photos.queue.borrow_mut().push_back(photo);
            }
        }
    }

    /// The next photo waiting that's still in the message, and what shrinking it takes.
    fn next_photo(&self) -> Option<(Photo, Shrinking)> {
        loop {
            let photo = self.photos.queue.borrow_mut().pop_front()?;
            let shrinking = match &photo {
                Photo::Image(id) => self
                    .document
                    .borrow()
                    .images()
                    .find(|image| image.id == *id)
                    .map(|image| Shrinking::Image(Arc::clone(&image.data))),
                Photo::File(path) => (self.photos.waiting_for(path)
                    || self.files.borrow().iter().any(|file| file.path == *path))
                .then(|| Shrinking::File(path.clone())),
            };
            if let Some(shrinking) = shrinking {
                return Some((photo, shrinking));
            }
        }
    }

    /// Shrink the next photo waiting, off the UI thread, unless one is being shrunk.
    fn shrink_next_photo(&self) {
        #[cfg(test)]
        if self.photos.held.get() {
            return;
        }
        if self.photos.running.borrow().is_some() {
            return;
        }
        let Some(composers) = self.composers() else {
            return;
        };
        let Some((photo, shrinking)) = self.next_photo() else {
            return;
        };
        *self.photos.running.borrow_mut() = Some(photo);
        let temp_dir = composers.temp_dir.clone();
        let started_in = self.generation.get();
        let weak = self.this.clone();
        let request: documents::DocumentFuture<Shrunk> = Box::pin(async move {
            tokio::task::spawn_blocking(move || shrinking.run(&temp_dir))
                .await
                .map_err(|error| error.to_string())
        });
        documents::run_request(&composers.runtime, request, move |shrunk| {
            let Some(session) = weak.upgrade() else {
                return;
            };
            if session.generation.get() != started_in {
                return;
            }
            let Some(photo) = session.photos.running.borrow_mut().take() else {
                return;
            };
            session.photo_shrunk(photo, shrunk.ok());
            session.refresh_photos();
        });
    }

    /// Put a shrunk photo in place of the original, or keep the original when it didn't shrink.
    fn photo_shrunk(&self, photo: Photo, shrunk: Option<Shrunk>) {
        match photo {
            Photo::Image(id) => {
                let data = match shrunk {
                    Some(Shrunk::Image(data)) => data,
                    _ => None,
                };
                self.photos.images.borrow_mut().insert(id, data);
            }
            Photo::File(path) => {
                let copy = match shrunk {
                    Some(Shrunk::File(copy)) => copy,
                    _ => None,
                };
                if copy.is_none() {
                    self.photos.kept.borrow_mut().insert(path.clone());
                }
                let waiting = {
                    let mut waiting = self.photos.waiting.borrow_mut();
                    waiting
                        .iter()
                        .position(|file| file.path == path)
                        .map(|index| waiting.remove(index))
                };
                let copy = copy.filter(|_| self.photos.on());
                if let Some(file) = waiting {
                    let file = match copy {
                        Some((copy, size)) => file.shrunk(copy, size),
                        None => file,
                    };
                    self.attach_photo(file);
                    if let Some(host) = self.host() {
                        apply_compose_files(&host.ui(), &self.files.borrow());
                    }
                    self.report_attached_photos();
                    return;
                }
                let Some((copy, size)) = copy else {
                    return;
                };
                let mut files = self.files.borrow_mut();
                let Some(index) = files
                    .iter()
                    .position(|file| file.path == path && file.original.is_none())
                else {
                    return;
                };
                let original = files.remove(index);
                files.insert(index, original.shrunk(copy, size));
                if let Some(host) = self.host() {
                    apply_compose_files(&host.ui(), &files);
                }
            }
        }
    }

    /// Attach `file`, a photo that waited to be shrunk, when it fits.
    fn attach_photo(&self, file: ComposeFile) {
        let mut files = self.files.borrow_mut();
        let total = files
            .iter()
            .map(|file| file.size)
            .sum::<u64>()
            .saturating_add(self.photos.image_bytes(&self.document.borrow()));
        if total.saturating_add(file.size) > MAX_COMPOSE_ATTACHMENT_BYTES {
            self.photos.skipped.set(self.photos.skipped.get() + 1);
            return;
        }
        files.push(file);
        self.photos.attached.set(self.photos.attached.get() + 1);
    }

    /// Say how attaching photos went, once none wait anymore.
    fn report_attached_photos(&self) {
        if !self.photos.waiting.borrow().is_empty() {
            return;
        }
        let added = self.photos.attached.replace(0);
        let skipped = self.photos.skipped.replace(0);
        if added + skipped == 0 {
            return;
        }
        if let Some(host) = self.host() {
            let (notice, error) = attachments_notice(added, skipped);
            host.ui().set_notice(notice, error);
        }
    }
}

/// Show `summary` in the message's photo row.
pub(super) fn apply_photos(ui: &ComposerUi<'_>, summary: &PhotoSummary) {
    ui.set_photo_count(i32::try_from(summary.count).unwrap_or(i32::MAX));
    ui.set_photo_size(format_file_size(summary.size).into());
    ui.set_photo_full_size(format_file_size(summary.full_size).into());
    ui.set_shrink_photos(summary.on);
    ui.set_shrinking_photos(summary.working);
}

#[cfg(all(test, desktop))]
mod tests {
    use super::super::tests::{Setup, setup};
    use super::*;

    /// A grainy JPEG photo `width` by `height`, as cameras take them.
    fn photo(width: u32, height: u32) -> Vec<u8> {
        let mut seed = 0x9e37_79b9_u32;
        let picture = image::RgbImage::from_fn(width, height, |x, y| {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            image::Rgb([(x % 251) as u8, (y % 241) as u8, (seed % 96) as u8])
        });
        let mut data = Vec::new();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut data, 95)
            .encode_image(&picture)
            .unwrap();
        data
    }

    fn attachment(path: &Path) -> NewAttachment {
        NewAttachment {
            path: path.to_owned(),
            filename: path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned()),
            import: None,
        }
    }

    impl ComposeSession {
        /// Shrink the photos waiting, one after another, as the runtime does.
        fn shrink_photos_now(&self, temp_dir: &Path) {
            while let Some((photo, shrinking)) = self.next_photo() {
                *self.photos.running.borrow_mut() = Some(photo.clone());
                let shrunk = shrinking.run(temp_dir);
                self.photos.running.borrow_mut().take();
                self.photo_shrunk(photo, Some(shrunk));
            }
            self.refresh_photos();
        }
    }

    #[test]
    fn photos_go_out_shrunk_and_come_back_at_full_size() {
        let Setup {
            app,
            composers,
            _runtime,
            _dir,
        } = setup();
        app.invoke_open_compose("".into());
        let session = composers.in_app().unwrap();
        session.photos.held.set(true);
        let host = session.host().unwrap();
        let ui = host.ui();
        let folder = tempfile::tempdir().unwrap();
        let path = |name: &str| std::fs::canonicalize(folder.path()).unwrap().join(name);
        std::fs::write(path("large.jpg"), photo(2000, 1000)).unwrap();
        std::fs::write(path("small.jpg"), photo(800, 600)).unwrap();
        let large_size = std::fs::metadata(path("large.jpg")).unwrap().len();

        // A large photo attached, a small one, and a large one in the text.
        session.attach(vec![
            attachment(&path("large.jpg")),
            attachment(&path("small.jpg")),
        ]);
        assert!(
            session.insert_image(InlineImage::from_file_bytes(photo(1800, 1200), "Ring").unwrap())
        );
        assert_eq!(ui.get_photo_count(), 2, "the small photo stays as it is");
        assert!(!ui.get_shrink_photos());
        let full_size = ui.get_photo_size();
        assert_eq!(ui.get_photo_full_size(), full_size);

        // Turned on, they're shrunk one after another, and the message carries them.
        session.set_shrink_photos(true);
        assert!(ui.get_shrinking_photos());
        session.shrink_photos_now(folder.path());
        assert!(!ui.get_shrinking_photos());
        assert_eq!(ui.get_photo_full_size(), full_size);
        assert_ne!(ui.get_photo_size(), full_size);
        {
            let files = session.files.borrow();
            assert_eq!(files.len(), 2);
            let shrunk = &files[0];
            assert!(shrunk.holds(&path("large.jpg")) && shrunk.path != path("large.jpg"));
            assert_eq!(shrunk.filename, "large.jpg");
            assert!(shrunk.size < large_size / 2, "{} bytes", shrunk.size);
            assert_eq!(std::fs::metadata(&shrunk.path).unwrap().len(), shrunk.size);
            assert_eq!(files[1].path, path("small.jpg"));
        }
        let document = session.document.borrow();
        let image = document.images().next().unwrap();
        let shrunk = session
            .photos
            .shrunk(image)
            .expect("the photo in the text shrinks");
        assert!(shrunk.len() < image.data.len() / 2);
        assert!(session.photos.image_bytes(&document) < image.data.len() as u64 / 2);
        drop(document);

        // Photos added while it's on go in shrunk.
        std::fs::write(path("later.jpg"), photo(1700, 1700)).unwrap();
        session.attach(vec![attachment(&path("later.jpg"))]);
        assert_eq!(session.files.borrow().len(), 2, "it waits to be shrunk");
        session.shrink_photos_now(folder.path());
        assert_eq!(session.files.borrow().len(), 3);
        assert!(session.files.borrow()[2].original.is_some());
        assert_eq!(ui.get_photo_count(), 3);

        // Turned off, the originals come back and the shrunk copies go.
        let copy = session.files.borrow()[0].path.clone();
        session.set_shrink_photos(false);
        assert!(!copy.exists());
        let files = session.files.borrow();
        assert_eq!(files[0].path, path("large.jpg"));
        assert_eq!(files[0].size, large_size);
        assert!(files.iter().all(|file| file.original.is_none()));
        drop(files);
        let document = session.document.borrow();
        assert!(
            session
                .photos
                .shrunk(document.images().next().unwrap())
                .is_none()
        );
        drop(document);
        assert_eq!(ui.get_photo_size(), ui.get_photo_full_size());
    }
}
