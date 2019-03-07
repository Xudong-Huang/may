//! Filesystem manipulation operations.
//!
//! This module contains basic methods to manipulate the contents of the local
//! filesystem. All methods in this module represent cross-platform filesystem
//! operations. compatible with std::fs::*

use std::fmt;
use std::fs::{File as StdFile, Metadata, OpenOptions, Permissions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::Path;

use coroutine_impl::is_coroutine;
use io as io_impl;
use io::sys::fs as fs_impl;
use yield_now::yield_with;

pub struct File {
    sys: StdFile,
    io: fs_impl::FileIo,
    ctx: io_impl::IoContext,
    offset: u64,
}

impl File {
    // the file must be created with flag FILE_FLAG_OVERLAPPED
    pub fn from(file: StdFile) -> io::Result<File> {
        if !is_coroutine() {
            #[cold]
            {
                return Ok(File {
                    io: fs_impl::FileIo::new(None)?,
                    sys: file,
                    ctx: io_impl::IoContext::new(),
                    offset: 0,
                });
            }
        }

        Ok(File {
            io: fs_impl::FileIo::new(Some(&file))?,
            sys: file,
            ctx: io_impl::IoContext::new(),
            offset: 0,
        })
    }

    pub fn open<P: AsRef<Path>>(path: P) -> io::Result<File> {
        if !is_coroutine() {
            #[cold]
            {
                let file = StdFile::open(path)?;
                return Ok(File {
                    io: fs_impl::FileIo::new(None)?,
                    sys: file,
                    ctx: io_impl::IoContext::new(),
                    offset: 0,
                });
            }
        }

        let file = fs_impl::open(path)?;
        File::from(file)
    }

    pub fn open_with_options<P: AsRef<Path>>(
        options: &mut OpenOptions,
        path: P,
    ) -> io::Result<File> {
        if !is_coroutine() {
            #[cold]
            {
                let file = options.open(path)?;
                return Ok(File {
                    io: fs_impl::FileIo::new(None)?,
                    sys: file,
                    ctx: io_impl::IoContext::new(),
                    offset: 0,
                });
            }
        }

        let file = fs_impl::open_with_options(options, path)?;
        File::from(file)
    }

    pub fn create<P: AsRef<Path>>(path: P) -> io::Result<File> {
        if !is_coroutine() {
            #[cold]
            {
                let file = StdFile::create(path)?;
                return Ok(File {
                    io: fs_impl::FileIo::new(None)?,
                    sys: file,
                    ctx: io_impl::IoContext::new(),
                    offset: 0,
                });
            }
        }
        let file = fs_impl::create(path)?;
        File::from(file)
    }

    pub fn sync_all(&self) -> io::Result<()> {
        self.sys.sync_all()
    }

    pub fn sync_data(&self) -> io::Result<()> {
        self.sys.sync_data()
    }

    pub fn set_len(&self, size: u64) -> io::Result<()> {
        self.sys.set_len(size)
    }

    pub fn metadata(&self) -> io::Result<Metadata> {
        self.sys.metadata()
    }

    pub fn try_clone(&self) -> io::Result<File> {
        // windows overlapped duplicate would return err
        // The parameter is incorrect. (os error 87)
        let file = self.sys.try_clone()?;
        File::from(file)
    }

    pub fn set_permissions(&self, perm: Permissions) -> io::Result<()> {
        self.sys.set_permissions(perm)
    }
}

impl fmt::Debug for File {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        self.sys.fmt(f)
    }
}

impl Read for File {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if !self.ctx.check_context(|_| Ok(()))? {
            #[cold]
            return self.sys.read(buf);
        }

        self.io.reset();

        let ret = {
            let reader = fs_impl::FileRead::new(self, self.offset, buf);
            yield_with(&reader);
            reader.done()
        };

        match ret {
            Ok(len) => {
                self.offset += len as u64;
                Ok(len)
            }
            Err(e) => {
                if e.to_string().contains("end") {
                    return Ok(0);
                }
                Err(e)
            }
        }
    }
}

impl Write for File {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if !self.ctx.check_context(|_| Ok(()))? {
            #[cold]
            return self.sys.write(buf);
        }

        self.io.reset();

