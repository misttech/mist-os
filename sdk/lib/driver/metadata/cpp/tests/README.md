# Metadata Test
The goal of `:metadata-test` is to make sure that drivers can send and
receive metadata to each other using the `//sdk/lib/driver/metadata/cpp`
library. The test will spawn a driver test realm and include the following four
drivers: `test_root`, `test_parent`, `test_child`, and `test_grandchild`.

Here is the device tree of the test realm created by `:metadata-test`:
```
[dev] pid=101984 fuchsia-boot:///dtr#meta/test_root.cm
  [test_parent_expose] pid=102475 fuchsia-boot:///dtr#meta/test_parent_expose.cm
    [test_child_use] pid=103327 fuchsia-boot:///dtr#meta/test_child_use.cm
      [test_grandchild] pid=104714 fuchsia-boot:///dtr#meta/test_grandchild.cm
        [grandchild] pid=None unbound
    [test_child_no_use] pid=103443 fuchsia-boot:///dtr#meta/test_child_no_use.cm
      [test_grandchild] pid=104919 fuchsia-boot:///dtr#meta/test_grandchild.cm
        [grandchild] pid=None unbound
  [test_parent_no_expose] pid=102593 fuchsia-boot:///dtr#meta/test_parent_no_expose.cm
    [test_child_use] pid=103601 fuchsia-boot:///dtr#meta/test_child_use.cm
      [test_grandchild] pid=105332 fuchsia-boot:///dtr#meta/test_grandchild.cm
        [grandchild] pid=None unbound
    [test_child_no_use] pid=103812 fuchsia-boot:///dtr#meta/test_child_no_use.cm
      [test_grandchild] pid=105550 fuchsia-boot:///dtr#meta/test_grandchild.cm
        [grandchild] pid=None unbound
```

## Drivers
### Test Root
The `test_root`'s purpose is to spawn the two variants of the `test_parent`
driver by creating nodes for those drivers to bind to.

### Test Parent
The `test_parent`'s purpose is to serve metadata to `test_child`. The
`test_parent` will spawn two child nodes that the `test_child` driver variants
can bind to. `test_parent` has two variants: `test_parent_expose` and
`test_parent_no_expose`. The main difference between the two is that
`test_parent_expose` exposes the "fuchsia.hardware.test.Metadata" service while `test_parent_no_expose` does not.

### Test Child
The `test_child`'s purpose is to receive metadata from its parent driver and
forward it to the driver bound to its child node. The driver will spawn a child
node that the `test_grandchild` node can bind to. It also has two variants:
`test_child_use` and `test_child_no_use`. The main difference between the two
is that `test_child_use` declares that it uses the
"fuchsia.hardware.test.Metadata" service while `test_child_no_use` does not.

### Test Grandchild
The `test_grandchild`'s purpose is to recieve metadata from its parent driver
which presumably forwarded the metadata from its parent driver (i.e.
`test_grandchild`'s grandparent).
