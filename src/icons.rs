use crate::item::{ATTR_READONLY, ATTR_REPARSE_POINT, ATTR_SYSTEM, Item};
use crate::query::extension_of;
use slint::{Image, Rgba8Pixel, SharedPixelBuffer, Timer, TimerMode};
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet, VecDeque};
use std::mem::size_of;
use std::path::{Path, PathBuf};
use std::rc::{Rc, Weak};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::Duration;
use windows::Win32::Foundation::SIZE;
use windows::Win32::Graphics::Gdi::{
    BI_RGB, BITMAP, BITMAPINFO, BITMAPINFOHEADER, CreateCompatibleDC, DIB_RGB_COLORS, DeleteDC, DeleteObject,
    GetDIBits, GetObjectW, HBITMAP,
};
use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoInitializeEx};
use windows::Win32::UI::Shell::{
    IShellItemImageFactory, SHCreateItemFromParsingName, SIIGBF, SIIGBF_ICONONLY, SIIGBF_THUMBNAILONLY,
};
use windows::core::HSTRING;

pub const THIS_PC: &str = "::{20D04FE0-3AEA-1069-A2D8-08002B30309D}";

const MAX_LOADER_THREADS: usize = 6;
const MAX_QUEUED: usize = 192;
const CACHE_BYTES: usize = 256 << 20;
const POLL_INTERVAL: Duration = Duration::from_millis(24);
const ATTR_OFFLINE: u32 = 0x1000;
const ATTR_RECALL_ON_OPEN: u32 = 0x4_0000;
const ATTR_RECALL_ON_DATA_ACCESS: u32 = 0x40_0000;
const NOT_DOWNLOADED: u32 = ATTR_OFFLINE | ATTR_RECALL_ON_OPEN | ATTR_RECALL_ON_DATA_ACCESS;
const CUSTOMIZABLE_FOLDER: u32 = ATTR_READONLY | ATTR_SYSTEM | ATTR_REPARSE_POINT;

const THUMBNAIL_EXTENSIONS: [&str; 30] = [
    "jpg", "jpeg", "jpe", "jfif", "png", "gif", "bmp", "dib", "tif", "tiff", "webp", "heic", "heif", "avif", "cr2",
    "nef", "arw", "dng", "orf", "rw2", "mp4", "m4v", "mkv", "mov", "avi", "wmv", "webm", "mpg", "mpeg", "pdf",
];
const OWN_ICON_EXTENSIONS: [&str; 9] = ["exe", "ico", "lnk", "url", "scr", "cur", "ani", "msc", "appref-ms"];

