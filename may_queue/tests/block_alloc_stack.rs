//! Regression: neither block allocation nor queue teardown may
//! materialise BLOCK_SIZE payloads on the calling stack. A 4 KiB payload
//! makes each block ~0.5 MiB; the 128 KiB thread below overflows under
//! the old `Box::new(BlockNode { .. })` / `bulk_pop()`-draining `Drop`,
//! and passes with off-stack allocation and item-wise draining. The
//! budget is held tight enough that debug-mode per-item moves still fit
//! but a single on-stack block (or inline SmallVec) cannot.

#[derive(Clone)]
struct Big(#[allow(dead_code)] [u8; 4096]);

fn hammer<Q>(push: impl Fn(&Q, Big), pop: impl Fn(&Q) -> Option<Big>, q: Q)
where
    Q: Send + 'static,
{
    // Cross several BLOCK_SIZE boundaries, then drop `q` non-empty so the
    // Drop path is exercised with items still queued.
    for round in 0..5 {
        for _ in 0..200 {
            push(&q, Big([round as u8; 4096]));
        }
        for _ in 0..150 {
            assert!(pop(&q).is_some(), "queue lost items");
        }
    }
}

fn on_small_stack(name: &str, f: impl FnOnce() + Send + 'static) {
    std::thread::Builder::new()
        .name(name.to_string())
        .stack_size(128 * 1024)
        .spawn(f)
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn mpsc_blocks_allocate_off_stack() {
    on_small_stack("mpsc-small-stack", || {
        let q = may_queue::mpsc::Queue::new();
        hammer(|q, v| q.push(v), |q| q.pop(), q);
    });
}

#[test]
fn spsc_blocks_allocate_off_stack() {
    on_small_stack("spsc-small-stack", || {
        let q = may_queue::spsc::Queue::new();
        hammer(|q, v| q.push(v), |q| q.pop(), q);
    });
}
