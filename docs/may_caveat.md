Though [MAY][may] is easy to use, but it does have some restrictions. And these restrictions are hard to remove. You must know them very well before writing any user coroutine code.

If you are not aware of them, your application will easily lost performance or even trigger undefined behaviors.

I will list 4 rules below

## Don't call thread blocking APIs
This is obvious. Calling thread block version APIs in coroutine would halt the worker thread and can't schedule other ready coroutines. It will hurt the performance. 

Those block version APIs includes:
* io block operation such as socket `read()`/`write()`
* sync primitive APIs such as `Mutex`/`Condvar`
* thread related APIs such as `sleep()`/`yield_now()`
* and functions that call those block version api internally, such as third party libraries


The solution is calling [MAY][may] API instead. And port necessary dependency libraries to May compatible version.

## Don't use Thread Local Storage
Access TLS in coroutine would trigger undefined behavior and it will be hard to debug the issue.

There is a [post](cls) already cover this topic. And the solution is using Coroutine Local Storage instead.

But if you are depending on a third party function that uses TLS you likely get hurt sooner or later. There is an [issue][issue] that discuss this a bit.

Currently calling `libstd` APIs is safe in  [MAY][may] coroutines.

## Don't run CPU bound tasks for long time
[MAY][may] scheduler runs coroutines cooperatively which means if a running coroutine doesn't yield out, it will occupy the running thread and other coroutines will not be scheduled on that thread.

[MAY][may] APIs will automatically yield out if necessary, so this is not a problem. But if you are running a long time CPU bound task in coroutine, you'd better call `coroutine::yield_now()` manually at appropriate point.


## Don't exceed the stack 
[MAY][may] doesn't support automatic stack increasing. Each coroutine alloc a limited stack size for its own. If the coroutine exceeds it's stack size, it will trigger undefined behavior.

So that you should avoid calling recursive functions in coroutine. Recursive function calls would easily exhaust your stack.

And you also should avoid calling functions that internally use a big stack space like `std::io::copy()`.

`may::config()::set_stack_size()` can be used to set the default stack size for all coroutines. And you can use `may::coroutine::Builder` to specify a single coroutine stack size.

```rust
Builder::new().stack_size(0x2000).spawn(...)
```

For how to tune the coroutine stack size, please ref [this][stack_size]

## Summary
Develop a coroutine library in rust is not an easy task. [MAY][may] can't prevent users doing wrong things. So you should know those caveats clearly before using the library. I hope those restrictions will not prevent you from using the library 🙂

<!--refs-->
[may]:https://github.com/Xudong-Huang/may
[cls]:https://blog.zhpass.com/2017/12/18/CLS/
[issue]:https://github.com/Xudong-Huang/may/issues/6
[stack_size]:tune_stack_size.md