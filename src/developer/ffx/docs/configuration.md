# Configuring FFX

Last updated 2023-01-17

The `ffx` tool supports many configuration values to tweak it's behaviour. This
is an attempt to centralize the available configuration values and document
them.

Note this document is (for now) manually updated and as such may be out of date.

When updating, please add the value in alphabetical order.

| Configuration Value                     | Documentation                      |
| --------------------------------------- | ---------------------------------- |
| `discovery.zedboot.advert_port`         | Zedboot discovery port (must be a  |
:                                         : nonzero u16)                       :
| `discovery.zedboot.enabled`             | Determines if zedboot discovery is |
:                                         : enabled. Defaults to false         :
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
| `target.default`                        | The default target to use if one   |
:                                         : is unspecified                     :
| `targets.manual`                        | Contains the list of manual        |
:                                         : targets                            :