type Pixels = SharedPixelBuffer<Rgba8Pixel>;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum Source {
    Folder,
    Extension(String),
    Path(PathBuf, u64),
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct IconKey {
    source: Source,
    size: u32,
    thumbnail: bool,
}

fn wants_thumbnail(extension: &str, attrs: u32, thumbnails: bool) -> bool {
    thumbnails && attrs & NOT_DOWNLOADED == 0 && THUMBNAIL_EXTENSIONS.contains(&extension)
}

fn item_source(item: &Item, thumbnail: bool) -> Source {
    let own_path = || Source::Path(item.path(), item.times.modified);
    if item.is_dir {
        return if item.attrs & CUSTOMIZABLE_FOLDER != 0 { own_path() } else { Source::Folder };
    }
    let extension = extension_of(&item.name);
    if thumbnail || OWN_ICON_EXTENSIONS.contains(&extension.as_str()) {
        own_path()
    } else {
        Source::Extension(extension)
    }
}

fn generic_source(item: &Item) -> Source {
    if item.is_dir { Source::Folder } else { Source::Extension(extension_of(&item.name)) }
}

fn to_rgba(pixels: &mut [u8]) {
    let no_alpha = pixels.chunks_exact(4).all(|pixel| pixel[3] == 0);
    for pixel in pixels.chunks_exact_mut(4) {
        pixel.swap(0, 2);
        if no_alpha {
            pixel[3] = u8::MAX;
        }
    }
}

unsafe fn read_bitmap(bitmap: HBITMAP) -> Option<Pixels> {
    unsafe {
        let mut info = BITMAP::default();
        let size = size_of::<BITMAP>() as i32;
        if GetObjectW(bitmap.into(), size, Some((&raw mut info).cast())) == 0 {
            return None;
        }
        let (width, height) = (info.bmWidth.unsigned_abs(), info.bmHeight.unsigned_abs());
        let mut header = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width as i32,
                biHeight: -(height as i32),
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut pixels = Pixels::new(width, height);
        let target = pixels.make_mut_bytes();
        let context = CreateCompatibleDC(None);
        let copied =
            GetDIBits(context, bitmap, 0, height, Some(target.as_mut_ptr().cast()), &mut header, DIB_RGB_COLORS);
        let _ = DeleteDC(context);
        if copied == 0 {
            return None;
        }
        to_rgba(target);
        Some(pixels)
    }
}

fn shell_image(path: &str, size: u32, flags: SIIGBF) -> Option<Pixels> {
    unsafe {
        let factory: IShellItemImageFactory = SHCreateItemFromParsingName(&HSTRING::from(path), None).ok()?;
        let wanted = SIZE { cx: size as i32, cy: size as i32 };
        let bitmap = factory.GetImage(wanted, flags).ok()?;
        let pixels = read_bitmap(bitmap);
        let _ = DeleteObject(bitmap.into());
        pixels
    }
}

struct Request {
    key: IconKey,
    path: String,
    pixels: u32,
}

impl Request {
    fn load(&self) -> Option<Pixels> {
        let thumbnail = self.key.thumbnail.then(|| shell_image(&self.path, self.pixels, SIIGBF_THUMBNAILONLY));
        thumbnail.flatten().or_else(|| shell_image(&self.path, self.pixels, SIIGBF_ICONONLY))
    }
}

#[derive(Default)]
struct Queue {
    requests: Mutex<VecDeque<Request>>,
    wake: Condvar,
}

fn load_requests(queue: Arc<Queue>, results: Sender<(IconKey, Option<Pixels>)>) {
    let _ = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) };
    loop {
        let request = {
            let mut requests = queue.requests.lock().unwrap();
            loop {
                match requests.pop_back() {
                    Some(request) => break request,
                    None => requests = queue.wake.wait(requests).unwrap(),
                }
            }
        };
        let pixels = request.load();
        if results.send((request.key, pixels)).is_err() {
            return;
        }
    }
}

struct Entry {
    image: Image,
    bytes: usize,
    last_used: u64,
}

pub struct IconStore {
    me: Weak<IconStore>,
    scale: Cell<f32>,
    entries: RefCell<HashMap<IconKey, Entry>>,
    cached_bytes: Cell<usize>,
    clock: Cell<u64>,
    pending: RefCell<HashSet<IconKey>>,
    queue: Arc<Queue>,
    results: Receiver<(IconKey, Option<Pixels>)>,
    timer: Timer,
    listener: RefCell<Option<Box<dyn Fn()>>>,
}

impl IconStore {
    pub fn new() -> Rc<Self> {
        let queue = Arc::new(Queue::default());
        let (sender, results) = mpsc::channel();
        let threads = thread::available_parallelism().map_or(2, |count| (count.get() / 2).clamp(2, MAX_LOADER_THREADS));
        for _ in 0..threads {
            let (queue, sender) = (queue.clone(), sender.clone());
            thread::spawn(move || load_requests(queue, sender));
        }
        Rc::new_cyclic(|me| Self {
            me: me.clone(),
            scale: Cell::new(1.0),
            entries: RefCell::default(),
            cached_bytes: Cell::new(0),
            clock: Cell::new(0),
            pending: RefCell::default(),
            queue,
            results,
            timer: Timer::default(),
            listener: RefCell::default(),
        })
    }

    pub fn set_scale(&self, scale: f32) {
        self.scale.set(scale);
    }

    pub fn set_listener(&self, listener: impl Fn() + 'static) {
        *self.listener.borrow_mut() = Some(Box::new(listener));
    }

