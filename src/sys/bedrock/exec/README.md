# exec

`exec` is a Rust library that provides common tools and interfaces that allow
users to run and manage software on Fuchsia at various levels of abstraction,
from Zircon processes to components.

This is a prototype for the bedrock layer of the Component Framework that
defines core APIs for structuring and running a software topology, underpinning
the component model. It provides an alternate way of implementing the
component runtime, not a new definition of components.

## Building

To add this component to your build, append
`--with src/sys/bedrock/exec:tests`
to the `fx set` invocation.

## Testing

Tests for `exec` are available in the `exec-unittests` package.

```
$ fx test exec-unittests
```

[processes]: https://fuchsia.dev/fuchsia-src/concepts/process/overview
[components]: https://fuchsia.dev/fuchsia-src/concepts/components
