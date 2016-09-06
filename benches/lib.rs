#![feature(test)]
extern crate coroutine;
extern crate test;

// use std::panic;
use coroutine::*;
use test::Bencher;

#[bench]
fn yield_bench(b: &mut Bencher) {
    // don't print any panic info
    // when cancel the generator
    // panic::set_hook(Box::new(|_| {}));
    b.iter(|| {
        let h = spawn(|| {
            for _i in 0..10000 {
                yield_now();
            }
        });

        h.join().unwrap();
    });
}


#[bench]
fn spawn_bench(b: &mut Bencher) {
    get_scheduler();
    b.iter(|| {
        let total_work = 1000;
        let threads = 2;
        let mut vec = Vec::with_capacity(threads);
        for _t in 0..threads {
            let j = std::thread::spawn(move || {
                scope(|scope| {
                    for _i in 0..total_work / threads {
                        scope.spawn(|| {
                            // yield_now();
                        });
                    }
                });
            });
            vec.push(j);
        }
        for j in vec {
            j.join().unwrap();
        }
    });
}

#[bench]
fn spawn_bench_1(b: &mut Bencher) {
    get_scheduler();
    b.iter(|| {
        let total_work = 1000;
        let threads = 2;
        let mut vec = Vec::with_capacity(threads);
        for _t in 0..threads {
            let work = total_work / threads;
            let j = std::thread::spawn(move || {
                let v = (0..work).map(|_| coroutine::spawn(|| {})).collect::<Vec<_>>();
                for h in v {
                    h.join().unwrap();
                }
            });
            vec.push(j);
        }
        for j in vec {
            j.join().unwrap();
        }
    });
}

#[bench]
fn smoke_bench(b: &mut Bencher) {
    b.iter(|| {
        let threads = 5;
        let mut vec = Vec::with_capacity(threads);
        for _t in 0..threads {
            let j = std::thread::spawn(|| {
                scope(|scope| {
                    for _i in 0..200 {
                        scope.spawn(|| {
                            for _j in 0..1000 {
                                yield_now();
                            }
                        });
                    }
                });
            });
            vec.push(j);
        }
        for j in vec {
            j.join().unwrap();
        }
    });
}

#[bench]
fn smoke_bench_1(b: &mut Bencher) {
    b.iter(|| {
        let threads = 5;
        let mut vec = Vec::with_capacity(threads);
        for _t in 0..threads {
            let j = std::thread::spawn(|| {
                scope(|scope| {
                    for _i in 0..2000 {
                        scope.spawn(|| {
                            for _j in 0..10 {
                                yield_now();
                            }
                        });
                    }
                });
            });
            vec.push(j);
        }
        for j in vec {
            j.join().unwrap();
        }
    });
}

#[bench]
fn smoke_bench_2(b: &mut Bencher) {
    b.iter(|| {
        scope(|s| {
            // create a main coroutine, let it spawn 10 sub coroutine
            for _ in 0..1000 {
                s.spawn(|| {
                    scope(|ss| {
                        for _ in 0..10 {
                            ss.spawn(|| {
                                // each task yield 10 times
                                for _ in 0..10 {
                                    yield_now();
                                }
                            });
                        }
                    });
                });
            }
        });
    });
}