    pub fn cancel_queued(&self) {
        let dropped: Vec<Request> = self.queue.requests.lock().unwrap().drain(..).collect();
        let mut pending = self.pending.borrow_mut();
        for request in dropped {
            pending.remove(&request.key);
        }
    }

    pub fn item_icon(&self, item: &Item, size: u32, thumbnails: bool) -> (Image, bool) {
        let thumbnail = !item.is_dir && wants_thumbnail(&extension_of(&item.name), item.attrs, thumbnails);
        let key = IconKey { source: item_source(item, thumbnail), size, thumbnail };
        if let Some(image) = self.lookup(&key) {
            return (image, true);
        }
        let path = item.path().to_string_lossy().to_string();
        let generic = IconKey { source: generic_source(item), size, thumbnail: false };
        let placeholder = self.lookup(&generic);
        if placeholder.is_none() && generic != key {
            self.request(generic, path.clone());
        }
        self.request(key, path);
        (placeholder.unwrap_or_default(), false)
    }

    pub fn folder_icon(&self, path: &Path, attrs: u32, size: u32) -> Option<Image> {
        let source = if attrs & CUSTOMIZABLE_FOLDER != 0 { Source::Path(path.to_path_buf(), 0) } else { Source::Folder };
        self.shell_icon(source, &path.to_string_lossy(), size)
    }

    pub fn location_icon(&self, parsing_name: &str, size: u32) -> Option<Image> {
        self.shell_icon(Source::Path(parsing_name.into(), 0), parsing_name, size)
    }

    fn shell_icon(&self, source: Source, path: &str, size: u32) -> Option<Image> {
        let key = IconKey { source, size, thumbnail: false };
        let image = self.lookup(&key);
        if image.is_none() {
            self.request(key, path.to_string());
        }
        image
    }

    fn lookup(&self, key: &IconKey) -> Option<Image> {
        let now = self.clock.get() + 1;
        self.clock.set(now);
        let mut entries = self.entries.borrow_mut();
        let entry = entries.get_mut(key)?;
        entry.last_used = now;
        Some(entry.image.clone())
    }

    fn request(&self, key: IconKey, path: String) {
        if !self.pending.borrow_mut().insert(key.clone()) {
            return;
        }
        let pixels = (key.size as f32 * self.scale.get()).ceil() as u32;
        let dropped = {
            let mut requests = self.queue.requests.lock().unwrap();
            requests.push_back(Request { key, path, pixels });
            (requests.len() > MAX_QUEUED).then(|| requests.pop_front()).flatten()
        };
        if let Some(request) = dropped {
            self.pending.borrow_mut().remove(&request.key);
        }
        self.queue.wake.notify_one();
        if !self.timer.running() {
            let me = self.me.clone();
            self.timer.start(TimerMode::Repeated, POLL_INTERVAL, move || {
                if let Some(store) = me.upgrade() {
                    store.collect();
                }
            });
        }
    }

    fn collect(&self) {
        let mut changed = false;
        while let Ok((key, pixels)) = self.results.try_recv() {
            self.pending.borrow_mut().remove(&key);
            self.insert(key, pixels);
            changed = true;
        }
        if self.pending.borrow().is_empty() {
            self.timer.stop();
        }
        if changed && let Some(listener) = &*self.listener.borrow() {
            listener();
        }
    }

    fn insert(&self, key: IconKey, pixels: Option<Pixels>) {
        let bytes = pixels.as_ref().map_or(0, |pixels| pixels.as_bytes().len());
        let image = pixels.map_or_else(Image::default, Image::from_rgba8_premultiplied);
        let entry = Entry { image, bytes, last_used: self.clock.get() };
        let replaced = self.entries.borrow_mut().insert(key, entry);
        let total = self.cached_bytes.get() + bytes - replaced.map_or(0, |entry| entry.bytes);
        self.cached_bytes.set(total);
        if total > CACHE_BYTES {
            self.evict();
        }
    }

