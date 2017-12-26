Visit TLS variables in coroutine context is not safe. It has the same effect of access global variables without protection in multi thread environment. Because coroutines could run on any thread that the scheduler assign with it at run time, and each time a coroutine is scheduled again the running thread may be a different one.

But only read TLS from coroutines context is some sort of "safe" for that it doesn't change any thing. It may get outdated value when it scheduled to a different thread, where the TLS value is not the same as the former thread.

If you really need a local storage variable usage in the coroutine context, then use the coroutine local storage(CLS) instead. the CLS guarantees that each coroutine has its own unique local storage. Beside, if you access a CLS in the thread context then it will fall back to its TLS storage according to the given key. So access CLS from thread context is totally safe.

In rust declare a TLS is through using the `thread_local` macro, where declare a CLS variable is through using `coroutine_local` macro, so what you need to do is just replacing the `thread_local` word with `coroutine_local`. Below is a simple example that shows how it works.

```rust
fn coroutine_local_many() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    coroutine_local!(static FOO: AtomicUsize = AtomicUsize::new(0));

    coroutine::scope(|scope| {
        for i in 0..10 {
            scope.spawn(move || {
                FOO.with(|f| {
                    assert_eq!(f.load(Ordering::Relaxed), 0);
                    f.store(i, Ordering::Relaxed);
                    assert_eq!(f.load(Ordering::Relaxed), i);
                });
            });
        }
    });
    // called in thread
    FOO.with(|f| {
        assert_eq!(f.load(Ordering::Relaxed), 0);
    });
}
```

