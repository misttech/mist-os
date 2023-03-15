# cap

`cap` is a Rust library for defining capabilities and transferring them
between programs.

## Building

To add this component to your build, append
`--with src/sys/bedrock/cap:tests`
to the `fx set` invocation.

## Testing

Tests for `cap` are available in the `cap-tests` and `cap-unittests` packages.

```
$ fx test cap-tests
$ fx test cap-unittests
```

