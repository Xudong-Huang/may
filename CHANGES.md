## v0.3.13

* update scheduler, merge io workers and normal workers


## v0.3.5

`MAY` 0.3.5 use crossbeam work stealing deque for scheduler

* remove clippy warnings
* optimize some code
* fix cancel tricky bug
* add syn_flag primitive
* scheduler update support work stealing (#49)

## v0.3.0

`MAY` 0.3.0 improve performance and add some new features

* fix bugs
* add https server example
* add websocket server example
* replace net2 dependency with socket2 (#21)
* improve performance for getting scheduler instance
* support CoIo as a generic wrapper for normal io object (#22)
* support unix socket (#17)


## v0.2.0

`MAY` 0.2.0 focus on changing the `spawn` APIs to unsafe so that apply rust safety rules. And this is a breaking change to `v0.1.0`

* all the `spawn` APs are declared with `unsafe` for `TLS` access and stack exceed limitations 
* add `go!` macro for ease of use 'spawn' APIs to avoid writing `unsafe` block explicitly
* add simple http server example
* remove `unsafe` code for `MAY` configuration
* improve documentation

## v0.1.0

`MAY` 0.1.0 is the first public release.

* This release supports x86_64 Linux/MacOs/Windows platforms.
* Both stable and night rust compilation are supported.
