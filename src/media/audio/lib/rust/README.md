# `fuchsia-audio`

`fuchsia-audio` is a library for interacting with Fuchsia audio devices.

## Building

This project should be automatically included in builds.

## Using

`fuchsia-audio` can be used by depending on the
`//src/media/audio/lib/rust` GN target and then using
the `fuchsia_audio` crate in a Rust project.

`fuchsia-audio` is not available in the SDK.

## Testing

Unit tests for `fuchsia-audio` are available in the
`fuchsia_audio_tests` package:

```
$ fx test fuchsia_audio_tests
```

You'll need to include `//src/media/audio/lib/rust:tests` in your
build, either by using `fx args` to put it under `universe_package_labels`, or
by `fx set [....] --with //src/media/audio/lib/rust:tests`.
