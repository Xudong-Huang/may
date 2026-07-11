# Postmortem: Windows WSAECONNREFUSED Misclassification

## Summary

On Windows, TCP connection attempts that receive `WSAECONNREFUSED` (10061) return an `io::Error` with `ErrorKind::Uncategorized` instead of `ErrorKind::ConnectionRefused`. This causes three integration tests to fail on Windows while passing on Linux:

- `test_connection_refused`
- `test_connection_timeout`
- `test_connection_close_by_server`

All three tests assert `io::ErrorKind::ConnectionRefused` after a refused connection. On Windows they get `Uncategorized` instead, so the assertions fail.

## Timeline

- Bug exists since the initial implementation of `may::net::TcpStreamConnect` on Windows
- Discovered during CI Windows matrix testing on the `feat/native-http-client` branch
- Not caught earlier because all prior CI runs were Linux-only

## Impact

| Aspect | Detail |
|--------|--------|
| **Affected platform** | Windows only |
| **Affected component** | `may` crate — `may::net::TcpStreamConnect` |
| **Affected tests** | 3 integration tests in `client_integration.rs` |
| **Production risk** | High — any application logic that branches on `ErrorKind::ConnectionRefused` will take the wrong path on Windows. This includes connection retry logic, circuit breakers, fallback routing, and error logging |
| **Severity** | P1 — silent misclassification of a fundamental TCP error |

## Root Cause

The Windows overlapped connect path in `may/src/io/sys/windows/miow.rs:578-594`:

```rust
pub fn connect_complete(socket: RawSocket) -> io::Result<()> {
    const SO_UPDATE_CONNECT_CONTEXT: i32 = 0x7010;
    let result = unsafe {
        setsockopt(
            socket as SOCKET,
            SOL_SOCKET as _,
            SO_UPDATE_CONNECT_CONTEXT,
            std::ptr::null_mut(),
            0,
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}
```

When `setsockopt(SO_UPDATE_CONNECT_CONTEXT)` fails after an overlapped `ConnectEx` call completes with an error, `io::Error::last_os_error()` retrieves the last Win32 error. The Rust standard library's mapping for WSA error 10061 (`WSAECONNREFUSED`) does not include a translation to `io::ErrorKind::ConnectionRefused` — it falls through to the catch-all `Uncategorized`.

On Linux, the epoll-based connect path uses `check_connected()` which reads `SO_ERROR` and Rust's std correctly maps `ECONNREFUSED` to `io::ErrorKind::ConnectionRefused`.

The difference: Linux `ECONNREFUSED` (111) is part of Rust's standard errno-to-ErrorKind mapping. Windows WSA error codes are an extension beyond the POSIX errno namespace, and Rust's std only maps a subset of them. `WSAECONNREFUSED` (10061) is not among the mapped codes.

## Fix

Add explicit WSA error code checking in `connect_complete()` (and any other Windows overlapped operation that can return connection-related errors) before calling `io::Error::last_os_error()`:

**File:** `may/src/io/sys/windows/miow.rs`, function `connect_complete()`

```rust
pub fn connect_complete(socket: RawSocket) -> io::Result<()> {
    const SO_UPDATE_CONNECT_CONTEXT: i32 = 0x7010;
    let result = unsafe {
        setsockopt(
            socket as SOCKET,
            SOL_SOCKET as _,
            SO_UPDATE_CONNECT_CONTEXT,
            std::ptr::null_mut(),
            0,
        )
    };
    if result == 0 {
        Ok(())
    } else {
        let err = io::Error::last_os_error();
        // WSAECONNREFUSED (10061) is not mapped to ConnectionRefused by
        // Rust's std::io::ErrorKind on Windows — remap it explicitly.
        if err.raw_os_error() == Some(10061) {
            Err(io::Error::new(io::ErrorKind::ConnectionRefused, err))
        } else {
            Err(err)
        }
    }
}
```

This should be extended to other overlapped operations that can produce connection-refused errors: `accept_complete()` (WSAECONNABORTED/WSAECONNRESET may also need mapping), and any other I/O completion routine that converts raw OS errors.

## Prevention

1. Add a CI Windows matrix for all integration tests that test error conditions, not just happy-path connectivity
2. Create a helper function `map_wsa_errors(io::Error) -> io::Error` in the Windows I/O module that centralizes WSA-to-ErrorKind mapping
3. Consider filing a Rust std library issue — `WSAECONNREFUSED` should absolutely be mapped to `ConnectionRefused` in the standard library. Until it is, each Windows I/O module needs to handle this explicitly

## Lessons

- Rust's std library does not fully map Windows WSA error codes to `io::ErrorKind` variants. Any crate using overlapped I/O on Windows must handle this explicitly.
- Platform-specific I/O layers should have a shared error-mapping utility to avoid missing codes in one module while another handles it correctly.
- Linux CI parity alone is insufficient for cross-platform network code.
