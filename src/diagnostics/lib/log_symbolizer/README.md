# `log_symbolizer`

`log_symbolizer` utility library for symbolizing logs.

## Building

This project should be automatically included in builds.

## Using

`log_symbolizer` is consumed from ffx log.

## Testing

unit tests for `log_symbolizer` are available in the
`log_symbolizer-test` package:

```
$ fx test log_symbolizer-test
```

You'll need to include `//src/diagnostics/lib/log_symbolizer:tests` in your
build, either by using `fx args` to put it under `universe_package_labels`, or
by `fx set [....] --with //src/diagnostics/lib/log_symbolizer:tests`.
