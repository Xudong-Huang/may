#[macro_use]
extern crate coroutine;


#[test]
#[should_panic]
fn panic_not_coroutine() {
    coroutine_local!(static FOO: i32 = 3);

    // can only be called in coroutine context
    FOO.with(|f| {
        assert_eq!(*f, 3);
    });
}

#[test]
fn coroutine_local() {
    fn square(i: i32) -> i32 {
        i * i
    }
    coroutine_local!(static FOO: i32 = square(3));

    coroutine::spawn(|| {
            FOO.with(|f| {
                assert_eq!(*f, 9);
            });
        })
        .join()
        .unwrap();
}

#[test]
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
}