        let len = {
            let writer = fs_impl::FileWrite::new(self, self.offset, buf);
            yield_with(&writer);
            writer.done()?
        };

        self.offset += len as u64;
        Ok(len)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.sys.flush()
    }
}

impl Seek for File {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        if !self.ctx.check_context(|_| Ok(()))? {
            #[cold]
            return self.sys.seek(pos);
        }

        match pos {
            SeekFrom::Start(x) => self.offset = x,
            SeekFrom::End(i) => {
                let len = self.sys.metadata()?.len() as i64;
                self.offset = (len + i) as u64;
            }
            SeekFrom::Current(i) => {
                let cur = self.offset as i64;
                self.offset = (cur + i) as u64;
            }
        }
        Ok(self.offset)
    }
}

#[cfg(unix)]
impl fs_impl::AsFileIo for File {
    fn as_file_io(&self) -> &fs_impl::FileIo {
        &self.io
    }
}

#[cfg(unix)]
impl ::std::os::unix::io::AsRawFd for File {
    fn as_raw_fd(&self) -> ::std::os::unix::io::RawFd {
        self.sys.as_raw_fd()
    }
}

#[cfg(windows)]
impl ::std::os::windows::io::AsRawHandle for File {
    fn as_raw_handle(&self) -> ::std::os::windows::io::RawHandle {
        self.sys.as_raw_handle()
    }
}

/// How large a buffer to pre-allocate before reading the entire file.
fn initial_buffer_size(file: &File) -> usize {
    // Allocate one extra byte so the buffer doesn't need to grow before the
    // final `read` call at the end of the file.  Don't worry about `usize`
    // overflow because reading will fail regardless in that case.
    file.metadata().map(|m| m.len() as usize + 1).unwrap_or(0)
}

pub fn read<P: AsRef<Path>>(path: P) -> io::Result<Vec<u8>> {
    let mut file = File::open(path)?;
    let mut bytes = Vec::with_capacity(initial_buffer_size(&file));
    file.read_to_end(&mut bytes)?;
    Ok(bytes)
}

pub fn read_to_string<P: AsRef<Path>>(path: P) -> io::Result<String> {
    let mut file = File::open(path)?;
    let mut string = String::with_capacity(initial_buffer_size(&file));
    file.read_to_string(&mut string)?;
    Ok(string)
}

pub fn write<P: AsRef<Path>, C: AsRef<[u8]>>(path: P, contents: C) -> io::Result<()> {
    File::create(path)?.write_all(contents.as_ref())
}

// pub fn copy<P: AsRef<Path>, Q: AsRef<Path>>(from: P, to: Q) -> io::Result<u64> {
//     fs_imp::copy(from.as_ref(), to.as_ref())
// }

#[cfg(all(test, not(target_os = "emscripten")))]
mod tests {
    extern crate rand;
    use std::io::prelude::*;

    use super::*;
    use std::fs::{self, OpenOptions};
    use std::io::SeekFrom;
    use std::str;
    use tempdir::TempDir;

    fn tmpdir() -> TempDir {
        TempDir::new("test").expect("failed to create TempDir")
    }

    macro_rules! check {
        ($e:expr) => {
            match $e {
                Ok(t) => t,
                Err(e) => panic!("{} failed with: {}", stringify!($e), e),
            }
        };
    }

    macro_rules! error_contains {
        ($e:expr, $s:expr) => {
            match $e {
                Ok(_) => panic!("Unexpected success. Should've been: {:?}", $s),
                Err(ref err) => assert!(
                    err.to_string().contains($s),
                    format!("`{}` did not contain `{}`", err, $s)
                ),
            }
        };
    }

    #[test]
    fn file_test_thread_io_smoke_test() {
        let message = "it's alright. have a good time";
        let tmpdir = tmpdir();
        let filename = &tmpdir.as_ref().join("file_rt_io_file_test.txt");
        {
            let mut write_stream = check!(File::create(filename));
            check!(write_stream.write(message.as_bytes()));
        }
        {
            let mut read_stream = check!(File::open(filename));
            let mut read_buf = [0; 1028];
            let read_str = match check!(read_stream.read(&mut read_buf)) {
                0 => panic!("shouldn't happen"),
                n => str::from_utf8(&read_buf[..n]).unwrap().to_string(),
            };
            assert_eq!(read_str, message);
        }
        check!(fs::remove_file(filename));
    }

