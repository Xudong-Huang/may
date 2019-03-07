mod fs_read;
mod fs_write;

pub use self::fs_read::FileRead;
pub use self::fs_write::FileWrite;

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
