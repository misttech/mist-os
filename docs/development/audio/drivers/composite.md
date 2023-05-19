# Composite audio driver

The `Composite` interface is a FIDL protocol exposed by audio drivers. The `Composite` interface
is generic and allows the configuration of various audio hardware types including those supported
by the `StreamConfig`, `Dai` and `Codec` FIDL interfaces. The `Composite` interface is more
generic and provides more flexible routing within audio subsystems.

In this protocol, ring buffer and DAI endpoints are configured based on the topology exposed via
the [Audio Signal Processing](signal-processing.md) APIs. In particular, endpoints represent the
hardware that is abstracted, specifically the number of ring buffers and DAI interconnects. Audio
hardware that can be represented by other audio driver types (`StreamConfig`, `Dai`, `Codec`) can
instead be represented with a `Composite` driver. For example, a `Composite` driver can represent
a `Codec` with a topology that has zero RingBuffer endpoints and one DAI interconnect endpoint.

When an audio driver provides the `Composite` interface, its client is responsible for configuring
the hardware, including data topology. Driver responsibilities include using the
`SignalProcessing` protocol to enumerate the topologies and capabilities supported by the hardware
it abstracts.

The `Composite` FIDL protocol is defined at
[composite.fidl](/sdk/fidl/fuchsia.hardware.audio/composite.fidl).


