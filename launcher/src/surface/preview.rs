//! File previews: bounded whole-file reads, archive listings and PDF covers, loaded off the UI
//! thread. Payloads are data; the view decides how they look.

use std::collections::HashMap;
use std::ffi::OsStr;
use std::fs::File;
use std::io::{self, Read};
use std::os::fd::FromRawFd;
use std::os::unix::ffi::OsStrExt;
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::{Duration, Instant};

use super::model::{file_ext, image_mime, is_archive, language, model_fence};
use crate::sys::cstr;

pub(crate) const IMAGE_BYTES: u64 = 16 << 20;
pub(crate) const MARKDOWN_BYTES: u64 = 1 << 20;
pub(crate) const CODE_BYTES: u64 = 1 << 20;
pub(crate) const MERMAID_BYTES: u64 = 256 << 10;
const ARCHIVE_LINES: usize = 50;
const HELPER_TIMEOUT: Duration = Duration::from_secs(10);

/// A loaded preview. Image bytes travel separately so they can become a session blob.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Preview {
    /// The whole file exceeds `limit` or is not a readable regular file.
    Unavailable { limit: u64 },
    Archive(Vec<String>),
    Pdf { meta: HashMap<String, String>, cover: Option<Blob> },
    PdfUnavailable,
    Image { ext: String, mime: &'static str, size: usize, blob: Option<Blob> },
    Mermaid(String),
    Markdown(String),
    Model { fence: &'static str, text: String },
    Binary { size: usize },
    Code { text: String, lang: &'static str },
}

/// Image bytes before upload, or the session blob id after.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Blob {
    Bytes(Vec<u8>, &'static str),
    Id(String),
}

impl Preview {
    /// The metadata label shown above the preview.
    pub(crate) fn kind(&self, name: &str) -> String {
        match self {
            Self::Image { mime, .. } => (*mime).to_owned(),
            Self::Archive(_) => "archive".to_owned(),
            Self::Binary { .. } => "binary".to_owned(),
            Self::Markdown(_) => "markdown".to_owned(),
            Self::Mermaid(_) => "mermaid".to_owned(),
            Self::Pdf { .. } | Self::PdfUnavailable => "pdf".to_owned(),
            Self::Model { fence, .. } => format!("{fence} model"),
            Self::Code { lang, .. } => (*lang).to_owned(),
            Self::Unavailable { .. } => language(&file_ext(name)).to_owned(),
        }
    }
}

/// Reads a whole regular, non-symlink file of at most `limit` bytes, without blocking on FIFOs
/// or devices and without returning a truncated prefix when the file grows meanwhile.
pub(crate) fn read_bounded(path: &str, limit: u64) -> io::Result<Vec<u8>> {
    let name = cstr(OsStr::from_bytes(path.as_bytes()))?;
    let fd = unsafe {
        libc::open(
            name.as_ptr(),
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    let file = unsafe { File::from_raw_fd(fd) };
    let meta = file.metadata()?;
    if !meta.file_type().is_file() || meta.len() > limit {
        return Err(io::Error::other("not a regular file within the limit"));
    }
    let mut bytes = Vec::with_capacity(meta.len() as usize);
    file.take(limit + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        return Err(io::Error::other("file grew past the limit"));
    }
    Ok(bytes)
}

/// Runs a helper with a deadline; its stdout, if it succeeded within `cap` bytes.
fn helper(args: &[&str], cap: u64) -> Option<Vec<u8>> {
    let mut child = Command::new(args[0])
        .args(&args[1..])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let stdout = child.stdout.take()?;
    let reader = thread::spawn(move || {
        let mut bytes = Vec::new();
        stdout.take(cap + 1).read_to_end(&mut bytes).map(|_| bytes)
    });
    let deadline = Instant::now() + HELPER_TIMEOUT;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(20)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
        }
    };
    let bytes = reader.join().ok()?.ok()?;
    (status?.success() && bytes.len() as u64 <= cap).then_some(bytes)
}

fn archive(path: &str, ext: &str) -> Vec<String> {
    let args: &[&str] = match ext {
        "zip" => &["unzip", "-Z1", path],
        "7z" => &["7z", "l", "-ba", "-slt", path],
        _ => &["tar", "-tf", path],
    };
    let Some(bytes) = helper(args, 4 << 20) else {
        return Vec::new();
    };
    String::from_utf8_lossy(&bytes)
        .lines()
        .filter(|line| !line.is_empty())
        .take(ARCHIVE_LINES)
        .map(str::to_owned)
        .collect()
}

fn pdf(path: &str) -> Preview {
    // The same whole-file gate as text previews runs before any helper sees the file.
    if read_bounded(path, CODE_BYTES).is_err() {
        return Preview::Unavailable { limit: CODE_BYTES };
    }
    let meta = helper(&["pdfinfo", path], 1 << 20)
        .map(|bytes| {
            String::from_utf8_lossy(&bytes)
                .lines()
                .filter_map(|line| line.split_once(':'))
                .map(|(key, value)| (key.to_owned(), value.trim_start().to_owned()))
                .collect()
        })
        .unwrap_or_default();
    // Rasterize to stdout: no shared temp path to pre-seed, follow or race.
    let cover = helper(
        &["pdftocairo", "-f", "1", "-l", "1", "-singlefile", "-jpeg", "-r", "100", path, "-"],
        IMAGE_BYTES,
    )
    .filter(|bytes| !bytes.is_empty());
    match cover {
        Some(bytes) => Preview::Pdf {
            meta,
            cover: Some(Blob::Bytes(bytes, "image/jpeg")),
        },
        None => Preview::PdfUnavailable,
    }
}

/// Loads the preview of `path`; blocking, so it runs on a worker thread.
pub(crate) fn load(path: &str) -> Preview {
    let ext = file_ext(path);
    if is_archive(&ext) {
        return Preview::Archive(archive(path, &ext));
    }
    if ext == "pdf" {
        return pdf(path);
    }
    let mime = image_mime(&ext);
    let limit = match ext.as_str() {
        _ if mime.is_some() => IMAGE_BYTES,
        "mermaid" | "mmd" => MERMAID_BYTES,
        "md" | "markdown" => MARKDOWN_BYTES,
        _ => CODE_BYTES,
    };
    let Ok(bytes) = read_bounded(path, limit) else {
        return Preview::Unavailable { limit };
    };
    if let Some(mime) = mime {
        let size = bytes.len();
        let blob = (!bytes.is_empty()).then_some(Blob::Bytes(bytes, mime));
        return Preview::Image { ext, mime, size, blob };
    }
    let text = || String::from_utf8_lossy(&bytes).into_owned();
    match ext.as_str() {
        "mermaid" | "mmd" => Preview::Mermaid(text()),
        "md" | "markdown" => Preview::Markdown(text()),
        // Binary STL/PLY share the text extensions; NUL bytes keep them out of the fence.
        _ if bytes.contains(&0) => Preview::Binary { size: bytes.len() },
        _ => match model_fence(&ext) {
            Some(fence) => Preview::Model { fence, text: text() },
            None => Preview::Code {
                text: text(),
                lang: language(&ext),
            },
        },
    }
}

/// Which file version a preview belongs to; a changed size or mtime reloads it.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Stamp {
    pub(crate) path: String,
    pub(crate) size: Option<u64>,
    pub(crate) mtime: Option<f64>,
}

pub(crate) enum Slot {
    Loading(Instant),
    Ready(Preview),
}

/// The preview of the active file, loaded on a worker thread per request.
pub(crate) struct Previews {
    current: Option<(Stamp, Slot)>,
    tx: Sender<(Stamp, Preview)>,
    rx: Receiver<(Stamp, Preview)>,
}

impl Previews {
    pub(crate) fn new() -> Self {
        let (tx, rx) = mpsc::channel();
        Self {
            current: None,
            tx,
            rx,
        }
    }

    /// Makes `stamp` the active preview, starting its load when it is new.
    pub(crate) fn want(&mut self, stamp: Stamp) {
        if self.current.as_ref().is_some_and(|(current, _)| *current == stamp) {
            return;
        }
        let tx = self.tx.clone();
        let job = stamp.clone();
        thread::spawn(move || {
            let preview = load(&job.path);
            let _ = tx.send((job, preview));
        });
        self.current = Some((stamp, Slot::Loading(Instant::now())));
    }

    pub(crate) fn forget(&mut self) {
        self.current = None;
    }

    /// Takes finished loads; true when the active preview just became ready.
    pub(crate) fn poll(&mut self) -> bool {
        let mut ready = false;
        while let Ok((stamp, preview)) = self.rx.try_recv() {
            if let Some((current, slot)) = &mut self.current {
                if *current == stamp {
                    *slot = Slot::Ready(preview);
                    ready = true;
                }
            }
        }
        ready
    }

    pub(crate) fn get(&self, stamp: &Stamp) -> Option<&Slot> {
        self.current
            .as_ref()
            .filter(|(current, _)| current == stamp)
            .map(|(_, slot)| slot)
    }

    /// The ready preview, for uploading its image bytes.
    pub(crate) fn ready_mut(&mut self) -> Option<&mut Preview> {
        match &mut self.current {
            Some((_, Slot::Ready(preview))) => Some(preview),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::symlink;

    struct Dir(std::path::PathBuf);

    impl Dir {
        fn new(name: &str) -> Self {
            let path = std::env::temp_dir().join(format!("tern-yazi-preview-{}-{name}", std::process::id()));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        fn file(&self, name: &str, bytes: &[u8]) -> String {
            let path = self.0.join(name);
            fs::write(&path, bytes).unwrap();
            path.to_string_lossy().into_owned()
        }
    }

    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn bounded_reads_reject_oversized_symlinked_and_special_files() {
        let dir = Dir::new("bounded");
        let path = dir.file("a.txt", b"hello");
        assert_eq!(read_bounded(&path, 5).unwrap(), b"hello");
        assert!(read_bounded(&path, 4).is_err());
        let link = dir.0.join("link.txt");
        symlink(&path, &link).unwrap();
        assert!(read_bounded(&link.to_string_lossy(), 5).is_err());
        assert!(read_bounded(&dir.0.to_string_lossy(), 5).is_err());
    }

    #[test]
    fn classifies_by_extension_then_content() {
        let dir = Dir::new("classify");
        assert_eq!(
            load(&dir.file("a.rs", b"fn main() {}")),
            Preview::Code { text: "fn main() {}".to_owned(), lang: "rust" }
        );
        assert_eq!(load(&dir.file("a.md", b"# t")), Preview::Markdown("# t".to_owned()));
        assert_eq!(load(&dir.file("a.mmd", b"graph TD")), Preview::Mermaid("graph TD".to_owned()));
        assert_eq!(
            load(&dir.file("a.wrl", b"#VRML V2.0")),
            Preview::Model { fence: "vrml", text: "#VRML V2.0".to_owned() }
        );
        assert_eq!(load(&dir.file("a.stl", b"solid\0x")), Preview::Binary { size: 7 });
        assert!(matches!(
            load(&dir.file("a.svg", b"<svg/>")),
            Preview::Image { mime: "image/svg+xml", size: 6, blob: Some(_), .. }
        ));
        let big = vec![b'x'; (MERMAID_BYTES + 1) as usize];
        assert_eq!(load(&dir.file("b.mmd", &big)), Preview::Unavailable { limit: MERMAID_BYTES });
        assert_eq!(Preview::Unavailable { limit: 1 }.kind("x.py"), "python");
    }
}
