# System Tests

This is the integration tests for a number of system tests.

## Test Setup

In order to build the system tests, add this to your `fx set`:

```sh
% fx set ... \
  --with //src/sys/pkg:e2e_tests \
  --with //src/testing/sl4f \
  --with //src/sys/bin/start_sl4f \
  --args='core_realm_shards = [ "//src/testing/sl4f:sl4f_core_shard" ]'

% fx build
```

Next, you need to authenticate against luci to be able to download build
artifacts. First, authenticate against cipd with:

```sh
% cipd auth-login
```

Next, install chromium's `depot_tools` by following
[these instructions](https://commondatastorage.googleapis.com/chrome-infra-docs/flat/depot_tools/docs/html/depot_tools_tutorial.html).
Then, login to luci by running:

```sh
% cd depot_tools
% ./luci-auth login -scopes "https://www.googleapis.com/auth/userinfo.email https://www.googleapis.com/auth/devstorage.read_write"
...
```

Now you should be able to run the tests against any device that is discoverable
by `ffx target list`.

## Available Tests

### Upgrade Tests

This tests Over the Air (OTA) updates. At a high level, it:

* Downloads downgrade and upgrade build artifacts
* Tells the device to reboot into recovery.
* Paves the device with the downgrade build artifacts (known as version N-1).
* OTAs the to the upgrade build artifacts (known as version N).
* OTAs the device to a variant of the upgrade build artifacts (known as version
  N').
* OTAs back into upgrade build artifacts (version N).

You should be able to run the tests with:

```sh
% $(fx get-build-dir)/host_x64/system_tests_upgrade \
  --ssh-private-key ~/.ssh/fuchsia_ed25519 \
  --builder-name fuchsia/global.ci/core.x64-release-nuc_in_basic_envs \
  --fuchsia-build-dir $(fx get-build-dir)
```

This will run through the whole test paving the build to the latest version
available from the specified builder, then OTA-ing the device to the local build
directory.

The Upgrade Tests also support reproducing a specific build. To do this,
determine the build ids from the downgrade and upgrade builds, then run:

```sh
% $(fx get-build-dir)/host_x64/system_tests_upgrade \
  --ssh-private-key ~/.ssh/fuchsia_ed25519 \
  --build-id 123456789... \
  --build-id 987654321...
```

Or you can combine these options:

```sh
% $(fx get-build-dir)/host_x64/system_tests_upgrade \
  --ssh-private-key ~/.ssh/fuchsia_ed25519 \
  --build-id 123456789... \
  --fuchsia-build-dir $(fx get-build-dir)
```

There are more options to the test, to see them all run
`$(fx get-build-dir)/host_x64/system_tests_upgrade -- -h`.

### Reboot Testing

The system tests support running reboot tests, where a device is rebooted a
configurable number of times, or errs out if a problem occurs. This
can be done by running:

```sh
% $(fx get-build-dir)/host_x64/system_tests_reboot \
  --ssh-private-key ~/.ssh/fuchsia_ed25519 \
  --fuchsia-build-dir $(fx get-build-dir)
```

Or if you want to test a build, you can use:

* `--builder-name fuchsia/global.ci/core.x64-release-nuc_in_basic_envs`, to test the
  latest build published by that builder.
* `--build-id 1234...` to test the specific build.

## Running the Tests

When running the system tests, it's helpful to capture the serial logs, and
system logs, and the test output to a file in order to triage any failures. This
is especially handy when cycle testing. To simplify the setup, the system-tests
come with a helper script `run-test` that can setup a `tmux` session
for you. You can run it like this:

```sh
% ${FUCHSIA_DIR}/src/sys/pkg/tests/system-tests/bin/run-test \
  -o ~/logs \
  --tty /dev/ttyUSB0 \
  $(fx get-build-dir)/host_x64/system_tests_upgrade \
  --builder-name fuchsia/global.ci/core.x64-release-nuc_in_basic_envs \
  --fuchsia-build-dir $(fx get-build-dir)
```

This will setup a `tmux` with 3 windows, one for the serial session on
`/dev/ttyUSB0`, one for the system logs, and one for the test. All output from
the `tmux` windows will be saved into `~/logs`.

See the `run-test --help` for more options.

## Running the tests locally in the Fuchsia Emulator (experimental)

At the moment, the build script `fx qemu` does not bring up a configuration that can be
OTA-ed. Until this is implemented, the `bin/` directory contains some helper
scripts that bring up an OTA-able Fuchsia emulator. Follow these instructions to
it.

The `create-emu` script will create a Fuchsia EFI image:

```sh
% ./bin/create-emu some/image
```

This image can then be used by running:

```sh
% fx qemu --uefi -D some/path/to/image
```

Each script supports `--help` to see other supported flags.

These scripts wrap `fx make-fuchsia-vol` to create the Fuchsia image, and
`//zircon/scripts/run-zircon` to run QEMU.

Once the VM has finished paving, you can then use it with the upgrade tests:

```sh
% $(fx get-build-dir)/host_x64/system_tests_upgrade \
  --ssh-private-key ~/fuchsia/.ssh/pkey \
  --builder-name fuchsia/global.ci/workstation_eng.x64-release-e2e-isolated \
  --fuchsia-build-dir $(fx get-build-dir) \
  --device fuchsia-5254-0063-5e7a
```

Note that your QEMU device will always be named `fuchsia-5254-0063-5e7a`.
Explicitly naming the device for the test will help prevent accidental upgrades
to your other devices.
