## v0.3.51

* version bump only

## v0.3.50

* fix unix read paths for the newer `nix` `AsFd` API
* fix a wrong struct offset on windows
* fix clippy warnings and formatting

## v0.3.49

* log an error instead of panicking when the event loop select fails
* remove the mutex from `CondVar` and use a `VecDeque` for its wait list
* mark `init_scheduler` as cold

## v0.3.48

* remove unsafe code from the epoll, kqueue, and IOCP implementations (#112)
* add `sync::WaitGroup` ported from `crossbeam-utils` (#103)
* add `sync::Barrier` ported from the standard library (#103)
* remove the unsafe `AtomicOption::unsync_store` API
* panic on event loop select errors
* refine the kqueue select implementation
* make the selector and several internal structs crate-private
* add a benchmark comparison against crossbeam queues

## v0.3.47

* add `Config::set_timeout_ns` and `Config::set_worker_pin` options
* add `spawn_unsafe_builder` for scoped coroutines
* rework the work-stealing scheduler, limiting stealing to at most 3 workers
* add the `rand_work_steal` and `crossbeam_queue_steal` features
* fix timer and duration conversions for kqueue and the timeout list
* use `Box<Thread>` in the spsc blocker
* update `windows-sys` to 0.59
* fix an unsoundness in `may_queue` (#110)
* remove the nightly-only `thread_local` implementation
* bump `may_queue` to 0.1.23

## v0.3.46

* refine coroutine cancellation and clear the pending IO cancel when the coroutine is resumed
* store the IO flag as an `AtomicUsize`
* make `AtomicOption` const-constructible
* remove the `?Sized` bound from the `Mutex` `Default` implementation
* update dependencies and fix nightly/no-default-features builds

## v0.3.45

* update `generator` to 0.8
* enable `SO_REUSEPORT` on unix TCP listeners

## v0.3.44

* port the Windows overlapped IO implementation from `miow`, including IOCP, pipes, UDP, accept, connect, and socket read/write/peek
* fix IO cancellation on Windows
* fix the mpsc queue length calculation
* bump `may_queue` to 0.1.22

## v0.3.43

* update `generator` to 0.7.6 and fix `yield_from_gen`
* add the `loop_select!` macro (#102)
* add `peek` for TCP and unix streams (#106)
* refine the unsafe scope of the `select!` macros (#105)
* update `nix` to 0.27
* use stable `likely`/`unlikely`
* document aarch64 macOS support (#104)

## v0.3.42

* allow selecting a scheduler thread with `Builder::id`
* refine `Mutex::try_lock`
* update `socket2` to 0.5
* fix documentation

## v0.3.41

* add the `inner_cache` feature to `may_queue`'s spsc queue
* run IO coroutines directly when work stealing is disabled
* schedule spawned coroutines on the current scheduler
* remove `Clone` from the spsc sender
* fix `io_timeout` for kqueue
* fix the parked coroutine store

## v0.3.40

* use an spsc queue when the `work_steal` feature is disabled

## v0.3.39

* add the `work_steal` feature, enabled by default
* avoid stealing from the current worker's own queue
* refine running of queued tasks

## v0.3.38

* remove unused queue implementations
* use `spin_loop` instead of `backoff`
* use `compare_exchange_weak` for bulk pop
* update `core_affinity` to 0.8
* implement `RefUnwindSafe` for the mpsc queue
* bump `may_queue` to 0.1.19

## v0.3.37

* fix `is_empty` for the unordered mpsc queue
* use delay drop for spsc and update the spsc blocker
* don't keep a minimum number of items when stealing

## v0.3.36

* add an unordered mpsc queue
* add a `clear` API for `AtomicOption`
* implement `AsIoData` for `SplitIo`
* reduce `swap` calls and don't steal all available items
* depend on `crossbeam-utils` instead of `crossbeam`
* bump `may_queue` to 0.1.16

## v0.3.35

* add a fast blocking path and optimize spsc without timeouts
* add fast bulk pop for mpsc
* make `spmc::len()` unsafe
* use safe block index calculations
* add ABA checks for spmc local pop

## v0.3.34

* add an mpsc queue with bulk pop and peek
* use the mpsc queue for the scheduler and remove seg queue usage
* change the default coroutine pool size to 1000
* refine the scheduler and spmc ordering
* bump `may_queue` to 0.1.13

## v0.3.33

* fix ABA problems in the spmc queue
* refine spmc `len`, `drop`, and bulk pop
* enqueue IO tasks through the scheduler
* remove the 64-worker cap from the default worker count
* bump `may_queue` to 0.1.12

## v0.3.32

* add an spmc queue with `Local`/`Steal` and bulk pop
* use spmc `Local`/`Steal` queues for the scheduler
* use `AtomicCell` for `AtomicOption`
* reduce eventfd reads in the epoll event loop
* bump `may_queue` to 0.1.11

## v0.3.31

* refine the spsc queue and its blocking behavior
* bump `may_queue` to 0.1.10

## v0.3.30

* move the queue implementations into `may_queue`
* add more spsc/mpsc queue implementations
* use an mpsc seg queue for the scheduler
* use bulk pop to free unused IO objects
* remove the IO slot

## v0.3.29

* pin worker threads to CPU cores
* refine `wait_io` with `yield_with_io`
* refine epoll deregistration
* refine global queue collection

## v0.3.28

* remove the 64-thread restriction
* keep running when an epoll select fails
* don't set `SO_REUSEPORT` when binding TCP listeners
* don't use `EPOLL_CLOEXEC`

## v0.3.27

* use a tokio-style queue for scheduling
* increase the local queue size to 1024
* limit the IO schedule size
* use a `Cell<usize>` for the worker id

## v0.3.26

* refine the `spawn` implementation
* remove `ParkStatus`
* use crossbeam's `Injector` instead of `SegQueue`

## v0.3.25

* add the `io_cancel` feature gate (#100)
* add the `io_timeout` feature gate for Linux IO timeouts
* enable `wait_io` for threads
* add mutable access to the inner IO object
* remove nonblocking IO support in favor of inner IO handling
* implement `SplitIo` for unix streams and move it to `may::io`
* refine the scheduler, local run queue, and coroutine pool
* refine coroutine prefetch

## v0.3.24

* support splitting IO objects (#97)
* add `WaitIoWaker` and wakeup support for `WaitIo`
* expose `IoData`
* use crossbeam `SegQueue`
* fix the Windows split IO issue

## v0.3.23

* unify thread and coroutine IO handling (#96)
* fix an IO file descriptor leak (#99)
* fix the `yield_with_io` issue
* switch internal blocking primitives to `parking_lot`
* update `miow`
* move wakeup logs to trace level

## v0.3.22

* remove the panic filter now that `generator` handles it
* fix clippy warnings and refine code

## v0.3.21

* avoid triggering a panic while already panicking
* export `atomic_option::PointerType`
* fix typos

## v0.3.20

* fix the timer list bug
* limit the default number of workers to 64
* use `panic_any` for panic propagation
* enable GitHub Actions
* update `nix` to 0.21
* fix Windows compilation

## v0.3.19

* fix the queue send bound (#88)
* update `generator` to 0.7 and support aarch64
* make the generator return type `Send`
* use `min_specialization` instead of `specialization` on nightly
* use the `rustversion` crate
* update dependencies

## v0.3.18

* export `Coroutine`
* add cancellation logic for `wait_io`
* use `AtomicCell` instead of `AtomicOption` in some cases
* clear the cancel bit only on unix
* refine the `cqueue` example

## v0.3.17

* add `WaitIo` support for Linux
* refine mpsc atomic ordering

## v0.3.16

* fix the timer list install issue

## v0.3.15

* wake idle worker threads every second to look for work (#77)
* use a monotonic clock to calculate timeouts
* remove unused code and `extern crate` declarations
* update dependencies

## v0.3.14

* run coroutines directly in the timer thread
* remove the IO event loop schedule policy
* steal from the global queue last
* update the stack size debug output
* bump `may_queue` to 0.1.7

## v0.3.13

* update scheduler, merge io workers and normal workers


## v0.3.5

`MAY` 0.3.5 use crossbeam work stealing deque for scheduler

* remove clippy warnings
* optimize some code
* fix cancel tricky bug
* add syn_flag primitive
* scheduler update support work stealing (#49)

## v0.3.0

`MAY` 0.3.0 improve performance and add some new features

* fix bugs
* add https server example
* add websocket server example
* replace net2 dependency with socket2 (#21)
* improve performance for getting scheduler instance
* support CoIo as a generic wrapper for normal io object (#22)
* support unix socket (#17)


## v0.2.0

`MAY` 0.2.0 focus on changing the `spawn` APIs to unsafe so that apply rust safety rules. And this is a breaking change to `v0.1.0`

* all the `spawn` APs are declared with `unsafe` for `TLS` access and stack exceed limitations 
* add `go!` macro for ease of use 'spawn' APIs to avoid writing `unsafe` block explicitly
* add simple http server example
* remove `unsafe` code for `MAY` configuration
* improve documentation

## v0.1.0

`MAY` 0.1.0 is the first public release.

* This release supports x86_64 Linux/MacOs/Windows platforms.
* Both stable and night rust compilation are supported.
