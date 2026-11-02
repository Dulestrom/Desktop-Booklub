//! Thumbnails for the tiles. Decoding happens on worker threads, and only for tiles near the
//! viewport. M0 measured 31,496 decodes across a 2,000-book scroll without that rule, and
//! 3,678 with it, which is what took the grid from 47 to 60 frames per second.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::rc::Rc;

use booklub_core::background;
use gtk::prelude::*;
use gtk::{gdk, glib};

const WORKERS: usize = 4;
const CACHE_MAX: usize = 300;
const NO_FILE: &str = "";

struct Job {
    file_name: String,
}

/// Remembers which file a recycled picture currently wants.
fn wants(picture: &gtk::Picture, file_name: &str) -> bool {
    picture.widget_name() == file_name
}

pub struct Thumbs {
    cache: RefCell<HashMap<String, gdk::Texture>>,
    order: RefCell<VecDeque<String>>,
    pending: RefCell<HashMap<String, Vec<gtk::Picture>>>,
    waiting: RefCell<HashMap<String, Vec<gtk::Picture>>>,
    jobs: std::sync::mpsc::Sender<Job>,
    decoded: Cell<u64>,
}

impl Thumbs {
    pub fn new(backgrounds: PathBuf, thumbnails: PathBuf) -> Rc<Self> {
        let (job_tx, job_rx) = std::sync::mpsc::channel::<Job>();
        let job_rx = std::sync::Arc::new(std::sync::Mutex::new(job_rx));
        let (done_tx, done_rx) = async_pair();
        for _ in 0..WORKERS {
            let job_rx = job_rx.clone();
            let done_tx = done_tx.clone();
            let (backgrounds, thumbnails) = (backgrounds.clone(), thumbnails.clone());
            std::thread::spawn(move || loop {
                let job = {
                    let receiver = job_rx.lock().expect("no worker panics while holding this");
                    receiver.recv()
                };
                let Ok(job) = job else { break };
                let texture = load(&job.file_name, &backgrounds, &thumbnails);
                if done_tx.send((job.file_name, texture)).is_err() {
                    break;
                }
            });
        }

        let this = Rc::new(Self {
            cache: RefCell::default(),
            order: RefCell::default(),
            pending: RefCell::default(),
            waiting: RefCell::default(),
            jobs: job_tx,
            decoded: Cell::new(0),
        });
        let weak = Rc::downgrade(&this);
        // A short poll keeps worker results on the UI thread with no extra channel crate.
        glib::timeout_add_local(std::time::Duration::from_millis(8), move || {
            let Some(this) = weak.upgrade() else { return glib::ControlFlow::Break };
            while let Ok((file_name, texture)) = done_rx.try_recv() {
                this.finish(file_name, texture);
            }
            glib::ControlFlow::Continue
        });
        this
    }

    /// Sets a picture from the cache, or queues it for `pump` to request when it nears the view.
    pub fn request(&self, file_name: &str, picture: &gtk::Picture) {
        picture.set_widget_name(file_name);
        if let Some(texture) = self.cache.borrow().get(file_name) {
            picture.set_paintable(Some(texture));
            return;
        }
        picture.set_paintable(gdk::Paintable::NONE);
        if let Some(list) = self.waiting.borrow_mut().get_mut(file_name) {
            list.push(picture.clone());
            return;
        }
        self.pending.borrow_mut().entry(file_name.to_string()).or_default().push(picture.clone());
    }

    /// For tiles outside the recycled grid, which are few and always wanted.
    pub fn request_now(&self, file_name: &str, picture: &gtk::Picture) {
        picture.set_widget_name(file_name);
        if let Some(texture) = self.cache.borrow().get(file_name) {
            picture.set_paintable(Some(texture));
            return;
        }
        picture.set_paintable(gdk::Paintable::NONE);
        let mut waiting = self.waiting.borrow_mut();
        let first = !waiting.contains_key(file_name);
        waiting.entry(file_name.to_string()).or_default().push(picture.clone());
        if first {
            let _ = self.jobs.send(Job { file_name: file_name.to_string() });
        }
    }

