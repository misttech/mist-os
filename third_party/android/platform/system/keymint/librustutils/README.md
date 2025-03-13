The upstream KeyMint Rust reference implementation has a
[dependency](https://android.googlesource.com/platform/system/keymint/+/refs/heads/main/hal/src/env.rs)
on
[Android's librustutils](https://android.googlesource.com/platform/system/librustutils/+/refs/heads/main),
but only for retrieving a handful of system properties. The files here are
intended to stub these out and only these. This "librustutils" should not be
used outside of the KeyMint Rust reference implementation.

https://fxbug.dev/392914358 tracks what to do in the longer term, potentially
replacing src/hal/env.rs altogether.
