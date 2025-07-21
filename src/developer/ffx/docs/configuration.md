# Configuring FFX

Last updated 2025-01-27

The `ffx` tool supports many configuration values to tweak its behaviour. This
is an attempt to centralize the available configuration values and document
them.

Note this document is (for now) manually updated and as such may be out of date.

When updating, please add the value in alphabetical order.

    | Configuration Value                     | Documentation                      |
    | --------------------------------------- | ---------------------------------- |
    | `connectivity.direct`                   | Support direct target connections. |
    |                                         | Defaults to `false`.               |
    | `connectivity.enable_usb`               | Allow ffx to use a USB connection. |
    |                                         | Not supported on mac. Defaults to  |
    |                                         | `false`.                           |
    | `connectivity.enable_vsock`             | Allow using a VSOCK socket to      |
    |                                         | connect to virtual machine targets |
    |                                         | where supported. Defaults to       |
    |                                         | `false`.                           |
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
    | `discovery.mdns.enabled`                | Determines whether mDNS broadcasts |
    |                                         | are used to discover targets.      |
    |                                         | Default: `true`                    |
    | `discovery.timeout`                     | When doing _local_ discovery in    |
    |                                         | `ffx target list`, how long in     |
    |                                         | milliseconds to wait before        |
    |                                         | collecting responses. Defaults to  |
    |                                         | `2000`                             |
    | `discovery.zedboot.advert_port`         | Zedboot discovery port (must be a  |
    |                                         | nonzero u16). Default to `33331`   |
    | `discovery.zedboot.enabled`             | Determines if zedboot discovery is |
    |                                         | enabled. Defaults to `false`       |
    | `doctor.record_config`                  | Determines whether ffx doctor will |
    |                                         | record config data.  If unset, user|
    |                                         | will be prompted.                  |
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
    | `fastboot.devices_file.path`            | Path to the fastboot devices file. |
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
    | `fidl.ir.path`                          | The path for looking up FIDL IR    |
    |                                         | encoding/decoding FIDL messages.   |
    |                                         | Default is unset                   |
    | `ffx.daemon_timeout`                    | How long to wait in milliseconds   |
    |                                         | when attempting to connect to the  |
    |                                         | daemon. Defaults to `15000`        |
    |                                         | Defaults to `false`                |
    | `ffx.isolated`                          | "Alias" for encapsulation of       |
    |                                         | config options used to request     |
    |                                         | isolation. Currently affects:      |
    |                                         | `fastboot.usb.disabled`,           |
    |                                         | `ffx.analytics.disabled`,          |
    |                                         | `discovery.mdns.enabled`,          |
    |                                         | `discovery.mdns.autoconnect`       |
    |                                         | Defaults to `false`                |
    | `ffx.subtool-search-paths`              | A list of paths to search for non- |
    |                                         | SDK subtool binaries. Defaults to  |
    |                                         | `$BUILD_DIR/host-tools`            |
    | `ffx.ui.mode`                           | Sets the ui mode for ffx and fx    |
    |                                         | options are "text" and "tui",      |
    |                                         | defaults to `text` for plaintext   |
    |                                         | output. 'tui' enables TUI mode     |
    |                                         | where available.                   |
    | `ffx.ui.overrides`                      | Allows per-command overrides of the|
    |                                         | UI mode. Commands are identified by|
    |                                         | tool-command, e.g. `fx-use`        |
    | `fuchsia.analytics.ffx_invoker`         | Optional string used to specify the|
    |                                         | invoker in analytics, e.g. "fx".   |
    |                                         | Default is to not specify an       |
    |                                         | invoker.                           |
    | `fuzzer.output`                         | Output directory when using `ffx   |
    |                                         | fuzz`. No default.                 |
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
    | `overnet.socket`                        | Path to the overnet socket.        |
    |                                         | Defaults to ASCENDD env variable,  |
    |                                         | or a dynamically-calculated path if|
    |                                         | unset.                             |
    | `pbms.base_urls`                        | List of base URLS (of scheme file: |
    |                                         | or gs:). Files are used directly,  |
    |                                         | gs links are used to construct     |
    |                                         | branch-specific GCS objects.       |
    | `product.path`                          | Path to a product bundle. No       |
    |                                         | default.                           |
    | `product.reboot.use_dm`                 | Specifies whether to use `dm` over |
    |                                         | `ssh` to reboot the product when in|
    |                                         | product mode. Default: false       |
    | `proxy.timeout_secs`                    | Timeout when connecting to the     |
    |                                         | target. Also settable with `ffx    |
    |                                         | --timeout <secs>.  Default: 1      |
    |                                         | second.                            |
    | `repository.connect_timeout_secs`       | Timeout when repostiroy server     |
    |                                         | connects to the target.            |
    |                                         | Default: 120 seconds               |
    | `repository.repositories`               |                                    |
    | `repository.registrations`              |                                    |
    | `repository.default`                    | Default repository name. Default to|
    |                                         | empty string                       |
    | `repository.process_dir`                | Path to directory containing       |
    |                                         | package server instances. No       |
    |                                         | default.                           |
    | `repository.server.mode`                |                                    |
    | `repository.server.enabled`             | If the repository server is        |
    |                                         | enabled. Defaults to `false`       |
    | `repository.server.listen`              |                                    |
    | `repository.server.last_used_address`   |                                    |
    | `ssh.allow_fdomain`                     | Allow FDomain to be used as a      |
    |                                         | remoting protocol. Defaults to     |
    |                                         | `false` unless ffx is in "strict"  |
    |                                         | mode.                              |
    | `ssh.auth-sock`                         | If set, the path to the            |
    |                                         | authorization socket for SSH used  |
    |                                         | by overnet. Defaults to unset      |
    | `ssh.keepalive_timeout`                 | Time for an ssh connection to wait |
    |                                         | before timing out.                 |
    |                                         | Defaults to `20` seconds.          |
    | `target.host_pipe_ssh_timeout`          | Time the target waits for an       |
    |                                         | initial response from ssh on the   |
    |                                         | target (currently, only in `ffx    |
    |                                         | target list`). Defaults to `10`    |
    |                                         | seconds.                           |
    | `target.stateless_default_configuration`| Experimental flag to limit targets |
    |                                         | to those explicitly specified via  |
    |                                         | `--target` or via env variables.   |
    |                                         | Default: false                     |
    | `targets.manual`                        | Contains the list of manual        |
    |                                         | targets. Defaults to an empty list |
    | `trace.category_groups`                 | List of categories on which to     |
    |                                         | enable tracing.  Defaults to a     |
    |                                         | large list -- use `ffx config get` |
    |                                         | to see the default categories.     |
    | `triage.config_paths`                   | Contains the list of default triage|
    |                                         | configs. Must be set in out-of-tree|
    |                                         | environments.                      |
    | `tunnels`                               | Contains the list of tunnels as    |
    |                                         | specified by Tunnel::ForwardPort   |
    |                                         | requests. Default: empty           |
    | `watchdogs.host_pipe.enabled`           | Specifies whether to run           |
    |                                         | "watchdogs" on daemon host-pipes,  |
    |                                         | in order to debug whether ssh      |
    |                                         | failures are due to the Rust       |
    |                                         | executor getting stuck.            |
