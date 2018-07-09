/// macro used to spawn a coroutine
///
/// this macro is just a convenient wrapper for [`spawn`].
/// However the supplied coroutine block is not wrapped in `unsafe` block
///
/// [`spawn`]: coroutine/fn.spawn.html
#[macro_export]
macro_rules! go {
    // for free spawn
    ($func:expr) => {{
        fn _go_check<F, T>(f: F) -> F
        where
            F: FnOnce() -> T + Send + 'static,
            T: Send + 'static,
        {
            f
        }
        let f = _go_check($func);
        unsafe { $crate::coroutine::spawn(f) }
    }};

    // for builder/scope spawn
    ($builder:expr, $func:expr) => {{
        fn _go_check<F, T>(f: F) -> F
        where
            F: FnOnce() -> T + Send,
            T: Send,
        {
            f
        }
        let f = _go_check($func);
        unsafe { $builder.spawn(f) }
    }};

    // for cqueue add spawn
    ($cqueue:expr, $token:expr, $func:expr) => {{
        fn _go_check<F, T>(f: F) -> F
        where
            F: FnOnce($crate::cqueue::EventSender) -> T + Send,
            T: Send,
        {
            f
        }
        let f = _go_check($func);
        unsafe { $cqueue.add($token, f) }
    }};
}

/// macro used to create the select coroutine
/// that will run in a infinite loop, and generate
/// as many events as possible
#[macro_export]
macro_rules! cqueue_add {
    ($cqueue:ident, $token:expr, $name:pat = $top:expr => $bottom:expr) => {{
        go!($cqueue, $token, |es| loop {
            let $name = $top;
            es.send(es.get_token());
            $bottom
        })
    }};
}

/// macro used to create the select coroutine
/// that will run only once, thus generate only one event
#[macro_export]
macro_rules! cqueue_add_oneshot {
    ($cqueue:ident, $token:expr, $name:pat = $top:expr => $bottom:expr) => {{
        go!($cqueue, $token, |es| {
            let $name = $top;
            es.send(es.get_token());
            $bottom
        })
    }};
}

/// macro used to select for only one event
/// it will return the index of which event happens first
#[macro_export]
macro_rules! select {
    (
        $($name:pat = $top:expr => $bottom:expr),+
    ) => ({
        use $crate::cqueue;
        cqueue::scope(|cqueue| {
            let mut _token = 0;
            $(
                cqueue_add_oneshot!(cqueue, _token, $name = $top => $bottom);
                _token += 1;
            )+
            match cqueue.poll(None) {
                Ok(ev) => return ev.token,
                _ => unreachable!("select error"),
            }
        })
    })
}

/// macro used to join all scoped sub coroutines
#[macro_export]
macro_rules! join {
    (
        $($body:expr),+
    ) => ({
        use $crate::coroutine;
        coroutine::scope(|s| {
            $(
                go!(s, || $body);
            )+
        })
    })
}

/// A macro to create a `static` of type `LocalKey`
///
/// This macro is intentionally similar to the `thread_local!`, and creates a
/// `static` which has a `with` method to access the data on a coroutine.
///
/// The data associated with each coroutine local is per-coroutine,
/// so different coroutines will contain different values.
#[macro_export]
macro_rules! coroutine_local {
    (static $NAME:ident : $t:ty = $e:expr) => {
        static $NAME: $crate::LocalKey<$t> = {
            fn __init() -> $t {
                $e
            }
            fn __key() -> ::std::any::TypeId {
                struct __A;
                ::std::any::TypeId::of::<__A>()
            }
            $crate::LocalKey {
                __init: __init,
                __key: __key,
            }
        };
    };
}
