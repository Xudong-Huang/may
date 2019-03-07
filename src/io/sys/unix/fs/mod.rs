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

use std::fs::{File, OpenOptions};
use std::io;
use std::path::Path;

pub fn open<P: AsRef<Path>>(path: P) -> io::Result<File> {
    File::open(path)
}

pub fn create<P: AsRef<Path>>(path: P) -> io::Result<File> {
    File::create(path)
}

pub fn open_with_options<P: AsRef<Path>>(options: &mut OpenOptions, path: P) -> io::Result<File> {
    options.open(path.as_ref())
}

pub use self::detail::{AsFileIo, FileIo, FileRead, FileWrite};
