# Driver Transport Example

Reviewed on: 2024-03-20

This example demonstrates a parent driver serving a FIDL protocol over driver transport and a
child driver that connects and queries data from it.

## Building

To include the driver to your build, append `--with //examples/drivers:drivers` to your `fx
set` command. For example:

```bash
$ fx set core.x64 --with //examples/drivers:drivers
$ fx build
```

## Running

Register the parent driver by running this command
```bash
$ ffx driver register fuchsia-pkg://fuchsia.com/driver_transport#meta/driver_transport_parent.cm
```

Register the child driver by running this command
```bash
$ ffx driver register fuchsia-pkg://fuchsia.com/driver_transport#meta/driver_transport_child.cm
```

Verify that both drivers show up in this command
```bash
ffx driver list
```

Add a test node that binds to the parent driver:
```bash
$ ffx driver test-node add driver_transport_parent gizmo.example.TEST_NODE_ID=driver_transport_parent
```

Run the following command to verify that the driver is bound to the node:
```bash
$ ffx driver list-devices -v transport
```

You should see something like this:
```
Name     : driver_transport_parent
Moniker  : dev.driver_transport_parent
Driver   : fuchsia-pkg://fuchsia.com/driver_transport#meta/driver_transport_parent.cm
2 Properties
[ 1/  2] : Key "gizmo.example.TEST_NODE_ID"   Value "driver_transport_parent"
[ 2/  2] : Key "fuchsia.platform.DRIVER_FRAMEWORK_VERSION" Value 0x000002
0 Offers

Name     : driver_transport_child
Moniker  : dev.driver_transport_parent.driver_transport_child
Driver   : fuchsia-pkg://fuchsia.com/driver_transport#meta/driver_transport_child.cm
2 Properties
[ 1/  2] : Key "fuchsia.platform.DRIVER_FRAMEWORK_VERSION" Value 0x000002
[ 2/  2] : Key "fuchsia.examples.gizmo.Service" Value "fuchsia.examples.gizmo.Service.DriverTransport"
1 Offers
Service: fuchsia.examples.gizmo.Service
  Source: dev.driver_transport_parent
  Instances: default

Name     : transport-child
Moniker  : dev.driver_transport_parent.driver_transport_child.transport-child
Driver   : unbound
1 Properties
[ 1/  1] : Key "fuchsia.platform.DRIVER_FRAMEWORK_VERSION" Value 0x000002
0 Offers
```

## Testing

Include the tests to your build by appending `--with //examples/drivers:tests` to your `fx
set` command. For example:

```bash
$ fx set core.x64 --with //examples/drivers:drivers --with //examples:tests
$ fx build
```

Run unit tests with the command:
```bash
$ fx test driver_transport_example_test
```

The unit test sets up a fake driver transport server for the child driver and verifies that
the driver successfully queried the values from the server.

## Source layout

The core implementation of the parent driver is in `parent-driver.cc` and the child driver
is in `child-driver.cc`. Unit tests are located in `tests`.
