# Target Collection

The daemon maintains discovered and connected targets that are accessible by the `TargetCollection` fidl protocol defined in `sdk/fidl/fuchsia.developer.ffx/target.fidl`.

## Daemon Events

When a new target is discovered `DaemonEvent::NewTarget` is published to all daemon event listeners. This is currently used by the repository server to synchronize repos with a newly connected target once it establishes an RCS connection.

When a target is updated, `DaemonEvent::UpdatedTarget` is published. This is currently unused.

## Target Events

Each target has an event queue.

`TargetEvent::RcsActivated` is published when an RCS connection is established.

`TargetEvent::Rediscovered` is published when a target is updated. The name is a misnomer: while it fires when the target is discovered, it also fires when any target property is changed.

`TargetEvent::SshHostPipeErr` is published when the Overnet Host Pipe (for RCS) hits an error.

`TargetEvent::ConnectionStateChanged` is published when the connection state of a target is changed, and contains both the previous and current connection state.

## Discovery

Discovery is handled outside of Target Collection, but is the primary way targets are added to it. The current discovery mediums are: mDNS, USB, and the emulator file watcher.

mDNS discovery is both passive and active: It collects and parses mDNS packets but it also broadcasts mDNS requests.

USB discovery is passive: Periodically it reenumerates USB devices and parses the operating system's cached USB descriptor.

Emulator discovery is passive: It watches for changes to a directory `ffx emu` manages.

## Manual Targets

Manual targets are pinned within the target collection and survive across daemon restarts. Consider it an alternative form of discovery that is sourced from the config.

Manual targets are configured via `ffx target add` or `ffx config`. This is currently done by address only.

## Ephemeral Manual Targets

Ephemeral Manual targets are manual targets that are allowed to expire after a period of time. Expiry happens once the target disconnects.

Currently unused.
