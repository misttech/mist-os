# fake-driver

TODO: Brief overview of the component.

## Building

To add this component to your build, append
`--with-base src/power/power-broker/tests/testing/fake-driver`
to the `fx set` invocation.

## Running

Use `ffx component run` to launch this component into a restricted realm
for development purposes:

```
$ ffx component run /core/ffx-laboratory:fake-driver fuchsia-pkg://fuchsia.com/fake-driver#meta/fake-driver.cm
```

## Testing

Unit tests for fake-driver are available in the `fake-driver-tests`
package.

```
$ fx test fake-driver-tests
```

