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

    /// convert to inner data
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

impl<T> From<Error<T>> for io::Error {
    fn from(err: Error<T>) -> Self {
        err.err
    }
}

impl<T> error::Error for Error<T> {
    fn cause(&self) -> Option<&dyn error::Error> {
        self.err.source()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::error::Error as StdError;
    use std::io::{Error as IoError, ErrorKind};

    #[test]
    fn test_error_new() {
        let io_err = IoError::new(ErrorKind::NotFound, "file not found");
        let data = "test data";
        let error = Error::new(io_err, data);

        // Verify the error was created correctly
        assert_eq!(error.into_data(), "test data");
    }

    #[test]
    fn test_error_into_data() {
        let io_err = IoError::new(ErrorKind::PermissionDenied, "access denied");
        let data = vec![1, 2, 3, 4, 5];
        let error = Error::new(io_err, data.clone());

        // Test that we can extract the original data
        let extracted_data = error.into_data();
        assert_eq!(extracted_data, vec![1, 2, 3, 4, 5]);
    }

    #[test]
    fn test_error_display() {
        let io_err = IoError::new(ErrorKind::ConnectionRefused, "connection refused");
        let data = "network data";
        let error = Error::new(io_err, data);

        // Test Display trait implementation
        let display_str = format!("{error}");
        assert!(display_str.contains("connection refused"));
    }

    #[test]
    fn test_error_debug() {
        let io_err = IoError::new(ErrorKind::TimedOut, "operation timed out");
        let data = 42u32;
        let error = Error::new(io_err, data);

        // Test Debug trait implementation
        let debug_str = format!("{error:?}");
        assert!(debug_str.contains("operation timed out"));
    }

    #[test]
    fn test_error_from_conversion() {
        let io_err = IoError::new(ErrorKind::Interrupted, "operation interrupted");
        let data = "some data";
        let error = Error::new(io_err, data);

        // Test conversion from Error<T> to io::Error
        let converted: IoError = error.into();
        assert_eq!(converted.kind(), ErrorKind::Interrupted);
        assert!(converted.to_string().contains("operation interrupted"));
    }

    #[test]
    fn test_error_std_error_trait() {
        let io_err = IoError::new(ErrorKind::InvalidData, "invalid data format");
        let data = "corrupted data";
        let error = Error::new(io_err, data);

        // Test that it implements std::error::Error
        let std_error: &dyn StdError = &error;

        // Test cause/source method
        let _cause = std_error.source();
        // Just verify it doesn't panic
    }

    #[test]
    fn test_error_with_different_data_types() {
        // Test with String data
        let io_err = IoError::other("generic error");
        let string_data = String::from("string data");
        let string_error = Error::new(io_err, string_data.clone());
        assert_eq!(string_error.into_data(), string_data);

        // Test with numeric data
        let io_err = IoError::other("numeric error");
        let numeric_data = 123i64;
        let numeric_error = Error::new(io_err, numeric_data);
        assert_eq!(numeric_error.into_data(), 123i64);

        // Test with custom struct
        #[derive(Debug, PartialEq, Clone)]
        struct CustomData {
            field1: String,
            field2: u32,
        }

        let custom_data = CustomData {
            field1: "test".to_string(),
            field2: 456,
        };
        let io_err = IoError::other("custom error");
        let custom_error = Error::new(io_err, custom_data.clone());
        assert_eq!(custom_error.into_data(), custom_data);
    }

    #[test]
    fn test_error_chain() {
        // Test error chaining behavior
        let io_err = IoError::new(ErrorKind::BrokenPipe, "broken pipe");
        let data = "pipe data";
        let error = Error::new(io_err, data);

        // Convert to io::Error and verify the chain is preserved
        let converted_error: IoError = error.into();
        assert_eq!(converted_error.kind(), ErrorKind::BrokenPipe);
        assert!(converted_error.to_string().contains("broken pipe"));
    }
}
