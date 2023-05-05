## Audio driver tests

The `audio_driver_tests` suite validates implementations of `fuchsia.hardware.audio.StreamConfig`,
`fuchsia.hardware.audio.RingBuffer` and `fuchsia.hardware.audio.Dai` interfaces.
When the suite runs, it detects, initializes and tests all audio device drivers that are
registered with devfs and thus appear under `/dev/class/audio-input`, `/dev/class/audio-output`,
and `/dev/class/dai`. These drivers are tested non-hermetically, as system instances may be backed
by actual audio hardware.

The suite also hermetically creates and tests an instance of the Bluetooth a2dp library (see
`//src/connectivity/bluetooth/tests/audio-device-output-harness`); this can be disabled by
specifying `--devfs-only`. Note: all flags mentioned here are _suite-specific_. Any `fx test`
command that includes them must include a leading "`-- `".

By design, only one client may connect to an audio driver's `RingBuffer` interface at any time.
In most non-core product builds, `audio_core` is demand-started early in the bootup process,
triggering device initialization and configuration. Restated: on these products, `audio_core` will
connect to the `RingBuffer` of every audio device it detects, before the test gets a chance to run.

For this reason, `audio_driver_tests` assumes that `audio_core` IS present unless told otherwise. By
default, it runs only "basic" `StreamConfig`-related tests (see basic_test.cc) that can execute even
when `audio_core` is already connected to the audio driver. If `--admin` is specified, the suite
_also_ runs "admin" test cases that require access to the driver's RingBuffer interface (see
admin_test.cc). Note: `audio_core` can be manually demand-started even on `core` builds; if for any
reason `audio_core` is running, the "admin" tests will fail.

`audio_driver_tests` uses generous timeout durations; the tests should function correctly even in
heavily loaded test execution environments (such as a device emulator instance on a multi-tenant CQ
server). There are additional test cases, not run by default, that must run in a realtime-capable
environment (see position_test.cc). These tests are enabled by specifying `--run-position-tests`.

Thus, to ***fully*** validate a devfs-based audio driver, execute the following in a `core` release
build running natively (on a non-emulated system):

`fx test audio_driver_tests -- --admin --run-position-tests`
