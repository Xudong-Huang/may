use std::time::Duration;

fn main() {
    may::go!(|| {
        // `loop_select!` re-runs the branch closures and borrows the
        // receivers, so use the multi-consumer channel (`mpsc::Receiver` is
        // `!Sync`).
        let (sx1, rx1) = may::sync::mpmc::channel();
        let (sx2, rx2) = may::sync::mpmc::channel();
        sx1.send(100i32).unwrap();
        sx2.send(200i32).unwrap();

        may::loop_select!(
            v1 = rx1.recv() => println!("v1={:?}", v1),
            v2 = rx2.recv() => println!("v2={:?}", v2)
        );
    });
    std::thread::sleep(Duration::from_secs(1));
}
