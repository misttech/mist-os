## Audio driver tests

The `audio_driver_tests` suite validates implementations of `fuchsia.hardware.audio.StreamConfig`,
`fuchsia.hardware.audio.RingBuffer`, `fuchsia.hardware.audio.Dai`, `fuchsia.hardware.audio.Codec`,
`fuchsia.hardware.audio.Composite` and `fuchsia.hardware.audio.signalprocessing` interfaces.
When the suite runs, it detects, initializes and tests all audio device drivers that are
registered with devfs and thus appear under `/dev/class/audio-input`, `/dev/class/audio-output`,
`/dev/class/dai`, `/dev/class/codec` and `/dev/class/audio-composite`. These drivers are tested
non-hermetically, as system instances may be backed by actual audio hardware.

The suite also creates and tests a number of `virtual_audio` driver instances, using their default
settings; this can be disabled by including the `--no-virtual` flag.

The suite also hermetically creates and tests an instance of the Bluetooth a2dp library (see
`//src/connectivity/bluetooth/tests/audio-device-output-harness`); this can be disabled by
specifying the `--devfs-only` flag.

By design, clients of `fuchsia.hardware.audio.StreamConfig` and `fuchsia.hardware.audio.Dai`
may connect to only one audio driver's `RingBuffer` interface at any time. In most non-core product
builds, `audio_core` (or `audio_device_registry` which serves a similar role) is demand-started
early in the bootup process, triggering device initialization and configuration. Restated: on these
products, `audio_core` will connect to the `RingBuffer` of every audio device it detects, before the
test gets a chance to run.

For this reason, `audio_driver_tests` assumes that `audio_core` IS present unless told otherwise. By
default, it runs only "basic" `StreamConfig`-related tests (see basic_test.cc) that can execute even
when `audio_core` is already connected to the audio driver. If `--admin` is specified, the suite
_also_ runs "admin" test cases that (1) reconfigure devices without restoring the previous state,
or (2) use interfaces such as RingBuffer that can only be allocated to one client at a time (and
thus cannot be validated by "basic" tests that assume `audio_core` is already present). Note:
`audio_core` can be manually demand-started even on `core` builds; if for any reason `audio_core` is
running, the "admin" tests will fail.

`audio_driver_tests` uses generous timeout durations; the tests should function correctly even in
heavily loaded test execution environments (such as a device emulator instance on a multi-tenant CQ
server). There are additional test cases, not run by default, that must run in a realtime-capable
environment (see position_test.cc). These tests are enabled by specifying `--run-position-tests`.

Note: all flags mentioned here are _suite-specific_. Any `fx test` command that includes them must
include an extra `-- ` before these flags.

Thus, to ***fully*** validate a devfs-based audio driver, while avoiding testing `virtual_audio`
instances or the Bluetooth audio library, execute the following in a `core` release build running
natively (on a non-emulated system):

`fx test audio_driver_tests -- --admin --run-position-tests --no-virtual --devfs-only`
