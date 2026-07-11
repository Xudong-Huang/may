# Fix: Map WSAECONNREFUSED (10061) to ConnectionRefused on Windows

**Branch:** `fix/wsa-connrefused-mapping`
**Base:** `master`
**Commits:** 1

## Summary — context

During the **may_minihttp native HTTP client hardening** (PRD: `docs/PRD-http-client-hardening.md`), integration tests for the new `may_minihttp` client were run on a Windows CI matrix. Three tests failed:

- `test_connection_refused`
- `test_connection_timeout`
- `test_connection_close_by_server`

All three assert `io::ErrorKind::ConnectionRefused` after a refused connection. They pass on Linux because Rust's std correctly maps `ECONNREFUSED` (111). On Windows, `may::net::TcpStreamConnect` returns `io::ErrorKind::Uncategorized` for `WSAECONNREFUSED` (10061), so the assertions fail.

This PR fixes the root cause in the `may` crate's Windows I/O layer, which `may_minihttp` depends on for TCP connectivity.

## Root cause

Rust's std library does not map `WSAECONNREFUSED` (10061) to `io::ErrorKind::ConnectionRefused` on Windows — it falls through to `Uncategorized`. The `connect_complete()` wrapper in `may/src/io/sys/windows/miow.rs` passes the raw WSA error up unmodified.

On Linux the epoll-based path uses `SO_ERROR` and Rust's std correctly maps `ECONNREFUSED` (111).

## Changes

| File | Change |
|------|--------|
| `src/io/sys/windows/miow.rs` | Remap WSA 10061 to `ConnectionRefused` in `connect_complete()` (+9 lines) |
| `docs/postmortem_wsaconnrefused.md` | Full postmortem analysis (+104 lines) |

## Testing

The 3 previously-failing may_minihttp integration tests (`test_connection_refused`, `test_connection_timeout`, `test_connection_close_by_server`) now pass on Windows. All 20 client integration tests pass on Linux.

## Prevention

1. Add `map_wsa_errors(io::Error) -> io::Error` helper in the Windows I/O module so other overlapped operations (accept, etc.) don't repeat this gap.
2. CI Windows matrix must cover error-condition tests, not just happy-path connectivity.

## Related

- Upstream fix for may_minihttp: `PR_fix-client-flush-pipelining.md` (missing flush after `write_head_impl` causing pipelining header corruption)
- PRD that discovered both issues: `may_minihttp/docs/PRD-http-client-hardening.md`
