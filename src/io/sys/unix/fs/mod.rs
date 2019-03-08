#[cfg(any(target_os = "linux", target_os = "android"))]
#[path = "linux/mod.rs"]
mod detail;

#[cfg(any(
    target_os = "bitrig",
    target_os = "dragonfly",
    target_os = "freebsd",
    target_os = "ios",
    target_os = "macos",
    target_os = "netbsd",
    target_os = "openbsd"
))]
#[path = "bsd/mod.rs"]
mod detail;

use nix::fcntl::OFlag::O_DIRECT;
use std::fs::{File, OpenOptions};
use std::io;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

pub use self::detail::{FileIo, FileRead, FileWrite};

pub trait AsFileIo {
    fn as_file_io(&self) -> &FileIo;
}

pub fn open<P: AsRef<Path>>(path: P) -> io::Result<File> {
    OpenOptions::new()
        .read(true)
        .custom_flags(O_DIRECT)
        .open(path.as_ref())
}

pub fn create<P: AsRef<Path>>(path: P) -> io::Result<File> {
    OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .custom_flags(O_DIRECT)
        .open(path.as_ref())
}

pub fn open_with_options<P: AsRef<Path>>(options: &mut OpenOptions, path: P) -> io::Result<File> {
    options.custom_flags(O_DIRECT).open(path.as_ref())
}
