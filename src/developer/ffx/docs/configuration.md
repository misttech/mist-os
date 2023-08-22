# Configuring FFX

Last updated 2023-01-17

The `ffx` tool supports many configuration values to tweak it's behaviour. This
is an attempt to centralize the available configuration values and document
them.

Note this document is (for now) manually updated and as such may be out of date.

When updating, please add the value in alphabetical order.

| Configuration Value                     | Documentation                      |
| --------------------------------------- | ---------------------------------- |
| `daemon.autostart`                      | Determines if the daemon should    |
:                                         : start automatically when a subtool :
:                                         : that requires the daemon is        :
:                                         : invoked.  Defaults to `true`.      :
| `daemon.host_pipe_ssh_timeout`          | Time the daemon waits for an       |
:                                         : initial response from ssh on the   :
:                                         : target. Defaults to 50 seconds.    :
| `discovery.expire_targets`              | Determines if targets discovered   |
:                                         : should expire. Defaults to `true`  :
| `discovery.zedboot.advert_port`         | Zedboot discovery port (must be a  |
:                                         : nonzero u16)                       :
| `discovery.zedboot.enabled`             | Determines if zedboot discovery is |
:                                         : enabled. Defaults to false         :
| `emu.console.enabled`                   | The experimental flag for the      |
:                                         : console subcommand. Defaults to    :
:                                         : false.                             :
| `emu.device`                            | The default virtual device name to |
:                                         : configure the emulator. Defaults   :
:                                         : to the empty string, but can be    :
:                                         : overridden by the user.            :
| `emu.engine`                            | The default engine to launch from  |
:                                         : `ffx emu start`. Defaults to femu, :
:                                         : but can be overridden by the user. :
| `emu.gpu`                               | The default gpu type to use in     |
:                                         : `ffx emu start`. Defaults to       :
:                                         : "auto", but can be overridden by   :
:                                         : the user.                          :
| `emu.instance_dir`                      | The root directory for storing     |
:                                         : instance specific data. Instances  :
:                                         : should create a subdirectory in    :
:                                         : this directory to store data.      :
| `emu.kvm_path`                          | The filesystem path to the         |
:                                         : system's KVM device. Must be       :
:                                         : writable by the running process to :
:                                         : utilize KVM for acceleration.      :
| `emu.start.timeout`                     | The duration (in seconds) to       |
:                                         : attempt to establish an RCS        :
:                                         : connection with a new emulator     :
:                                         : before returning to the terminal.  :
:                                         : Not used in --console or           :
:                                         : ---monitor modes. Defaults to 60   :
:                                         : seconds.                           :
| `emu.upscript`                          | The full path to the script to run |
:                                         : initializing any network           :
:                                         : interfaces before starting the     :
:                                         : emulator.                          :
| `fastboot.flash.min_timeout_secs`       | The minimum flash timeout (in      |
:                                         : seconds) for flashing to a target  :
:                                         : device                             :
| `fastboot.flash.timeout_rate`           | The timeout rate in mb/s when      |
:                                         : communicating with the target      :
:                                         : device                             :
| `fastboot.reboot.reconnect_timeout`     | Timeout in seconds to wait for     |
:                                         : target after a reboot to fastboot  :
:                                         : mode                               :
| `fastboot.tcp.open.retry.count`         | Number of times to retry when      |
:                                         : connecting to a target in fastboot :
:                                         : over TCP                           :
| `fastboot.tcp.open.retry.wait`          | Time to wait for a response when   |
:                                         : connecting to a target in fastboot :
:                                         : over TCP                           :
| `fastboot.usb.disabled`                 | Disables fastboot usb discovery if |
:                                         : set to true.                       :
| `ffx.fastboot.inline_target`            | Boolean value to signal that the   |
:                                         : target is in fastboot, and to      :
:                                         : communicate directly with it as    :
:                                         : opposed to doing discovery.        :
| `log.dir`                               | Location for ffx and daemon logs   |
| `log.enabled`                           | Whether logging is enabled         |
| `log.include_spans`                     | Whether spans (function names,     |
:                                         : parameters, etc) are included      :
| `log.level`                             | Filter level for log messages      |
:                                         : Overridable on specific components :
:                                         : via `log.target_levels.<prefix>`.  :
:                                         : Values are:                        :
:                                         : `error`, `warn`, `info`, `debug`,  :
:                                         : `trace`                            :
| `log.rotate_size`                       | Limit of log size before log file  |
:                                         : is rotated (if rotation is enabled;:
:                                         : see `log.rotate_size`)             :
| `log.rotations`                         | How many rotations of log files    |
:                                         : to keep (0 to disable rotation)    :
| `log.target_levels.<prefix>`            | Filter levels for components with  :
:                                         : specified prefix. Values are:      :
:                                         : `error`, `warn`, `info`, `debug`,  :
:                                         : `trace`                            :
| `proactive_log.cache_directory`         | Location for target logs to be     |
:                                         : cached                             :
| `proactive_log.enabled`                 | Flag to enable proactive log       |
:                                         : (defaults to false)                :
| `proactive_log.max_log_size_bytes`      | Maximum log size in bytes          |
| `proactive_log.max_session_size_bytes`  | Maximum session size in bytes      |
| `proactive_log.max_sessions_per_target` | Maximum number of sessions per     |
:                                         : target                             :
| `proactive_log.symbolize.enabled`       | Configuration flag to enable the   |
:                                         : symbolizer. Defaults to false      :
| `proactive_log.symbolize.extra_args`    | Additional arguments to add to the |
:                                         : symbolizer.                        :
| `repository.repositories`               |                                    |
| `repository.registrations`              |                                    |
| `repository.default`                    |                                    |
| `repository.server.mode`                |                                    |
| `repository.server.enabled`             | If the repository server is        |
:                                         : enabled. Defaults to `false`       :
| `repository.server.listen`              |                                    |
| `repository.server.last_used_address`   |                                    |
| `ssh.auth-sock`                         | If set, the path to the            |
:                                         : authorization socket for SSH used  :
:                                         : by overnet                         :
| `target.default`                        | The default target to use if one   |
:                                         : is unspecified                     :
| `targets.manual`                        | Contains the list of manual        |
:                                         : targets                            :
| `ffx.subtool-search-paths`              | A list of paths to search for non- |
|                                         | SDK subtool binaries.              |
:                                         :                                    :