    pub fn release(&self, picture: &gtk::Picture) {
        picture.set_widget_name(NO_FILE);
        picture.set_paintable(gdk::Paintable::NONE);
    }

    /// Runs each frame. Sends decode jobs only for tiles within one screen of the viewport.
    pub fn pump(&self, viewport: &impl IsA<gtk::Widget>) {
        let height = viewport.height() as f32;
        let mut promote = Vec::new();
        self.pending.borrow_mut().retain(|file_name, pictures| {
            pictures.retain(|picture| wants(picture, file_name));
            let near = pictures.iter().any(|picture| {
                picture.compute_bounds(viewport).is_some_and(|bounds| bounds.y() + bounds.height() > -height && bounds.y() < height * 2.0)
            });
            if near {
                promote.push((file_name.clone(), std::mem::take(pictures)));
            }
            !near && !pictures.is_empty()
        });
        for (file_name, pictures) in promote {
            self.waiting.borrow_mut().insert(file_name.clone(), pictures);
            let _ = self.jobs.send(Job { file_name });
        }
    }

    #[cfg(feature = "dev-tools")]
    pub fn in_flight(&self) -> usize {
        self.waiting.borrow().len()
    }

    /// A background was replaced or removed, so its texture must not be reused.
    pub fn forget(&self, file_name: &str) {
        self.cache.borrow_mut().remove(file_name);
        self.order.borrow_mut().retain(|name| name != file_name);
    }

    fn finish(&self, file_name: String, texture: Option<gdk::Texture>) {
        self.decoded.set(self.decoded.get() + 1);
        let pictures = self.waiting.borrow_mut().remove(&file_name).unwrap_or_default();
        let Some(texture) = texture else { return };
        for picture in pictures {
            // A recycled tile may have moved on to another book.
            if wants(&picture, &file_name) {
                picture.set_paintable(Some(&texture));
            }
        }
        let mut order = self.order.borrow_mut();
        let mut cache = self.cache.borrow_mut();
        cache.insert(file_name.clone(), texture);
        order.push_back(file_name);
        while order.len() > CACHE_MAX {
            if let Some(oldest) = order.pop_front() {
                cache.remove(&oldest);
            }
        }
    }
}

type Done = (String, Option<gdk::Texture>);

fn async_pair() -> (std::sync::mpsc::Sender<Done>, std::sync::mpsc::Receiver<Done>) {
    std::sync::mpsc::channel()
}

/// A key is a book's file name, `wide:<file>` for a Category's upload, or `preset:<id>`.
fn load(key: &str, backgrounds: &std::path::Path, thumbnails: &std::path::Path) -> Option<gdk::Texture> {
    let (bytes, width, height) = if let Some(id) = key.strip_prefix("preset:") {
        let path = format!("/io/github/dulestrom/Booklub/category-art/{id}.jpg");
        let data = gtk::gio::resources_lookup_data(&path, gtk::gio::ResourceLookupFlags::NONE).ok()?;
        (data.to_vec(), background::WIDE_WIDTH, background::WIDE_WIDTH)
    } else if let Some(file) = key.strip_prefix("wide:") {
        let path = background::ensure_wide_thumbnail(file, backgrounds, thumbnails).ok()?;
        (std::fs::read(path).ok()?, background::WIDE_WIDTH, background::WIDE_HEIGHT)
    } else {
        let path = background::ensure_thumbnail(key, backgrounds, thumbnails).ok()?;
        (std::fs::read(path).ok()?, background::THUMB_WIDTH, background::THUMB_HEIGHT)
    };
    let image = background::decode_for_display(&bytes, width, height).ok()?;
    Some(crate::util::texture_from_rgba(&image))
}
