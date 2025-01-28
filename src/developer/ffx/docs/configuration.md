# Configuring FFX

Last updated 2025-01-27

The `ffx` tool supports many configuration values to tweak its behaviour. This
is an attempt to centralize the available configuration values and document
them.

Note this document is (for now) manually updated and as such may be out of date.

When updating, please add the value in alphabetical order.

    | Configuration Value                     | Documentation                      |
    | --------------------------------------- | ---------------------------------- |
    | `daemon.autostart`                      | Determines if the daemon should    |
    |                                         | start automatically when a subtool |
    |                                         | that requires the daemon is        |
    |                                         | invoked.  Defaults to `true`.      |
    | `daemon.host_pipe_ssh_timeout`          | Time the daemon waits for an       |
    |                                         | initial response from ssh on the   |
    |                                         | target. Defaults to `50` seconds.  |
    | `discovery.expire_targets`              | Determines if targets discovered   |
    |                                         | should expire. Defaults to `true`  |
    | `discovery.mdns.autoconnect`            | Determines whether to connect      |
    |                                         | automatically to targets           |
    |                                         | discovered through mDNS. Defaults  |
    |                                         | to `false`                         |
    | `discovery.timeout`                     | When doing _local_ discovery in    |
    |                                         | `ffx target list`, how long in     |
    |                                         | milliseconds to wait before        |
    |                                         | collecting responses. Defaults to  |
    |                                         | `2000`                             |
    | `discovery.zedboot.advert_port`         | Zedboot discovery port (must be a  |
    |                                         | nonzero u16). Default to `33331`   |
    | `discovery.zedboot.enabled`             | Determines if zedboot discovery is |
    |                                         | enabled. Defaults to `false`       |
    | `emu.console.enabled`                   | The experimental flag for the      |
    |                                         | console subcommand. Defaults to    |
    |                                         | `false`.                           |
    | `emu.device`                            | The default virtual device name to |
    |                                         | configure the emulator. Defaults   |
    |                                         | to `""` (the empty string), but can|
    |                                         | be overridden by the user.         |
    | `emu.engine`                            | The default engine to launch from  |
    |                                         | `ffx emu start`. Defaults to `femu`|
    |                                         | but can be overridden by the user. |
    | `emu.gpu`                               | The default gpu type to use in     |
    |                                         | `ffx emu start`. Defaults to       |
    |                                         | `auto`, but can be overridden by   |
    |                                         | the user.                          |
    | `emu.instance_dir`                      | The root directory for storing     |
    |                                         | instance specific data. Instances  |
    |                                         | should create a subdirectory in    |
    |                                         | this directory to store data.      |
    |                                         | Defaults to `$DATA/emu/instances`  |
    | `emu.kvm_path`                          | The filesystem path to the         |
    |                                         | system's KVM device. Must be       |
    |                                         | writable by the running process to |
    |                                         | utilize KVM for acceleration.      |
    |                                         | Defaults to `/dev/kvm`             |
    | `emu.start.timeout`                     | The duration (in seconds) to       |
    |                                         | attempt to establish an RCS        |
    |                                         | connection with a new emulator     |
    |                                         | before returning to the terminal.  |
    |                                         | Not used in --console or           |
    |                                         | ---monitor modes. Defaults to `60` |
    |                                         | seconds.                           |
    | `emu.upscript`                          | The full path to the script to run |
    |                                         | initializing any network           |
    |                                         | interfaces before starting the     |
    |                                         | emulator.                          |
    |                                         | Defaults to `""` (the empty string)|
    | `fastboot.devices_file`                 | Path to the fastboot devices file. |
    |                                         | Defaults to                        |
    |                                         | `${HOME}/.fastboot/devices`        |
    | `fastboot.flash.min_timeout_secs`       | The minimum flash timeout (in      |
    |                                         | seconds) for flashing to a target  |
    |                                         | device. Defaults to `60` seconds   |
    | `fastboot.flash.timeout_rate`           | The timeout rate in mb/s when      |
    |                                         | communicating with the target      |
    |                                         | device. Defaults to `2` MB/sec     |
    | `fastboot.reboot.reconnect_timeout`     | Timeout in seconds to wait for     |
    |                                         | target after a reboot to fastboot  |
    |                                         | mode. Defaults to `10` seconds     |
    | `fastboot.tcp.open.retry.count`         | Number of times to retry when      |
    |                                         | connecting to a target in fastboot |
    |                                         | over TCP                           |
    | `fastboot.tcp.open.retry.wait`          | Time to wait for a response when   |
    |                                         | connecting to a target in fastboot |
    |                                         | over TCP                           |
    | `fastboot.usb.disabled`                 | Disables fastboot usb discovery if |
    |                                         | set to true. Defaults to `false`   |
    | `ffx.daemon_timeout`                    | How long to wait in milliseconds   |
    |                                         | when attempting to connect to the  |
    |                                         | daemon. Defaults to `15000`        |
    | `ffx.fastboot.inline_target`            | Boolean value to signal that the   |
    |                                         | target is in fastboot, and to      |
    |                                         | communicate directly with it as    |
    |                                         | opposed to doing discovery.        |
    |                                         | Defaults to `false`                |
    | `ffx.isolated`                          | "Alias" for encapsulation of       |
    |                                         | config options used to request     |
    |                                         | isolation. Currently affects:      |
    |                                         | `fastboot.usb.disabled`,           |
    |                                         | `ffx.analytics.disabled`,          |
    |                                         | `discovery.mdns.enabled`,          |
    |                                         | `discovery.mdns.autoconnect`       |
    |                                         | Defaults to `false`                |
    | `log.dir`                               | Location for ffx and daemon logs   |
    |                                         | Defaults to first available of:    |
    |                                         |   `$FFX_LOG_DIR`                   |
    |                                         |   `$FUCHSIA_TEST_OUTDIR/ffx_logs`  |
    |                                         |   `$CACHE/logs`                    |
    | `log.enabled`                           | Whether logging is enabled         |
    |                                         | Defaults to `true`                 |
    | `log.include_spans`                     | Whether spans (function names,     |
    |                                         | parameters, etc) are included      |
    |                                         | Defaults to `false`                |
    | `log.level`                             | Filter level for log messages      |
    |                                         | Overridable on specific components |
    |                                         | via `log.target_levels.<prefix>`.  |
    |                                         | Values are:                        |
    |                                         | `error`, `warn`, `info`, `debug`,  |
    |                                         | `trace`                            |
    |                                         | Defaults to `info`                 |
    | `log.rotate_size`                       | Limit of log size before log file  |
    |                                         | is rotated (if rotation is enabled)|
    |                                         | Defaults to no rotation            |
    | `log.rotations`                         | How many rotations of log files    |
    |                                         | to keep (0 to disable rotation)    |
    |                                         | Defaults to `5`                    |
    | `log.target_levels.<prefix>`            | Filter levels for components with  |
    |                                         | specified prefix. Values are:      |
    |                                         | `error`, `warn`, `info`, `debug`,  |
    |                                         | `trace`. No components are defined |
    |                                         | by default                         |
    | `repository.repositories`               |                                    |
    | `repository.registrations`              |                                    |
    | `repository.default`                    |                                    |
    | `repository.server.mode`                |                                    |
    | `repository.server.enabled`             | If the repository server is        |
    |                                         | enabled. Defaults to `false`       |
    | `repository.server.listen`              |                                    |
    | `repository.server.last_used_address`   |                                    |
    | `ssh.auth-sock`                         | If set, the path to the            |
    |                                         | authorization socket for SSH used  |
    |                                         | by overnet. Defaults to unset      |
    | `ssh.keepalive_timeout`                 | Time for an ssh connection to wait |
    |                                         | before timing out.                 |
    |                                         | Defaults to `20` seconds.          |
    | `target.default`                        | The default target to use if one   |
    |                                         | is unspecified.                    |
    |                                         | Defaults to first available of:    |
    |                                         |   `$FUCHSIA_DEVICE_ADDR`           |
    |                                         |   `$FUCHSIA_NODENAME`              |
    | `target.host_pipe_ssh_timeout`          | Time the target waits for an       |
    |                                         | initial response from ssh on the   |
    |                                         | target (currently, only in `ffx    |
    |                                         | target list`). Defaults to `10`    |
    |                                         | seconds.                           |
    | `targets.manual`                        | Contains the list of manual        |
    |                                         | targets. Defaults to an empty list |
    | `ffx.subtool-search-paths`              | A list of paths to search for non- |
    |                                         | SDK subtool binaries. Defaults to  |
    |                                         | `$BUILD_DIR/host-tools`            |
    | `fidl.ir.path`                          | The path for looking up FIDL IR    |
    |                                         | encoding/decoding FIDL messages.   |
    |                                         | Default is unset                   |
