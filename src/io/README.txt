I/O event loop implementations

Linux:
  - epoll: readiness based (sys/unix, selected without the io_uring feature)
  - io_uring: completion based (sys/linux_uring, selected with the opt-in
    io_uring feature)

macOS/BSD: kqueue (sys/unix)
Windows: IOCP (sys/windows)
