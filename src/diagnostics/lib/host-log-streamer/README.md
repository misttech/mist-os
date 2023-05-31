# `host-log-streamer`

`host-log-streamer` is a utility library for streaming logs using
the `fuchsia.diagnostics.host.ArchiveAccessor` protocol.

## Building

This project should be automatically included in builds.

## Using

`host-log-streamer` is consumed from ffx log and log_listener.

## Testing

unit tests for `host-log-streamer` are available in the
`host-log-streamer-test` package:

```
$ fx test host-log-streamer-test
```

You'll need to include `//src/diagnostics/lib/host-log-streamer:tests` in your
build, either by using `fx args` to put it under `universe_package_labels`, or
by `fx set [....] --with //src/diagnostics/lib/host-log-streamer:tests`.
