# Driver Logging

Caution: This page may contain information that is specific to the legacy
version of the driver framework (DFv1).

You can have a driver send log messages to the
[syslog](/docs/development/diagnostics/logs/recording.md) through the use of the
`zxlogf(<log_level>,...)` macro, which is defined in
[lib/ddk/debug.h](/src/lib/ddk/include/lib/ddk/debug.h).

Depending on the type of log level, by default, log messages are sent to the
following logs:

* [syslog](/docs/development/diagnostics/logs/recording.md#logsinksyslog):
  * `ERROR`
  * `WARNING`
  * `INFO`

To control which log levels are sent to the `syslog`, you can use the
[assembly platform configuration](/reference/assembly/DiagnosticsConfig)
. An assembly platform configuration can be specified in either the product or
via assembly overrides.

For example, you can make a `//local/BUILD.gn` file:

 ```
# //local/BUILD.gn:
assembly_developer_overrides("sdhci-debug-logs") {
    platform = {
        diagnostics = {
            component_log_initial_interests = [
                {
                    component = "fuchsia-boot:///sdhci#meta/sdhci.cm"
                    log_severity = "debug"
                },
            ]
        }
    }
}
```

Then use `fx set` to set the `assembly-override`:

```
fx set <product.board> --assembly-override=//local:sdhci-debug-logs
```

Additionally, this enables `DEBUG` and `TRACE` logs for the sdhci driver, as we
are setting a _minimum_ log level, and `TRACE` is lower than `DEBUG`.

A driver's logs are tagged with the process name, "driver", and the driver name.
This can be used to filter the output of the syslog whilst searching for
particular logs.

Note: There is both producer and consumer filtering applied to logs. The above
covers the producer-side, for more information on the consumer-side, and on how
to view driver logs, see
[viewing logs](/docs/development/diagnostics/logs/viewing.md).