    fn evict(&self) {
        let mut entries = self.entries.borrow_mut();
        let mut by_age: Vec<(u64, IconKey)> =
            entries.iter().map(|(key, entry)| (entry.last_used, key.clone())).collect();
        by_age.sort_unstable_by_key(|(last_used, _)| *last_used);
        let mut total = self.cached_bytes.get();
        for (_, key) in by_age {
            if total <= CACHE_BYTES / 2 {
                break;
            }
            if let Some(entry) = entries.remove(&key) {
                total -= entry.bytes;
            }
        }
        self.cached_bytes.set(total);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::item::{ATTR_DIRECTORY, ItemState};
    use crate::scan::Times;

    fn item(name: &str, is_dir: bool, attrs: u32) -> Item {
        Item {
            name: name.into(),
            parent: Arc::from(r"C:\data"),
            is_dir,
            attrs,
            times: Times::default(),
            size: 0,
            file_count: 0,
            dir_count: 0,
            state: ItemState::Ready,
            hashes: None,
        }
    }

    fn own_path(item: &Item) -> Source {
        Source::Path(item.path(), item.times.modified)
    }

    #[test]
    fn plain_folders_share_one_icon() {
        assert_eq!(item_source(&item("music", true, ATTR_DIRECTORY), false), Source::Folder);
    }

    #[test]
    fn folders_that_may_be_customized_get_their_own_icon() {
        for attrs in [ATTR_READONLY, ATTR_SYSTEM, ATTR_REPARSE_POINT] {
            let folder = item("Documents", true, ATTR_DIRECTORY | attrs);
            assert_eq!(item_source(&folder, false), own_path(&folder));
        }
    }

    #[test]
    fn files_share_an_icon_per_extension() {
        assert_eq!(item_source(&item("a.TXT", false, 0), false), Source::Extension("txt".into()));
        assert_eq!(item_source(&item("README", false, 0), false), Source::Extension(String::new()));
        assert_eq!(item_source(&item(".gitignore", false, 0), false), Source::Extension(String::new()));
    }

    #[test]
    fn programs_and_shortcuts_keep_their_own_icon() {
        for name in ["setup.exe", "App.LNK", "favicon.ico"] {
            let file = item(name, false, 0);
            assert_eq!(item_source(&file, false), own_path(&file));
        }
    }

    #[test]
    fn thumbnails_are_for_images_in_views_that_show_them() {
        assert!(wants_thumbnail("jpg", 0, true));
        assert!(!wants_thumbnail("jpg", 0, false));
        assert!(!wants_thumbnail("txt", 0, true));
    }

    #[test]
    fn cloud_placeholders_are_not_downloaded_for_a_thumbnail() {
        assert!(!wants_thumbnail("png", ATTR_RECALL_ON_DATA_ACCESS, true));
        assert!(!wants_thumbnail("png", ATTR_OFFLINE, true));
    }

    #[test]
    fn bitmaps_become_rgba() {
        let mut pixels = [10, 20, 30, 40];
        to_rgba(&mut pixels);
        assert_eq!(pixels, [30, 20, 10, 40]);
    }

    #[test]
    fn bitmaps_without_any_alpha_become_opaque() {
        let mut pixels = [1, 2, 3, 0, 4, 5, 6, 0];
        to_rgba(&mut pixels);
        assert_eq!(pixels, [3, 2, 1, 255, 6, 5, 4, 255]);
    }

    #[test]
    fn transparency_is_kept_when_any_pixel_uses_it() {
        let mut pixels = [1, 2, 3, 0, 4, 5, 6, 255];
        to_rgba(&mut pixels);
        assert_eq!(pixels, [3, 2, 1, 0, 6, 5, 4, 255]);
    }

    #[test]
    fn the_shell_returns_a_real_folder_icon() {
        let _ = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
        let icon = shell_image(r"C:\Windows", 32, SIIGBF_ICONONLY).unwrap();
        assert_eq!((icon.width(), icon.height()), (32, 32));
        assert!(icon.as_bytes().chunks_exact(4).any(|pixel| pixel[3] != 0));
    }

    #[test]
    fn missing_files_have_no_image() {
        let _ = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
        assert!(shell_image(r"C:\this\does\not\exist.png", 32, SIIGBF_ICONONLY).is_none());
    }
}
