//! CoIo creation error
use std::{error, fmt, io};

/// CoIo creation error type
pub struct Error<T> {
    err: io::Error,
    data: T,
}

impl<T> Error<T> {
    /// create error from io::Error and data
    pub fn new(err: io::Error, data: T) -> Error<T> {
        Error { err, data }
    }

    /// convert to inneral data
    pub fn into_data(self) -> T {
        self.data
    }
}

impl<T> fmt::Display for Error<T> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        self.err.fmt(f)
    }
}

impl<T> fmt::Debug for Error<T> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        self.err.fmt(f)
    }
}

impl<T> Into<io::Error> for Error<T> {
    fn into(self) -> io::Error {
        self.err
    }
}

impl<T> error::Error for Error<T> {
    fn description(&self) -> &str {
        self.err.description()
    }

    fn cause(&self) -> Option<&error::Error> {
        self.err.cause()
    }
}
