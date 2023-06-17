# Display Coordinator Provider

The `BUILD.gn` in this directory generates two component packages,
`display-coordinator-provider` and `fake-display-coordinator-provider`.

Both of these publish the `fuchsia.hardware.display.Provider` service. In
general, the "real" one (without "fake-" prefix) is used for production
environments which contents are displayed on the display hardware, while the
"fake" one is used for hermetic testing without real hardware dependency.

## "Real" Display Coordinator Provider

The goal of `display-coordinator-provider` is to let components connect to the
`fuchsia.hardware.display.Coordinator` service, provided by the display
coordinator driver (available in `/dev/class/display-coordinator`) so that
components can present contents to the display hardware.

It serves as a bridge to allow components to access the Coordinator service without
having direct access to the `/dev/class/display-coordinator` directory.

`display-coordinator-provider` only supports a single client connection at any
given time. Consequently, two instances of Scenic (or any other display clients)
cannot be run simultaneously.

An extra hop between the display client and the display coordinator occurs when
`display-coordinator-provider` is used to access the Coordinator service.
However, the time overhead is negligible and is constant (in the number of
display open requests) for most of the display clients (like Scenic).

## Fake Display Coordinator Provider

The goal of `fake-display-coordinator-provider` is to provide a display engine
driver (`fake-display`) and a display coordinator driver within a hermetic
testing environment. This removes the dependency on acquiring the display
coordinator, allowing tests to run in display-less environments hermetically and
allowing multiple display coordinator and display driver instances to co-exist.

## Usage

Realms should declare a child component in its component manifest for the
display coordinator provider. The child must be named
`display-coordinator-provider` to make its capabilities be correctly routed.

They should also include the corresponding shard component manifest file
(`display_coordinator_provider.shard.cml` for the real provider, or
`fake_display_coordinator_provider.shard.cml` for the fake provider) to route
required capabilities to the `display-coordinator-provider` child.

They should also explicitly offer the `fuchsia.hardware.display.Provider`
service from `display-coordinator-provider` to clients (for example, Scenic).

For example, here is an excerpt of `ui_test_realm`
(`//src/ui/testing/ui_test_realm/meta/scenic.shard.cml`) declaring a fake
display coordinator provider from its own package, offering parent capabilities
to the display coordinator provider, and providing the
`fuchsia.hardware.display.Provider` service to Scenic:

```json5
{
  include: [
    "//src/graphics/display/testing/coordinator-provider/meta/fake_display_coordinator_provider.shard.cml",
  ],
  children: [
    {
      name: "display-coordinator-provider",
      url: "#meta/hdcp.cm",
    },
  ],
  offer: [
    {
      protocol: ["fuchsia.hardware.display.Provider"],
      from: "#display-coordinator-provider",
      to: ["#scenic"],
    },
  ],
}
```

For realms using display coordinator provider, see the `ui` component manifest
defined in `//src/ui/meta/ui.cml` for example.