    #[test]
    fn file_test_io_smoke_test() {
        let message = "it's alright. have a good time";
        let tmpdir = tmpdir();
        go!(move || {
            let filename = &tmpdir.as_ref().join("file_rt_io_file_test.txt");
            {
                let mut write_stream = check!(File::create(filename));
                check!(write_stream.write(message.as_bytes()));
            }
            {
                let mut read_stream = check!(File::open(filename));
                let mut read_buf = [0; 1028];
                let read_str = match check!(read_stream.read(&mut read_buf)) {
                    0 => panic!("shouldn't happen"),
                    n => str::from_utf8(&read_buf[..n]).unwrap().to_string(),
                };
                assert_eq!(read_str, message);
            }
            check!(fs::remove_file(filename));
        })
        .join()
        .unwrap();
    }

    #[test]
    fn file_test_io_non_positional_read() {
        let message: &str = "ten-four";
        let mut read_mem = [0; 8];
        let tmpdir = tmpdir();
        go!(move || {
            let filename = &tmpdir.as_ref().join("file_rt_io_file_test_positional.txt");
            {
                let mut rw_stream = check!(File::create(filename));
                check!(rw_stream.write(message.as_bytes()));
            }
            {
                let mut read_stream = check!(File::open(filename));
                {
                    let read_buf = &mut read_mem[0..4];
                    check!(read_stream.read(read_buf));
                }
                {
                    let read_buf = &mut read_mem[4..8];
                    check!(read_stream.read(read_buf));
                }
            }
            check!(fs::remove_file(filename));
            let read_str = str::from_utf8(&read_mem).unwrap();
            assert_eq!(read_str, message);
        })
        .join()
        .unwrap();
    }

    #[test]
    fn file_test_io_seek_and_tell_smoke_test() {
        let message = "ten-four";
        let mut read_mem = [0; 4];
        let set_cursor = 4 as u64;
        let tmpdir = tmpdir();
        go!(move || {
            let tell_pos_pre_read;
            let tell_pos_post_read;
            let filename = &tmpdir.as_ref().join("file_rt_io_file_test_seeking.txt");
            {
                let mut rw_stream = check!(File::create(filename));
                check!(rw_stream.write(message.as_bytes()));
            }
            {
                let mut read_stream = check!(File::open(filename));
                check!(read_stream.seek(SeekFrom::Start(set_cursor)));
                tell_pos_pre_read = check!(read_stream.seek(SeekFrom::Current(0)));
                check!(read_stream.read(&mut read_mem));
                tell_pos_post_read = check!(read_stream.seek(SeekFrom::Current(0)));
            }
            check!(fs::remove_file(filename));
            let read_str = str::from_utf8(&read_mem).unwrap();
            assert_eq!(read_str, &message[4..8]);
            assert_eq!(tell_pos_pre_read, set_cursor);
            assert_eq!(tell_pos_post_read, message.len() as u64);
        })
        .join()
        .unwrap();
    }

    #[test]
    fn file_test_io_seek_and_write() {
        let initial_msg = "food-is-yummy";
        let overwrite_msg = "-the-bar!!";
        let final_msg = "foo-the-bar!!";
        let seek_idx = 3;
        let mut read_mem = [0; 13];
        let tmpdir = tmpdir();
        go!(move || {
            let filename = &tmpdir
                .as_ref()
                .join("file_rt_io_file_test_seek_and_write.txt");
            {
                let mut rw_stream = check!(File::create(filename));
                check!(rw_stream.write(initial_msg.as_bytes()));
                check!(rw_stream.seek(SeekFrom::Start(seek_idx)));
                check!(rw_stream.write(overwrite_msg.as_bytes()));
            }
            {
                let mut read_stream = check!(File::open(filename));
                check!(read_stream.read(&mut read_mem));
            }
            check!(fs::remove_file(filename));
            let read_str = str::from_utf8(&read_mem).unwrap();
            assert!(read_str == final_msg);
        })
        .join()
        .unwrap();
    }

