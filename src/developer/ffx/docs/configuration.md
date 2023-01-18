# Configuring FFX

Last updated 2023-01-17

The `ffx` tool supports many configuration values to tweak it's behaviour. This
is an attempt to centralize the available configuration values and document
them.

Note this document is (for now) manually updated and as such may be out of date.

When updating, please add the value in alphabetical order.

| Configuration Value | Documentation |
| ------------------- | ------------- |
| fastboot.flash.min_timeout_secs | The minimum flash timeout (in seconds) for flashing to a target device |
| fastboot.flash.timeout_rate |  The timeout rate in mb/s when communicating with the target device |
| fastboot.reboot.reconnect_timeout | Timeout in seconds to wait for target after a reboot to fastboot mode |
| fastboot.tcp.open.retry.count | Number of times to retry when connecting to a target in fastboot over TCP |
| fastboot.tcp.open.retry.wait | Time to wait for a response when connecting to a target in fastboot over TCP |
| fastboot.usb.disabled |  Disables fastboot usb discovery if set to true. |
| target.default      | The default target to use if one is unspecified |

