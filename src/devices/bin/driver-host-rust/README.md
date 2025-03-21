# driver-host-rust

This is the rust implementation of the elf component which drivers are loaded into. It's primary duties are related to managing the driver runtime as well as managing the driver lifecycle. 

## Building

To add this component to your build, append
`--with-base src/devices/bin/driver-host-rust`
to the `fx set` invocation.

## Running

Use `ffx component run` to launch this component into a restricted realm
for development purposes:

```
$ ffx component run /core/ffx-laboratory:driver-host-rust fuchsia-pkg://fuchsia.com/driver-host-rust#meta/driver-host-rust.cm
```

## Testing

Unit tests for driver-host-rust are available in the `driver-host-rust-tests`
package.

```
$ fx test driver-host-rust-tests
```

