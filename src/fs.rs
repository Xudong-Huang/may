//! Filesystem manipulation operations.
//!
//! This module contains basic methods to manipulate the contents of the local
//! filesystem. All methods in this module represent cross-platform filesystem
//! operations. compatible with std::fs::*

use std::fmt;
use std::fs::{File as StdFile, Metadata, OpenOptions, Permissions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::Path;

use io as io_impl;
use io::sys::fs as fs_impl;
use yield_now::yield_with;

pub struct File {
    sys: StdFile,
    io: io_impl::IoData,
    ctx: io_impl::IoContext,
}

impl File {
    pub fn open<P: AsRef<Path>>(path: P) -> io::Result<File> {
        let file = OpenOptions::new().read(true).open(path.as_ref())?;

        io_impl::add_file(&file).map(|io| File {
            io,
            sys: file,
            ctx: io_impl::IoContext::new(),
        })
    }

    pub fn create<P: AsRef<Path>>(path: P) -> io::Result<File> {
        let file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(path.as_ref())?;

        io_impl::add_file(&file).map(|io| File {
            io,
            sys: file,
            ctx: io_impl::IoContext::new(),
        })
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
        let file = self.sys.try_clone()?;

        io_impl::add_file(&file).map(|io| File {
            io,
            sys: file,
            ctx: io_impl::IoContext::new(),
        })
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

        let reader = fs_impl::FileRead::new(&self.sys, buf);
        yield_with(&reader);
        reader.done()
    }
}

impl Write for File {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if !self.ctx.check_context(|_| Ok(()))? {
            #[cold]
            return self.sys.write(buf);
        }

        self.io.reset();

        let writer = fs_impl::FileWrite::new(&self.sys, buf);
        yield_with(&writer);
        writer.done()
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Seek for File {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        self.sys.seek(pos)
    }
}

// impl<'a> Read for &'a File {
//     fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
//         self.inner.read(buf)
//     }
// }

// impl<'a> Write for &'a File {
//     fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
//         self.inner.write(buf)
//     }

//     fn flush(&mut self) -> io::Result<()> {
//         Ok(())
//     }
// }

// impl<'a> Seek for &'a File {
//     fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
//         self.inner.seek(pos)
//     }
// }

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

// #[cfg(test)]
// mod tests {
//     use std::io::prelude::*;

//     use fs::{self, File, OpenOptions};
//     use rand::{rngs::StdRng, FromEntropy, RngCore};
//     use std::io::{ErrorKind, SeekFrom};
//     use std::path::Path;
//     use str;
//     use sys_common::io::test::{tmpdir, TempDir};

//     macro_rules! check {
//         ($e:expr) => {
//             match $e {
//                 Ok(t) => t,
//                 Err(e) => panic!("{} failed with: {}", stringify!($e), e),
//             }
//         };
//     }

//     #[cfg(windows)]
//     macro_rules! error {
//         ($e:expr, $s:expr) => {
//             match $e {
//                 Ok(_) => panic!("Unexpected success. Should've been: {:?}", $s),
//                 Err(ref err) => assert!(
//                     err.raw_os_error() == Some($s),
//                     format!("`{}` did not have a code of `{}`", err, $s)
//                 ),
//             }
//         };
//     }

//     #[cfg(unix)]
//     macro_rules! error {
//         ($e:expr, $s:expr) => {
//             error_contains!($e, $s)
//         };
//     }

//     macro_rules! error_contains {
//         ($e:expr, $s:expr) => {
//             match $e {
//                 Ok(_) => panic!("Unexpected success. Should've been: {:?}", $s),
//                 Err(ref err) => assert!(
//                     err.to_string().contains($s),
//                     format!("`{}` did not contain `{}`", err, $s)
//                 ),
//             }
//         };
//     }

//     #[test]
//     fn file_test_io_smoke_test() {
//         let message = "it's alright. have a good time";
//         let tmpdir = tmpdir();
//         let filename = &tmpdir.join("file_rt_io_file_test.txt");
//         {
//             let mut write_stream = check!(File::create(filename));
//             check!(write_stream.write(message.as_bytes()));
//         }
//         {
//             let mut read_stream = check!(File::open(filename));
//             let mut read_buf = [0; 1028];
//             let read_str = match check!(read_stream.read(&mut read_buf)) {
//                 0 => panic!("shouldn't happen"),
//                 n => str::from_utf8(&read_buf[..n]).unwrap().to_string(),
//             };
//             assert_eq!(read_str, message);
//         }
//         check!(fs::remove_file(filename));
//     }

//     #[test]
//     fn file_test_io_non_positional_read() {
//         let message: &str = "ten-four";
//         let mut read_mem = [0; 8];
//         let tmpdir = tmpdir();
//         let filename = &tmpdir.join("file_rt_io_file_test_positional.txt");
//         {
//             let mut rw_stream = check!(File::create(filename));
//             check!(rw_stream.write(message.as_bytes()));
//         }
//         {
//             let mut read_stream = check!(File::open(filename));
//             {
//                 let read_buf = &mut read_mem[0..4];
//                 check!(read_stream.read(read_buf));
//             }
//             {
//                 let read_buf = &mut read_mem[4..8];
//                 check!(read_stream.read(read_buf));
//             }
//         }
//         check!(fs::remove_file(filename));
//         let read_str = str::from_utf8(&read_mem).unwrap();
//         assert_eq!(read_str, message);
//     }

//     #[test]
//     fn file_test_io_seek_and_tell_smoke_test() {
//         let message = "ten-four";
//         let mut read_mem = [0; 4];
//         let set_cursor = 4 as u64;
//         let tell_pos_pre_read;
//         let tell_pos_post_read;
//         let tmpdir = tmpdir();
//         let filename = &tmpdir.join("file_rt_io_file_test_seeking.txt");
//         {
//             let mut rw_stream = check!(File::create(filename));
//             check!(rw_stream.write(message.as_bytes()));
//         }
//         {
//             let mut read_stream = check!(File::open(filename));
//             check!(read_stream.seek(SeekFrom::Start(set_cursor)));
//             tell_pos_pre_read = check!(read_stream.seek(SeekFrom::Current(0)));
//             check!(read_stream.read(&mut read_mem));
//             tell_pos_post_read = check!(read_stream.seek(SeekFrom::Current(0)));
//         }
//         check!(fs::remove_file(filename));
//         let read_str = str::from_utf8(&read_mem).unwrap();
//         assert_eq!(read_str, &message[4..8]);
//         assert_eq!(tell_pos_pre_read, set_cursor);
//         assert_eq!(tell_pos_post_read, message.len() as u64);
//     }

//     #[test]
//     fn file_test_io_seek_and_write() {
//         let initial_msg = "food-is-yummy";
//         let overwrite_msg = "-the-bar!!";
//         let final_msg = "foo-the-bar!!";
//         let seek_idx = 3;
//         let mut read_mem = [0; 13];
//         let tmpdir = tmpdir();
//         let filename = &tmpdir.join("file_rt_io_file_test_seek_and_write.txt");
//         {
//             let mut rw_stream = check!(File::create(filename));
//             check!(rw_stream.write(initial_msg.as_bytes()));
//             check!(rw_stream.seek(SeekFrom::Start(seek_idx)));
//             check!(rw_stream.write(overwrite_msg.as_bytes()));
//         }
//         {
//             let mut read_stream = check!(File::open(filename));
//             check!(read_stream.read(&mut read_mem));
//         }
//         check!(fs::remove_file(filename));
//         let read_str = str::from_utf8(&read_mem).unwrap();
//         assert!(read_str == final_msg);
//     }

//     #[test]
//     fn file_test_io_seek_shakedown() {
//         //                   01234567890123
//         let initial_msg = "qwer-asdf-zxcv";
//         let chunk_one: &str = "qwer";
//         let chunk_two: &str = "asdf";
//         let chunk_three: &str = "zxcv";
//         let mut read_mem = [0; 4];
//         let tmpdir = tmpdir();
//         let filename = &tmpdir.join("file_rt_io_file_test_seek_shakedown.txt");
//         {
//             let mut rw_stream = check!(File::create(filename));
//             check!(rw_stream.write(initial_msg.as_bytes()));
//         }
//         {
//             let mut read_stream = check!(File::open(filename));

//             check!(read_stream.seek(SeekFrom::End(-4)));
//             check!(read_stream.read(&mut read_mem));
//             assert_eq!(str::from_utf8(&read_mem).unwrap(), chunk_three);

//             check!(read_stream.seek(SeekFrom::Current(-9)));
//             check!(read_stream.read(&mut read_mem));
//             assert_eq!(str::from_utf8(&read_mem).unwrap(), chunk_two);

//             check!(read_stream.seek(SeekFrom::Start(0)));
//             check!(read_stream.read(&mut read_mem));
//             assert_eq!(str::from_utf8(&read_mem).unwrap(), chunk_one);
//         }
//         check!(fs::remove_file(filename));
//     }

//     #[test]
//     fn file_test_io_eof() {
//         let tmpdir = tmpdir();
//         let filename = tmpdir.join("file_rt_io_file_test_eof.txt");
//         let mut buf = [0; 256];
//         {
//             let oo = OpenOptions::new()
//                 .create_new(true)
//                 .write(true)
//                 .read(true)
//                 .clone();
//             let mut rw = check!(oo.open(&filename));
//             assert_eq!(check!(rw.read(&mut buf)), 0);
//             assert_eq!(check!(rw.read(&mut buf)), 0);
//         }
//         check!(fs::remove_file(&filename));
//     }

//     #[test]
//     #[cfg(unix)]
//     fn file_test_io_read_write_at() {
//         use os::unix::fs::FileExt;

//         let tmpdir = tmpdir();
//         let filename = tmpdir.join("file_rt_io_file_test_read_write_at.txt");
//         let mut buf = [0; 256];
//         let write1 = "asdf";
//         let write2 = "qwer-";
//         let write3 = "-zxcv";
//         let content = "qwer-asdf-zxcv";
//         {
//             let oo = OpenOptions::new()
//                 .create_new(true)
//                 .write(true)
//                 .read(true)
//                 .clone();
//             let mut rw = check!(oo.open(&filename));
//             assert_eq!(check!(rw.write_at(write1.as_bytes(), 5)), write1.len());
//             assert_eq!(check!(rw.seek(SeekFrom::Current(0))), 0);
//             assert_eq!(check!(rw.read_at(&mut buf, 5)), write1.len());
//             assert_eq!(str::from_utf8(&buf[..write1.len()]), Ok(write1));
//             assert_eq!(check!(rw.seek(SeekFrom::Current(0))), 0);
//             assert_eq!(
//                 check!(rw.read_at(&mut buf[..write2.len()], 0)),
//                 write2.len()
//             );
//             assert_eq!(str::from_utf8(&buf[..write2.len()]), Ok("\0\0\0\0\0"));
//             assert_eq!(check!(rw.seek(SeekFrom::Current(0))), 0);
//             assert_eq!(check!(rw.write(write2.as_bytes())), write2.len());
//             assert_eq!(check!(rw.seek(SeekFrom::Current(0))), 5);
//             assert_eq!(check!(rw.read(&mut buf)), write1.len());
//             assert_eq!(str::from_utf8(&buf[..write1.len()]), Ok(write1));
//             assert_eq!(check!(rw.seek(SeekFrom::Current(0))), 9);
//             assert_eq!(
//                 check!(rw.read_at(&mut buf[..write2.len()], 0)),
//                 write2.len()
//             );
//             assert_eq!(str::from_utf8(&buf[..write2.len()]), Ok(write2));
//             assert_eq!(check!(rw.seek(SeekFrom::Current(0))), 9);
//             assert_eq!(check!(rw.write_at(write3.as_bytes(), 9)), write3.len());
//             assert_eq!(check!(rw.seek(SeekFrom::Current(0))), 9);
//         }
//         {
//             let mut read = check!(File::open(&filename));
//             assert_eq!(check!(read.read_at(&mut buf, 0)), content.len());
//             assert_eq!(str::from_utf8(&buf[..content.len()]), Ok(content));
//             assert_eq!(check!(read.seek(SeekFrom::Current(0))), 0);
//             assert_eq!(check!(read.seek(SeekFrom::End(-5))), 9);
//             assert_eq!(check!(read.read_at(&mut buf, 0)), content.len());
//             assert_eq!(str::from_utf8(&buf[..content.len()]), Ok(content));
//             assert_eq!(check!(read.seek(SeekFrom::Current(0))), 9);
//             assert_eq!(check!(read.read(&mut buf)), write3.len());
//             assert_eq!(str::from_utf8(&buf[..write3.len()]), Ok(write3));
//             assert_eq!(check!(read.seek(SeekFrom::Current(0))), 14);
//             assert_eq!(check!(read.read_at(&mut buf, 0)), content.len());
//             assert_eq!(str::from_utf8(&buf[..content.len()]), Ok(content));
//             assert_eq!(check!(read.seek(SeekFrom::Current(0))), 14);
//             assert_eq!(check!(read.read_at(&mut buf, 14)), 0);
//             assert_eq!(check!(read.read_at(&mut buf, 15)), 0);
//             assert_eq!(check!(read.seek(SeekFrom::Current(0))), 14);
//         }
//         check!(fs::remove_file(&filename));
//     }

//     #[test]
//     #[cfg(windows)]
//     fn file_test_io_seek_read_write() {
//         use os::windows::fs::FileExt;

//         let tmpdir = tmpdir();
//         let filename = tmpdir.join("file_rt_io_file_test_seek_read_write.txt");
//         let mut buf = [0; 256];
//         let write1 = "asdf";
//         let write2 = "qwer-";
//         let write3 = "-zxcv";
//         let content = "qwer-asdf-zxcv";
//         {
//             let oo = OpenOptions::new()
//                 .create_new(true)
//                 .write(true)
//                 .read(true)
//                 .clone();
//             let mut rw = check!(oo.open(&filename));
//             assert_eq!(check!(rw.seek_write(write1.as_bytes(), 5)), write1.len());
//             assert_eq!(check!(rw.seek(SeekFrom::Current(0))), 9);
//             assert_eq!(check!(rw.seek_read(&mut buf, 5)), write1.len());
//             assert_eq!(str::from_utf8(&buf[..write1.len()]), Ok(write1));
//             assert_eq!(check!(rw.seek(SeekFrom::Current(0))), 9);
//             assert_eq!(check!(rw.seek(SeekFrom::Start(0))), 0);
//             assert_eq!(check!(rw.write(write2.as_bytes())), write2.len());
//             assert_eq!(check!(rw.seek(SeekFrom::Current(0))), 5);
//             assert_eq!(check!(rw.read(&mut buf)), write1.len());
//             assert_eq!(str::from_utf8(&buf[..write1.len()]), Ok(write1));
//             assert_eq!(check!(rw.seek(SeekFrom::Current(0))), 9);
//             assert_eq!(
//                 check!(rw.seek_read(&mut buf[..write2.len()], 0)),
//                 write2.len()
//             );
//             assert_eq!(str::from_utf8(&buf[..write2.len()]), Ok(write2));
//             assert_eq!(check!(rw.seek(SeekFrom::Current(0))), 5);
//             assert_eq!(check!(rw.seek_write(write3.as_bytes(), 9)), write3.len());
//             assert_eq!(check!(rw.seek(SeekFrom::Current(0))), 14);
//         }
//         {
//             let mut read = check!(File::open(&filename));
//             assert_eq!(check!(read.seek_read(&mut buf, 0)), content.len());
//             assert_eq!(str::from_utf8(&buf[..content.len()]), Ok(content));
//             assert_eq!(check!(read.seek(SeekFrom::Current(0))), 14);
//             assert_eq!(check!(read.seek(SeekFrom::End(-5))), 9);
//             assert_eq!(check!(read.seek_read(&mut buf, 0)), content.len());
//             assert_eq!(str::from_utf8(&buf[..content.len()]), Ok(content));
//             assert_eq!(check!(read.seek(SeekFrom::Current(0))), 14);
//             assert_eq!(check!(read.seek(SeekFrom::End(-5))), 9);
//             assert_eq!(check!(read.read(&mut buf)), write3.len());
//             assert_eq!(str::from_utf8(&buf[..write3.len()]), Ok(write3));
//             assert_eq!(check!(read.seek(SeekFrom::Current(0))), 14);
//             assert_eq!(check!(read.seek_read(&mut buf, 0)), content.len());
//             assert_eq!(str::from_utf8(&buf[..content.len()]), Ok(content));
//             assert_eq!(check!(read.seek(SeekFrom::Current(0))), 14);
//             assert_eq!(check!(read.seek_read(&mut buf, 14)), 0);
//             assert_eq!(check!(read.seek_read(&mut buf, 15)), 0);
//         }
//         check!(fs::remove_file(&filename));
//     }

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

//     #[test]
//     fn write_then_read() {
//         let mut bytes = [0; 1024];
//         StdRng::from_entropy().fill_bytes(&mut bytes);

//         let tmpdir = tmpdir();

//         check!(fs::write(&tmpdir.join("test"), &bytes[..]));
//         let v = check!(fs::read(&tmpdir.join("test")));
//         assert!(v == &bytes[..]);

//         check!(fs::write(&tmpdir.join("not-utf8"), &[0xFF]));
//         error_contains!(
//             fs::read_to_string(&tmpdir.join("not-utf8")),
//             "stream did not contain valid UTF-8"
//         );

//         let s = "𐁁𐀓𐀠𐀴𐀍";
//         check!(fs::write(&tmpdir.join("utf8"), s.as_bytes()));
//         let string = check!(fs::read_to_string(&tmpdir.join("utf8")));
//         assert_eq!(string, s);
//     }

//     #[test]
//     fn file_try_clone() {
//         let tmpdir = tmpdir();

//         let mut f1 = check!(OpenOptions::new()
//             .read(true)
//             .write(true)
//             .create(true)
//             .open(&tmpdir.join("test")));
//         let mut f2 = check!(f1.try_clone());

//         check!(f1.write_all(b"hello world"));
//         check!(f1.seek(SeekFrom::Start(2)));

//         let mut buf = vec![];
//         check!(f2.read_to_end(&mut buf));
//         assert_eq!(buf, b"llo world");
//         drop(f2);

//         check!(f1.write_all(b"!"));
//     }
// }
