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