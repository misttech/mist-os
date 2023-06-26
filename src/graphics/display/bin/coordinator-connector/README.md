# Display Coordinator Connector

The `BUILD.gn` in this directory generates a component package named
`display-coordinator-connector`

It publishes the `fuchsia.hardware.display.Provider` service used for production
environments which contents are displayed on the display hardware.

## Introduction

The goal of `display-coordinator-connector` is to let components connect to the
`fuchsia.hardware.display.Coordinator` service, provided by the display
coordinator driver (available in `/dev/class/display-coordinator`) so that
components can present contents to the display hardware.

It serves as a bridge to allow components to access the Coordinator service
without having direct access to the `/dev/class/display-coordinator` directory.

`display-coordinator-connector` only supports a single client connection at any
given time. Consequently, two instances of Scenic (or any other display clients)
cannot be run simultaneously.

An extra hop between the display client and the display coordinator occurs when
`display-coordinator-connector` is used to access the Coordinator service.
However, the time overhead is negligible and is constant (in the number of
display open requests) for most of the display clients (like Scenic).

## Usage

Realms should declare a child component in its component manifest for the
display coordinator connector. The child must be named
`display-coordinator-connector` to make its capabilities be correctly routed.

They should also include the corresponding shard component manifest file
`display_coordinator_connector.shard.cml` to route required capabilities to the
`display-coordinator-connector` child.

They should also explicitly offer the `fuchsia.hardware.display.Provider`
service from `display-coordinator-connector` to clients (for example, Scenic).

For example, here is an excerpt of the `ui` component manifest defined in
`//src/ui/meta/ui.cml` declaring a real display coordinator connector from the
global package URL, offering parent capabilities to the display coordinator
connector, and providing the `fuchsia.hardware.display.Provider` service to
Scenic:

```json5
{
  include: [
    "//src/graphics/display/bin/coordinator-connector/meta/display_coordinator_connector.shard.cml",
  ],
  children: [
    {
      name: "display-coordinator-connector",
      url: "fuchsia-pkg://fuchsia.com/display-coordinator-provider#meta/hdcp.cm",
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
