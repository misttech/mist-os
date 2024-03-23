# End to end test framework

[TOC]

Lacewing is Fuchsia's python end-to-end multi-device testing framework.

This framework can be used for authoring host-driven E2E tests that interact
with 1 or more Fuchsia devices. The framework can be extended to work with
non Fuchsia devices as well.

This framework aims to provide a uniform test writing experience to all
users and exposes conveninece methods for Fuchsia device interactions that
are ergonomic and robust; with the ultimate goal of providing scriptable
host-side FIDL access to a Fuchsia device-under-test.

At a high level, the Lacewing test framework consists of three layers of
abstraction:
* Test framework - Mobly
* Device Controller - Honeydew
* Host-Target Transport - Currently SL4F (eventually [Fuchsia Controller](https://cs.opensource.google/fuchsia/fuchsia/+/main:src/developer/ffx/lib/fuchsia-controller/))

Why the name Lacewing? Lacewings are insects that keep the Fuchsia plant healthy
and free of harmful bugs - which is something that this framework aspires to be!

## Getting Started [30 mins]

Host tests development with the Lacewing framework is simple and fast.

* Leverages widely used Mobly test framework for standardized testbed definition
and multi-device support.
* Fuchsia device interaction logic is built into the Honeydew library.
* Fast edit-compile-test local development workflow via `fx test`.
* Fuchsia infra integration works out-of-the-box (provided a testbed exists in
lab and Swarming).

The following sections walk through the local development process for a basic
single-device Lacewing test case.

### Confirm device connection

Ensure that there's a Fuchsia device that's accessible from the host via

```sh
$ cd $FUCHSIA_DIR
$ ffx target list
```

If you do not have a physical device handy, refer to the [Fuchsia-Emulator](./honeydew/tests/functional_tests/README.md#Fuchsia-Emulator) section to start an emulator.

### Test directory

Start by creating a test directory:

```sh
$ cd $FUCHSIA_DIR/src/testing/end_to_end/examples
$ mkdir my_test_dir
$ cd my_test_dir
$ touch my_test.py
$ touch BUILD.gn
````

### Mobly Config YAML File

The Lacewing framework uses the open source Mobly test framework for handling
generic E2E test framework features such as testbed specification, device
initialization abstraction, assertions, and logging.

Every Mobly test requires a testbed configuration file to be provided to specify
the device(s) under test (DUTs) that the E2E test will need to interact with.

For local testing, Lacewing generates a default Mobly config based on the host
environment. This is a best-effort algorithm that assumes that all detected
Fuchsia device can be accessed using the `~/.ssh/fuchsia_ed25519` SSH private
key. This generated config file should serve the majority of in-tree use cases
where devices and emulators are paved/flashed/built directly from Fuchsia.git).

If the default config generation does not fit your use-case, see the
[Local Manual Mobly Config section](#local-manual-mobly-config) to create a YAML
file from scratch and override the default behavior.

NOTE: For infra execution, Mobly config generation is entirely automated by
Lacewing so nothing is required from test authors (besides picking the correct
|environments| in the `BUILD.gn` for test targets to run against).
```sh
$ touch my_config.yaml
````

For simplicity, this is an example of a single Fuchsia device testbed.

Multi-device testbeds follows a similar configuration but is out-of-scope of this guide - Please direct to
[Multi-device setup and execution](https://cs.opensource.google/fuchsia/fuchsia/+/main:src/testing/end_to_end/examples/test_multi_device/README.md)

### Lacewing test module

Every Lacewing Mobly test follows the simple scaffolding below:

```py
import logging

from fuchsia_base_test import fuchsia_base_test
from mobly import test_runner

_LOGGER = logging.getLogger(__name__)


class MyFirstLacewingTest(fuchsia_base_test.FuchsiaBaseTest):
    def setup_class(self):
        """Initialize all DUT(s)"""
        super().setup_class()
        self.fuchsia_dut = self.fuchsia_devices[0]

    def test_my_first_testcase(self):
        _LOGGER.info("Running my first Lacewing test...")
        # Test logic goes here.
        # e.g. self.fuchsia_dut.some_api(...)

if __name__ == '__main__':
    test_runner.main()
```

The `self.fuchsia_dut` object contains convenient functions for common Fuchsia
device interactions. Fore more information see the
[Exploring the APIs section](#exploring-the-apis).

### Build definition

Now that you have a Lacewing test module you can integrate it with the build
system.


```gn
import("//build/python/python_mobly_test.gni")

python_mobly_test("my_test_target") {
    main_source = "my_test.py"
    libraries = [
      # Honeydew provides device interaction APIs.
      "//src/testing/end_to_end/honeydew",
      # Base class provides common Fuchsia testing setup and teardown logic.
      "//src/testing/end_to_end/mobly_base_tests:fuchsia_base_test",
    ]
    # Transport can also be "sl4f" if SL4F is required by your test.
    transport = "fuchsia-controller"
}
```
### Test execution

Once the Lacewing target has been defined, you can trigger the test to run
locally via `fx test`.

```sh
# Step 1 - Configure build

# 1.a - If Fuchsia-Controller transport.
$ fx set workbench_eng.x64 \
    --with-host //src/testing/end_to_end/examples/my_test_dir:my_test_target

# 1.b - If SL4F transport.
$ fx set core.x64 \
    --with-host //src/testing/end_to_end/examples/my_test_dir:my_test_target \
    --with //src/testing/sl4f \
    --with //src/sys/bin/start_sl4f \
    --args 'core_realm_shards += [ "//src/testing/sl4f:sl4f_core_shard" ]'

# Step 2- Start the emulator and package server (in separate terminals)
$ fx serve
$ ffx emu stop ; ffx emu start -H --net tap

# Step 3 - Run the test
$ fx test //src/testing/end_to_end/examples/my_test_dir:my_test_target --e2e \
    --output
```

By default, test logs are stored under `${FUCHSIA_OUT_DIR}/test_out/`. Users may
override this by supplying `$FUCHSIA_TEST_OUTDIR=<custom_path>` before their
`fx test` invocation.

Congrats! You've just written and run your first Lacewing test!

## Further Reading

### Test parameters

If you'd like to pass parameters to the Lacewing test, you can do so by creating
a Mobly Test Param YAML file.

```sh
$ touch params.yaml
```

The YAML content can be in any organization structure you'd like and any common
data types:

```yaml
bool_param: True
str_param: some_string
dict_param:
  fld_1: val_1
  fld_2: val_2
list_param:
  - 1
  - 2
  - 3
  - 4
```

After updating the content of this file, update BUILD.gn to include it in the
`python_mobly_test()` target.

```gn
python_mobly_test("my_test") {
    ...
    params_source = "params.yaml"
    ...
}
```

Inside your Lacewing test, you can access the parameters like so:

```py
# The params are accessible from any test methods with access to `self`as well,
# not just in setup related methods.
def setup_class(self):
    ...
    my_bool_param = self.user_params["bool_param"]
    my_list_param = self.user_params["list_param"]
    ...
```

### Test output

If your test generates artifacts, it is recommended that you do so under the
`self.log_path` directory.

Similar to `self.user_params`, this is a framework-provided variable that can be
accessed at any point during the test. Storing your output under this directory
will ensure your test and its output integrates well if it's ever run in Fuchsia
infra.

```py
my_artifact_path = os.path.join(self.log_path, 'my_artifact')
with open(my_artifact_path, 'w+', encoding="utf8") as file_handle:
    file_handle.write('data')
```

### Exploring the APIs

After creating your first Lacewing test, it's time to add the test logic that
will perform all of the interesting host-device-interactions.

The Lacewing framework comes with its built-in API for mediating interactions
with Fuchsia devices.

See the [Honeydew README.md](https://cs.opensource.google/fuchsia/fuchsia/+/main:src/testing/end_to_end/honeydew/README.md) to learn more.

### Existing test examples

See [examples](https://cs.opensource.google/fuchsia/fuchsia/+/main:src/testing/end_to_end/examples/) for working examples of existing Lacewing tests.

### Local Manual Mobly Config
NOTE: Read on if the default generated Mobly config is insufficient.

```sh
$ touch my_config.yaml
````

For simplicity, this is an example of a single Fuchsia device testbed.

NOTE: Multi-device testbeds follows a similar configuration but is out-of-scope
of this guide - stay tuned for the "Multi-device testing" guide.
TODO(https://fxbug.dev/42075357)

```yaml
TestBeds:
  - Name: My_Simple_Testbed
    Controllers:
      FuchsiaDevice:
        - name: $FUCHSIA_NODENAME
          ssh_private_key: $PKEY
```

Where `$FUCHSIA_NODENAME` and `$PKEY` need to be manually substituted to fit
your local development environment (More info on each of these fields in
the sections below).

After updating the content of this file, update BUILD.gn to include it in the
`python_mobly_test()` target.

```gn
python_mobly_test("my_test") {
    ...
    local_config_source = "my_config.yaml"
    ...
}
```

#### $FUCHSIA_NODENAME
`name: $FUCHSIA_NODENAME` is the device-under-test that's accessible from the
host. A quick way to determine what this is in your local environment is to use
`ffx`:

```sh
$ ffx target list
NAME                SERIAL       TYPE             STATE      ADDRS/IP                           RCS
fuchsia-emulator*   <unknown>    core.x64    Product    [fe80::1a1c:ebd2:2db:6104%qemu]    Y
```

The `$FUCSHIA_NODENAME` in the above example would be `fuchsia-emulator`.

NOTE: This may be an emulator or a physical device. If there are multiple
accessible devices, choose only the one that you'd like to target in the host
test.

#### $PKEY
The `ssh_private_key: $PKEY` value should match the path of the SSH private key
that can be used to connect to the DUT. This is the key that pairs with the
`authorized_keys` used in paving workflows or exists in the emulator image. For
most users, this is ` ~/.ssh/fuchsia_ed25519` as it's the key used by Fuchsia's
`fx` workflows. If the DUT is provisioned by other means, you'd have to provide
the path to the corresponding SSH private key.

A quick way to confirm that Fuchsia's default SSH key works is the following:

```sh
$ fx set-device $FUCHSIA_NODENAME
$ fx shell ls
```

If the above succeeds, then the following command returns the working SSH key
path:

```sh
$ head -1 $FUCHSIA_DIR/.fx-ssh-path
```
