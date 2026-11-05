#[macro_use]
extern crate may;

use may::coroutine;

fn colliding_key() -> std::any::TypeId {
    std::any::TypeId::of::<()>()
}

fn init_i32() -> i32 {
    1
}

fn init_u32() -> u32 {
    2
}

static COLLIDING_I32: may::LocalKey<i32> = may::LocalKey {
    __key: colliding_key,
    __init: init_i32,
};
static COLLIDING_U32: may::LocalKey<u32> = may::LocalKey {
    __key: colliding_key,
    __init: init_u32,
};

#[test]
fn local_in_thread() {
    coroutine_local!(static FOO: i32 = 3);

    // can only be called in coroutine context
    FOO.with(|f| {
        assert_eq!(*f, 3);
    });
}

#[test]
fn mismatched_local_key_types_panic() {
    COLLIDING_I32.with(|value| assert_eq!(*value, 1));
    assert!(std::panic::catch_unwind(|| COLLIDING_U32.with(|_| ())).is_err());
}

#[test]
fn coroutine_local() {
    fn square(i: i32) -> i32 {
        i * i
    }
    coroutine_local!(static FOO: i32 = square(3));

    go!(|| {
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
            go!(scope, move || {
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