    #[test]
    fn file_test_io_seek_shakedown() {
        //                   01234567890123
        let initial_msg = "qwer-asdf-zxcv";
        let chunk_one: &str = "qwer";
        let chunk_two: &str = "asdf";
        let chunk_three: &str = "zxcv";
        let mut read_mem = [0; 4];
        let tmpdir = tmpdir();
        go!(move || {
            let filename = &tmpdir
                .as_ref()
                .join("file_rt_io_file_test_seek_shakedown.txt");
            {
                let mut rw_stream = check!(File::create(filename));
                check!(rw_stream.write(initial_msg.as_bytes()));
            }
            {
                let mut read_stream = check!(File::open(filename));

                check!(read_stream.seek(SeekFrom::End(-4)));
                check!(read_stream.read(&mut read_mem));
                assert_eq!(str::from_utf8(&read_mem).unwrap(), chunk_three);

                check!(read_stream.seek(SeekFrom::Current(-9)));
                check!(read_stream.read(&mut read_mem));
                assert_eq!(str::from_utf8(&read_mem).unwrap(), chunk_two);

                check!(read_stream.seek(SeekFrom::Start(0)));
                check!(read_stream.read(&mut read_mem));
                assert_eq!(str::from_utf8(&read_mem).unwrap(), chunk_one);
            }
            check!(fs::remove_file(filename));
        })
        .join()
        .unwrap();
    }

    #[test]
    fn file_test_io_eof() {
        let tmpdir = tmpdir();
        go!(move || {
            let mut buf = [0; 16];
            let filename = tmpdir.as_ref().join("file_rt_io_file_test_eof.txt");
            {
                let mut rw = check!(File::open_with_options(
                    OpenOptions::new().create_new(true).write(true).read(true),
                    &filename
                ));
                assert_eq!(check!(rw.read(&mut buf)), 0);
                assert_eq!(check!(rw.read(&mut buf)), 0);
            }
            check!(fs::remove_file(&filename));
        })
        .join()
        .unwrap();
    }

    //     #[test]
    //     fn copy_file_ok() {
    //         let tmpdir = tmpdir();
    //         let input = tmpdir.join("in.txt");
    //         let out = tmpdir.join("out.txt");

    //         check!(check!(File::create(&input)).write(b"hello"));
    //         check!(fs::copy(&input, &out));
    //         let mut v = Vec::new();
    //         check!(check!(File::open(&out)).read_to_end(&mut v));
    //         assert_eq!(v, b"hello");

    //         assert_eq!(
    //             check!(input.metadata()).permissions(),
    //             check!(out.metadata()).permissions()
    //         );
    //     }

    #[test]
    fn write_then_read() {
        use self::rand::{rngs::StdRng, FromEntropy, RngCore};
        let mut bytes = [0; 1024];
        StdRng::from_entropy().fill_bytes(&mut bytes);

        let tmpdir = tmpdir();
        go!(move || {
            check!(fs::write(&tmpdir.as_ref().join("test"), &bytes[..]));
            let v = check!(fs::read(&tmpdir.as_ref().join("test")));
            assert!(v == &bytes[..]);

            check!(fs::write(&tmpdir.as_ref().join("not-utf8"), &[0xFF]));
            error_contains!(
                fs::read_to_string(&tmpdir.as_ref().join("not-utf8")),
                "stream did not contain valid UTF-8"
            );

            let s = "𐁁𐀓𐀠𐀴𐀍";
            check!(fs::write(&tmpdir.as_ref().join("utf8"), s.as_bytes()));
            let string = check!(fs::read_to_string(&tmpdir.as_ref().join("utf8")));
            assert_eq!(string, s);
        })
        .join()
        .unwrap();
    }

    #[test]
    #[cfg(not(windows))]
    fn file_try_clone() {
        let tmpdir = tmpdir();
        go!(move || {
            let mut f1 = check!(File::open_with_options(
                OpenOptions::new().read(true).write(true).create(true),
                &tmpdir.as_ref().join("test")
            ));
            let mut f2 = check!(f1.try_clone());

            check!(f1.write_all(b"hello world"));
            check!(f1.seek(SeekFrom::Start(2)));

            let mut buf = vec![];
            check!(f2.read_to_end(&mut buf));
            assert_eq!(buf, b"llo world");
            drop(f2);

            check!(f1.write_all(b"!"));
        })
        .join()
        .unwrap();
    }
}
