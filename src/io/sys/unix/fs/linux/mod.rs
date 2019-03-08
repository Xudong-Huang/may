mod aio_bindings;
mod aio_manager;
mod fs_read;
mod fs_write;

pub use self::fs_read::FileRead;
pub use self::fs_write::FileWrite;

use std::fs::File;

pub struct FileIo {}

pub trait AsFileIo {
    fn as_file_io(&self) -> &FileIo;
}

impl FileIo {
    pub fn new(file: Option<&File>) -> io::Result<FileIo> {}
}
