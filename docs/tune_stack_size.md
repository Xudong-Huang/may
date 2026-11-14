Because [MAY][may] doesn't support automatic stack increasing, you have to determine the stack size of the coroutine before running your application. Read [this][caveat] for more information.

When creating coroutines in [MAY][may], the library would alloc a chunk of memory from heap as the stack for each coroutine. The default stack size is 8k words, in 64bit system it is 64k bytes. For most of the cases, this stack size is big enough for simple coroutines. Buf if you have a very complicated coroutine that need more stack size, or you want to shrink the stack size to save some memory usage, you can set the stack size explicitly to a reasonable value.

Keep in mind that cancelling a coroutine is delivered by unwinding a `Cancel` panic, and the unwinder itself needs stack. On some platforms (notably macOS arm64 in release builds) that can take tens of kilobytes. The default leaves room for cancelling a simple coroutine; if you shrink the stack, leave headroom for coroutines that may be cancelled.

## Change default coroutine stack size
You can config the default stack size at the initialization stage. when create a coroutine and you not specify the stack size for it, it will use this default stack size.

The unit is by `word`. the flowing code would set the default stack size as 8k bytes.

```rust
may::config().set_stack_size(0x400);
// this coroutine would use 8K bytes stack
go!(...);
```

## Set stack size for a single coroutine
You can use the coroutine `Builder` to specify a stack size for the new spawned coroutine. This would ignore the global default stack size.

The following code would spawn a coroutine with 16k bytes stack

```rust
// this coroutine would use 16K bytes stack
let builder = may::coroutine::Builder::new().stack_size(0x800);
unsafe { builder.spawn(...) }.unwrap();
```

## Get the coroutine stack usage
If you need to know the exact stack usage number for your coroutine, you can set the  stack size to an odd number. If the passed in stack size is an odd number, [MAY][may] would initialize the whole stack for the coroutine with a special pattern data, thus during the programme executing we can detect the **footprint** of the stack, after the coroutine is finished, [MAY][may] would print out the actual usage.

For example the blow code
```rust
extern crate may;
use std::io::{self, Read};

fn main() {
    go!(
        may::coroutine::Builder::new()
            .name("test".to_owned())
            .stack_size(0x1000 - 1),
        || {
            println!("hello may");
        }
    ).unwrap();

    println!("Press any key to continue...");
    let _ = io::stdin().read(&mut [0u8]).unwrap();
}
```

would give output like this

```sh
hello may
coroutine name = Some("test"), stack size = 4095,  used size = 266
```





<!--refs-->
[may]:https://github.com/Xudong-Huang/may
[caveat]:may_caveat.md
