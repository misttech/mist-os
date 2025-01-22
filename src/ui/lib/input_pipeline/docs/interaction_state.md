# input_pipeline > Interaction State

Reviewed on: 2025-01-14

## Purpose

User interaction state is determined by the `InteractionStateHandler`, an
`InputHandler` which reads user input activity in the form of `InputEvent`s
traveling through the `InputPipeline`.

## States

The handler is `Active` on startup and remains `Active` as it receives
InputEvents. The service transitions to `Idle` after a certain amount of time,
known as the idle transition threshold, has transpired since the last timestamp
processed by the handler, e.g. 100 milliseconds.

Products may configure this threshold using the `fuchsia.ui.IdleThresholdMs`
configuration capability.

If the service receives InputEvents after transitioning to `Idle`, it
will transition back to the `Active` state.

### State transition diagram

On startup or with events
┌──────────┐
│ ┌──────┐ │ ┌────┐
└>│Active│─┘ │Idle│
└──┬───┘ └─┬──┘
│ │
│No events for X amount of time│
│───────────────────────────────>│
│ │
│ InputEvent │
│<───────────────────────────────│
┌──┴───┐ ┌─┴──┐
│Active│ │Idle│
└──────┘ └────┘

Where X = idle transition threshold

## What counts as user interaction

`InteractionStateHandler` counts button, mouse, and touchscreen events as user
interactions. In the future, we will use `fuchsia.ui.SupportedInputDevices` to
count only input events from supported input devices.

One exception is lightsensor devices, which produce passive input data, and
therefore is not used to compute the _user_ interaction state.

## Subscribing to interaction state

Clients can subscribe to transitions in interaction state via
`fuchsia.input.interaction.Notifier/WatchState`, which follows a hanging-get
pattern.

The server will always respond immediately with the initial state, and
after that whenever the system's state changes.

### Example usage

```rust
use async_utils::hanging_get::client::HangingGetStream;
use fidl_fuchsia_input_interaction::{NotifierMarker, NotifierProxy};
use fuchsia_component::client::connect_to_protocol;

let notifier_proxy = connect_to_protocol::<NotifierMarker>()?;
let mut watch_interaction_state_stream =
    HangingGetStream::new(notifier_proxy, NotifierProxy::watch_state);

while let Some(Ok(state)) = watch_interaction_state_stream.next().await {
    match state {
        State::Active => {/* do something */},
        State::Idle => {/* do something */}
    }
}
```

## Future work

### Idleness beyond the threshold

Some clients may be interested in implementing functionality far deeper
into idleness than the InteractionStateHandler currently supports. As more
concrete use cases arise, the service could be extended to meet growing needs.
In the mean time, there are still ways the service can be used to meet certain
goals.

For example, if the InteractionStateHandler transitioned to idle 100 ms
after the last user input event, but a client wanted to do something only
after it knew that a user has been idle for 1 second, it is still possible
to use the current API to accomplish these goals.

It is recommended in this case to still subscribe to interaction state changes
via `fuchsia.input.interaction.Notifier/WatchState` and implement your own
timers beyond. One such approach might look like:

1. Start a 900 millisecond timer after receiving an `Idle` state.
2. If the service reports an `Active` state before the timer elapses,
   cancel the timer.
3. If the timer does elapse, the client can have confidence that the state
   has been idle for one second in total and do work accordingly.
