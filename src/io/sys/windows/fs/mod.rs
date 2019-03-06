mod fs_read;
mod fs_write;

pub use self::fs_read::FileRead;
pub use self::fs_write::FileWrite;

use std::fs::{File, OpenOptions};
use std::io;
use std::os::windows::fs::OpenOptionsExt;
use std::path::Path;

use winapi::um::winbase::FILE_FLAG_OVERLAPPED;

pub fn open<P: AsRef<Path>>(path: P) -> io::Result<File> {
    OpenOptions::new()
        .read(true)
        .custom_flags(FILE_FLAG_OVERLAPPED)
        .open(path.as_ref())
}

pub fn create<P: AsRef<Path>>(path: P) -> io::Result<File> {
    OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .custom_flags(FILE_FLAG_OVERLAPPED)
        .open(path.as_ref())
}

pub fn open_with_options<P: AsRef<Path>>(options: &mut OpenOptions, path: P) -> io::Result<File> {
    options
        .custom_flags(FILE_FLAG_OVERLAPPED)
        .open(path.as_ref())
}
