# Fake Display Coordinator Connector

The `BUILD.gn` in this directory a component package named
`fake-display-coordinator-connector`. It's a testing double of the
[`display-coordinator-connector`][display-coordinator-connector] package.

It publishes the `fuchsia.hardware.display.Provider` service that can be used
for hermetic testing without real hardware dependency.

## Introduction

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
`fake_display_coordinator_connector.shard.cml` to route required capabilities to
the `display-coordinator-connector` child.

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
    "//src/graphics/display/testing/fake-coordinator-connector/meta/fake_display_coordinator_connector.shard.cml",
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

[display-coordinator-connector]:
  /src/graphics/display/bin/coordinator-connector/
