use std::time::Duration;

fn main() {
    may::go!(|| {
        let (sx1, rx1) = may::sync::mpsc::channel();
        let (sx2, rx2) = may::sync::mpsc::channel();
        sx1.send(100i32).unwrap();
        sx2.send(200i32).unwrap();

        may::loop_select!(
            v1 = rx1.recv() => println!("v1={:?}", v1),
            v2 = rx2.recv() => println!("v2={:?}", v2)
        );
    });
    std::thread::sleep(Duration::from_secs(1));
}
