# Display Coordinator Connector

The `BUILD.gn` in this directory generates two component packages,
`display-coordinator-connector` and `fake-display-coordinator-connector`.

Both of these publish the `fuchsia.hardware.display.Provider` service. In
general, the "real" one (without "fake-" prefix) is used for production
environments which contents are displayed on the display hardware, while the
"fake" one is used for hermetic testing without real hardware dependency.

## "Real" Display Coordinator Connector

The goal of `display-coordinator-connector` is to let components connect to the
`fuchsia.hardware.display.Coordinator` service, provided by the display
coordinator driver (available in `/dev/class/display-coordinator`) so that
components can present contents to the display hardware.

It serves as a bridge to allow components to access the Coordinator service without
having direct access to the `/dev/class/display-coordinator` directory.

`display-coordinator-connector` only supports a single client connection at any
given time. Consequently, two instances of Scenic (or any other display clients)
cannot be run simultaneously.

An extra hop between the display client and the display coordinator occurs when
`display-coordinator-connector` is used to access the Coordinator service.
However, the time overhead is negligible and is constant (in the number of
display open requests) for most of the display clients (like Scenic).

## Fake Display Coordinator Connector

The goal of `fake-display-coordinator-connector` is to provide a display engine
driver (`fake-display`) and a display coordinator driver within a hermetic
testing environment. This removes the dependency on acquiring the display
coordinator, allowing tests to run in display-less environments hermetically and
allowing multiple display coordinator and display driver instances to co-exist.

## Usage

Realms should declare a child component in its component manifest for the
display coordinator connector. The child must be named
`display-coordinator-connector` to make its capabilities be correctly routed.

They should also include the corresponding shard component manifest file
(`display_coordinator_connector.shard.cml` for the real connector, or
`fake_display_coordinator_connector.shard.cml` for the fake connector) to route
required capabilities to the `display-coordinator-connector` child.

They should also explicitly offer the `fuchsia.hardware.display.Provider`
service from `display-coordinator-connector` to clients (for example, Scenic).

For example, here is an excerpt of `ui_test_realm`
(`//src/ui/testing/ui_test_realm/meta/scenic.shard.cml`) declaring a fake
display coordinator connector from its own package, offering parent capabilities
to the display coordinator connector, and providing the
`fuchsia.hardware.display.Provider` service to Scenic:

```json5
{
  include: [
    "//src/graphics/display/testing/coordinator-connector/meta/fake_display_coordinator_connector.shard.cml",
  ],
  children: [
    {
      name: "display-coordinator-connector",
      url: "#meta/display-coordinator-connector.cm",
    },
  ],
  offer: [
    {
      protocol: ["fuchsia.hardware.display.Provider"],
      from: "#display-coordinator-connector",
      to: ["#scenic"],
    },
  ],
}
```

For realms using display coordinator connector, see the `ui` component manifest
defined in `//src/ui/meta/ui.cml` for example.
