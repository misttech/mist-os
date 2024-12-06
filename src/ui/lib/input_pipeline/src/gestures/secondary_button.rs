// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use super::gesture_arena::{
    self, DetailedReasonFloat, DetailedReasonUint, EndGestureEvent, ExamineEventResult,
    ProcessBufferedEventsResult, ProcessNewEventResult, Reason, RecognizedGesture, TouchpadEvent,
    VerifyEventResult,
};
use super::utils::{movement_from_events, MovementDetail};
use crate::mouse_binding::{MouseButton, MouseEvent, MouseLocation, MousePhase, RelativeLocation};
use crate::utils::{euclidean_distance, Position};

use maplit::hashset;
use std::collections::HashSet;

/// The initial state of this recognizer, before a secondary tap has been
/// detected.
#[derive(Debug)]
pub(super) struct InitialContender {
    /// The maximum displacement that a detected finger can withstand to still
    /// be considered a secondary click. Measured in millimeters.
    pub(super) spurious_to_intentional_motion_threshold_mm: f32,

    /// Use a larger threshold to detect motion on the edge of contact-button down,
    /// button down-button up
    pub(super) spurious_to_intentional_motion_threshold_button_change_mm: f32,

    /// The timeout of the edge of contact-button down, button down-button up,
    /// The recognizer will leave edge state either timeout or motion detected.
    pub(super) button_change_state_timeout: zx::MonotonicDuration,
}

/// The state when this recognizer has detected a single finger down.
#[derive(Debug)]
struct OneFingerContactContender {
    /// The TouchpadEvent when a finger down was first detected.
    one_finger_contact_event: TouchpadEvent,

    /// The maximum displacement that a detected finger can withstand to still
    /// be considered a tap. Measured in millimeters.
    spurious_to_intentional_motion_threshold_mm: f32,

    /// Use a larger threshold to detect motion on the edge of contact-button down,
    /// button down-button up
    spurious_to_intentional_motion_threshold_button_change_mm: f32,

    /// The timeout of the edge of contact-button down, button down-button up,
    /// The recognizer will leave edge state either timeout or motion detected.
    button_change_state_timeout: zx::MonotonicDuration,
}

/// The state when this recognizer has detected two fingers down.
#[derive(Debug)]
struct TwoFingerContactContender {
    /// The TouchpadEvent when two fingers were first detected.
    two_finger_contact_event: TouchpadEvent,

    /// The maximum displacement that a detected finger can withstand to still
    /// be considered a tap. Measured in millimeters.
    spurious_to_intentional_motion_threshold_mm: f32,

    /// Use a larger threshold to detect motion on the edge of contact-button down,
    /// button down-button up
    spurious_to_intentional_motion_threshold_button_change_mm: f32,

    /// The timeout of the edge of contact-button down, button down-button up,
    /// The recognizer will leave edge state either timeout or motion detected.
    button_change_state_timeout: zx::MonotonicDuration,
}

/// The state when this recognizer has detected a secondary button down, but the
/// gesture arena has not declared this recognizer the winner.
#[derive(Debug)]
struct MatchedContender {
    /// The TouchpadEvent when two fingers and button were first detected.
    pressed_event: TouchpadEvent,

    /// The maximum displacement that a detected finger can withstand to still
    /// be considered a tap. Measured in millimeters.
    spurious_to_intentional_motion_threshold_mm: f32,

    /// Use a larger threshold to detect motion on the edge of contact-button down,
    /// button down-button up
    spurious_to_intentional_motion_threshold_button_change_mm: f32,

    /// The timeout of the edge of contact-button down, button down-button up,
    /// The recognizer will leave edge state either timeout or motion detected.
    button_change_state_timeout: zx::MonotonicDuration,
}

/// The state when this recognizer has won the contest.
#[derive(Debug)]
struct ButtonDownWinner {
    /// The TouchpadEvent when two fingers and button were first detected.
    pressed_event: TouchpadEvent,

    /// The threshold to detect motion.
    spurious_to_intentional_motion_threshold_mm: f32,

    /// Use a larger threshold to detect motion on the edge of contact-button down,
    /// button down-button up
    spurious_to_intentional_motion_threshold_button_change_mm: f32,

    /// The timeout of the edge of contact-button down, button down-button up,
    /// The recognizer will leave edge state either timeout or motion detected.
    button_change_state_timeout: zx::MonotonicDuration,
}

/// The state when ButtonDownWinner got motion more than threshold to recognize as
/// drag gesture.
#[derive(Debug)]
struct DragWinner {
    /// The last TouchpadEvent.
    last_event: TouchpadEvent,

    /// Use a larger threshold to detect motion on the edge of contact-button down,
    /// button down-button up
    spurious_to_intentional_motion_threshold_button_change_mm: f32,

    /// The timeout of the edge of contact-button down, button down-button up,
    /// The recognizer will leave edge state either timeout or motion detected.
    button_change_state_timeout: zx::MonotonicDuration,
}

/// The state when ButtonDownWinner / DragWinner got button up, this winner is
/// used to discard tailing movement from button up.
#[derive(Debug)]
struct ButtonUpWinner {
    /// The button up event.
    button_up_event: TouchpadEvent,

    /// Use a larger threshold to detect motion on the edge of contact-button down,
    /// button down-button up
    spurious_to_intentional_motion_threshold_button_change_mm: f32,

    /// The timeout of the edge of contact-button down, button down-button up,
    /// The recognizer will leave edge state either timeout or motion detected.
    button_change_state_timeout: zx::MonotonicDuration,
}

impl InitialContender {
    #[allow(clippy::boxed_local, reason = "mass allow for https://fxbug.dev/381896734")]
    fn into_one_finger_contact_contender(
        self: Box<Self>,
        one_finger_contact_event: TouchpadEvent,
    ) -> Box<dyn gesture_arena::Contender> {
        Box::new(OneFingerContactContender {
            one_finger_contact_event,
            spurious_to_intentional_motion_threshold_mm: self
                .spurious_to_intentional_motion_threshold_mm,
            spurious_to_intentional_motion_threshold_button_change_mm: self
                .spurious_to_intentional_motion_threshold_button_change_mm,
            button_change_state_timeout: self.button_change_state_timeout,
        })
    }

    #[allow(clippy::boxed_local, reason = "mass allow for https://fxbug.dev/381896734")]
    fn into_two_finger_contacts_contender(
        self: Box<Self>,
        two_finger_contact_event: TouchpadEvent,
    ) -> Box<dyn gesture_arena::Contender> {
        Box::new(TwoFingerContactContender {
            two_finger_contact_event,
            spurious_to_intentional_motion_threshold_mm: self
                .spurious_to_intentional_motion_threshold_mm,
            spurious_to_intentional_motion_threshold_button_change_mm: self
                .spurious_to_intentional_motion_threshold_button_change_mm,
            button_change_state_timeout: self.button_change_state_timeout,
        })
    }

    #[allow(clippy::boxed_local, reason = "mass allow for https://fxbug.dev/381896734")]
    fn into_matched_contender(
        self: Box<Self>,
        pressed_event: TouchpadEvent,
    ) -> Box<dyn gesture_arena::MatchedContender> {
        Box::new(MatchedContender {
            pressed_event,
            spurious_to_intentional_motion_threshold_mm: self
                .spurious_to_intentional_motion_threshold_mm,
            spurious_to_intentional_motion_threshold_button_change_mm: self
                .spurious_to_intentional_motion_threshold_button_change_mm,
            button_change_state_timeout: self.button_change_state_timeout,
        })
    }
}

impl gesture_arena::Contender for InitialContender {
    fn examine_event(self: Box<Self>, event: &TouchpadEvent) -> ExamineEventResult {
        let num_pressed_buttons = event.pressed_buttons.len();

        let num_contacts = event.contacts.len();
        match num_contacts {
            1 => {
                if num_pressed_buttons != 0 {
                    return ExamineEventResult::Mismatch(Reason::DetailedUint(
                        DetailedReasonUint {
                            criterion: "num_pressed_buttons",
                            min: Some(0),
                            max: Some(0),
                            actual: num_pressed_buttons,
                        },
                    ));
                }

                ExamineEventResult::Contender(self.into_one_finger_contact_contender(event.clone()))
            }
            2 => {
                match num_pressed_buttons {
                    0 => ExamineEventResult::Contender(
                        self.into_two_finger_contacts_contender(event.clone()),
                    ),
                    // Currently we don't have hardware with more than 1 button in
                    // touchpad, ignore this case.
                    _ => ExamineEventResult::MatchedContender(
                        self.into_matched_contender(event.clone()),
                    ),
                }
            }
            0 | _ => ExamineEventResult::Mismatch(Reason::DetailedUint(DetailedReasonUint {
                criterion: "num_contacts",
                min: Some(1),
                max: Some(2),
                actual: num_contacts,
            })),
        }
    }
}

impl OneFingerContactContender {
    #[allow(clippy::boxed_local, reason = "mass allow for https://fxbug.dev/381896734")]
    fn into_two_finger_contacts_contender(
        self: Box<Self>,
        two_finger_contact_event: TouchpadEvent,
    ) -> Box<dyn gesture_arena::Contender> {
        Box::new(TwoFingerContactContender {
            two_finger_contact_event,
            spurious_to_intentional_motion_threshold_mm: self
                .spurious_to_intentional_motion_threshold_mm,
            spurious_to_intentional_motion_threshold_button_change_mm: self
                .spurious_to_intentional_motion_threshold_button_change_mm,
            button_change_state_timeout: self.button_change_state_timeout,
        })
    }
}

impl gesture_arena::Contender for OneFingerContactContender {
    fn examine_event(self: Box<Self>, event: &TouchpadEvent) -> ExamineEventResult {
        let num_pressed_buttons = event.pressed_buttons.len();
        if num_pressed_buttons > 0 {
            return ExamineEventResult::Mismatch(Reason::DetailedUint(DetailedReasonUint {
                criterion: "num_pressed_buttons",
                min: None,
                max: Some(0),
                actual: num_pressed_buttons,
            }));
        }

        let num_contacts = event.contacts.len();

        match num_contacts {
            1 => {
                let displacement_mm = euclidean_distance(
                    position_from_event(event, 0),
                    position_from_event(&self.one_finger_contact_event, 0),
                );
                if displacement_mm >= self.spurious_to_intentional_motion_threshold_mm {
                    return ExamineEventResult::Mismatch(Reason::DetailedFloat(
                        DetailedReasonFloat {
                            criterion: "displacement_mm",
                            min: None,
                            max: Some(self.spurious_to_intentional_motion_threshold_mm),
                            actual: displacement_mm,
                        },
                    ));
                }
                ExamineEventResult::Contender(self)
            }
            2 => {
                let displacement_mm = euclidean_distance(
                    position_from_event(event, 0),
                    position_from_event(&self.one_finger_contact_event, 0),
                );
                if displacement_mm >= self.spurious_to_intentional_motion_threshold_mm {
                    return ExamineEventResult::Mismatch(Reason::DetailedFloat(
                        DetailedReasonFloat {
                            criterion: "displacement_mm",
                            min: None,
                            max: Some(self.spurious_to_intentional_motion_threshold_mm),
                            actual: displacement_mm,
                        },
                    ));
                }
                ExamineEventResult::Contender(
                    self.into_two_finger_contacts_contender(event.clone()),
                )
            }
            _ => ExamineEventResult::Mismatch(Reason::DetailedUint(DetailedReasonUint {
                criterion: "num_contacts",
                min: Some(1),
                max: Some(2),
                actual: num_contacts,
            })),
        }
    }
}

impl TwoFingerContactContender {
    #[allow(clippy::boxed_local, reason = "mass allow for https://fxbug.dev/381896734")]
    fn into_matched_contender(
        self: Box<Self>,
        pressed_event: TouchpadEvent,
    ) -> Box<dyn gesture_arena::MatchedContender> {
        Box::new(MatchedContender {
            pressed_event,
            spurious_to_intentional_motion_threshold_mm: self
                .spurious_to_intentional_motion_threshold_mm,
            spurious_to_intentional_motion_threshold_button_change_mm: self
                .spurious_to_intentional_motion_threshold_button_change_mm,
            button_change_state_timeout: self.button_change_state_timeout,
        })
    }
}

impl gesture_arena::Contender for TwoFingerContactContender {
    fn examine_event(self: Box<Self>, event: &TouchpadEvent) -> ExamineEventResult {
        let num_contacts = event.contacts.len();
        if num_contacts != 2 {
            return ExamineEventResult::Mismatch(Reason::DetailedUint(DetailedReasonUint {
                criterion: "num_contacts",
                min: Some(2),
                max: Some(2),
                actual: num_contacts,
            }));
        }

        // both touch contact should not move > threshold.
        let MovementDetail { euclidean_distance, movement: _ } =
            movement_from_events(&self.two_finger_contact_event, &event);
        if euclidean_distance >= self.spurious_to_intentional_motion_threshold_mm {
            return ExamineEventResult::Mismatch(Reason::DetailedFloat(DetailedReasonFloat {
                criterion: "displacement_mm",
                min: None,
                max: Some(self.spurious_to_intentional_motion_threshold_mm),
                actual: euclidean_distance,
            }));
        }

        let num_pressed_buttons = event.pressed_buttons.len();
        match num_pressed_buttons {
            // No button down, keep matching.
            0 => ExamineEventResult::Contender(self),
            // If button down, into matched.
            // Currently we don't have hardware with more than 1 button in
            // touchpad, ignore this case.
            _ => ExamineEventResult::MatchedContender(self.into_matched_contender(event.clone())),
        }
    }
}

impl MatchedContender {
    #[allow(clippy::boxed_local, reason = "mass allow for https://fxbug.dev/381896734")]
    fn into_button_down_winner(self: Box<Self>) -> Box<dyn gesture_arena::Winner> {
        Box::new(ButtonDownWinner {
            pressed_event: self.pressed_event,
            spurious_to_intentional_motion_threshold_mm: self
                .spurious_to_intentional_motion_threshold_mm,
            spurious_to_intentional_motion_threshold_button_change_mm: self
                .spurious_to_intentional_motion_threshold_button_change_mm,
            button_change_state_timeout: self.button_change_state_timeout,
        })
    }
}

impl gesture_arena::MatchedContender for MatchedContender {
    fn verify_event(self: Box<Self>, _event: &TouchpadEvent) -> VerifyEventResult {
        // This verify_event expected not call because all other recognizers
        // should exit on 2 finger button down.

        tracing::error!("Unexpected MatchedContender::verify_event() called");

        VerifyEventResult::MatchedContender(self)
    }

    fn process_buffered_events(
        self: Box<Self>,
        _events: Vec<TouchpadEvent>,
    ) -> ProcessBufferedEventsResult {
        // all small motion before button down are ignored.
        ProcessBufferedEventsResult {
            generated_events: vec![touchpad_event_to_mouse_down_event(&self.pressed_event)],
            winner: Some(self.into_button_down_winner()),
            recognized_gesture: RecognizedGesture::SecondaryButtonDown,
        }
    }
}

impl ButtonDownWinner {
    #[allow(clippy::boxed_local, reason = "mass allow for https://fxbug.dev/381896734")]
    fn into_drag_winner(self: Box<Self>) -> Box<dyn gesture_arena::Winner> {
        Box::new(DragWinner {
            last_event: self.pressed_event,
            spurious_to_intentional_motion_threshold_button_change_mm: self
                .spurious_to_intentional_motion_threshold_button_change_mm,
            button_change_state_timeout: self.button_change_state_timeout,
        })
    }

    #[allow(clippy::boxed_local, reason = "mass allow for https://fxbug.dev/381896734")]
    fn into_button_up(
        self: Box<Self>,
        button_up_event: TouchpadEvent,
    ) -> Box<dyn gesture_arena::Winner> {
        Box::new(ButtonUpWinner {
            button_up_event,
            spurious_to_intentional_motion_threshold_button_change_mm: self
                .spurious_to_intentional_motion_threshold_button_change_mm,
            button_change_state_timeout: self.button_change_state_timeout,
        })
    }
}

impl gesture_arena::Winner for ButtonDownWinner {
    fn process_new_event(self: Box<Self>, event: TouchpadEvent) -> ProcessNewEventResult {
        let motion_threshold =
            if event.timestamp - self.pressed_event.timestamp > self.button_change_state_timeout {
                self.spurious_to_intentional_motion_threshold_mm
            } else {
                self.spurious_to_intentional_motion_threshold_button_change_mm
            };

        // Check for drag (button held, with sufficient contact movement).
        let MovementDetail { euclidean_distance, movement: _ } =
            movement_from_events(&self.pressed_event, &event);
        if euclidean_distance >= motion_threshold {
            let drag_winner = self.into_drag_winner();
            return drag_winner.process_new_event(event);
        }

        let num_pressed_buttons = event.pressed_buttons.len();
        match num_pressed_buttons {
            // All small motion before button up, and motion in button up event
            // are ignored.
            0 => ProcessNewEventResult::ContinueGesture(
                Some(touchpad_event_to_mouse_up_event(&event)),
                self.into_button_up(event),
            ),
            1 => ProcessNewEventResult::ContinueGesture(None, self),
            // Also wait for the button release to complete the click or drag gesture.
            // this should never happens unless there is a touchpad has more than 1 button.
            _ => ProcessNewEventResult::ContinueGesture(None, self),
        }
    }
}

impl DragWinner {
    #[allow(clippy::boxed_local, reason = "mass allow for https://fxbug.dev/381896734")]
    fn into_drag_winner(
        self: Box<Self>,
        last_event: TouchpadEvent,
    ) -> Box<dyn gesture_arena::Winner> {
        Box::new(DragWinner {
            spurious_to_intentional_motion_threshold_button_change_mm: self
                .spurious_to_intentional_motion_threshold_button_change_mm,
            button_change_state_timeout: self.button_change_state_timeout,
            last_event,
        })
    }

    #[allow(clippy::boxed_local, reason = "mass allow for https://fxbug.dev/381896734")]
    fn into_button_up(
        self: Box<Self>,
        button_up_event: TouchpadEvent,
    ) -> Box<dyn gesture_arena::Winner> {
        Box::new(ButtonUpWinner {
            spurious_to_intentional_motion_threshold_button_change_mm: self
                .spurious_to_intentional_motion_threshold_button_change_mm,
            button_change_state_timeout: self.button_change_state_timeout,
            button_up_event,
        })
    }
}

impl gesture_arena::Winner for DragWinner {
    fn process_new_event(self: Box<Self>, event: TouchpadEvent) -> ProcessNewEventResult {
        let num_pressed_buttons = event.pressed_buttons.len();
        match num_pressed_buttons {
            // Motion in button up event is ignored.
            0 => ProcessNewEventResult::ContinueGesture(
                Some(touchpad_event_to_mouse_up_event(&event)),
                self.into_button_up(event),
            ),
            1 | _ => {
                // More than 2 button should never happens unless there is a touchpad has
                // more than 1 button. Just treat this same with 1 button down.
                ProcessNewEventResult::ContinueGesture(
                    Some(touchpad_event_to_mouse_drag_event(&self.last_event, &event)),
                    self.into_drag_winner(event),
                )
            }
        }
    }
}

impl gesture_arena::Winner for ButtonUpWinner {
    fn process_new_event(self: Box<Self>, event: TouchpadEvent) -> ProcessNewEventResult {
        // Fingers leave or add to surface should end the ButtonUpWinner.
        let num_contacts = event.contacts.len();
        let num_contacts_button_up = self.button_up_event.contacts.len();
        if num_contacts != num_contacts_button_up {
            return ProcessNewEventResult::EndGesture(
                EndGestureEvent::UnconsumedEvent(event),
                Reason::DetailedUint(DetailedReasonUint {
                    criterion: "num_contacts",
                    min: Some(num_contacts_button_up as u64),
                    max: Some(num_contacts_button_up as u64),
                    actual: num_contacts,
                }),
            );
        }

        // Button change should end the ButtonUpWinner.
        let num_pressed_buttons = event.pressed_buttons.len();
        if num_pressed_buttons != 0 {
            return ProcessNewEventResult::EndGesture(
                EndGestureEvent::UnconsumedEvent(event),
                Reason::DetailedUint(DetailedReasonUint {
                    criterion: "num_buttons",
                    min: Some(0),
                    max: Some(0),
                    actual: num_pressed_buttons,
                }),
            );
        }

        // Events after timeout should not be discarded by ButtonUpWinner.
        if event.timestamp - self.button_up_event.timestamp > self.button_change_state_timeout {
            return ProcessNewEventResult::EndGesture(
                EndGestureEvent::UnconsumedEvent(event),
                Reason::Basic("button_up_timeout"),
            );
        }

        // Events move more than threshold should end the ButtonUpWinner.
        let MovementDetail { euclidean_distance, movement: _ } =
            movement_from_events(&self.button_up_event, &event);
        if euclidean_distance > self.spurious_to_intentional_motion_threshold_button_change_mm {
            return ProcessNewEventResult::EndGesture(
                EndGestureEvent::UnconsumedEvent(event),
                Reason::DetailedFloat(DetailedReasonFloat {
                    criterion: "displacement_mm",
                    min: None,
                    max: Some(self.spurious_to_intentional_motion_threshold_button_change_mm),
                    actual: euclidean_distance,
                }),
            );
        }

        // Discard this event.
        ProcessNewEventResult::ContinueGesture(None, self)
    }
}

/// This function returns the position associated with the touch contact at the
/// given index from a TouchpadEvent.
fn position_from_event(event: &TouchpadEvent, index: usize) -> Position {
    event.contacts[index].position
}

fn touchpad_event_to_mouse_down_event(event: &TouchpadEvent) -> gesture_arena::MouseEvent {
    make_mouse_event(
        event.timestamp,
        Position::zero(),
        MousePhase::Down,
        hashset! {gesture_arena::SECONDARY_BUTTON},
        hashset! {gesture_arena::SECONDARY_BUTTON},
    )
}

fn touchpad_event_to_mouse_up_event(event: &TouchpadEvent) -> gesture_arena::MouseEvent {
    make_mouse_event(
        event.timestamp,
        Position::zero(),
        MousePhase::Up,
        hashset! {gesture_arena::SECONDARY_BUTTON},
        hashset! {},
    )
}

fn touchpad_event_to_mouse_drag_event(
    last_event: &TouchpadEvent,
    event: &TouchpadEvent,
) -> gesture_arena::MouseEvent {
    let MovementDetail { movement, euclidean_distance: _ } =
        movement_from_events(last_event, event);
    make_mouse_event(
        event.timestamp,
        movement,
        MousePhase::Move,
        hashset! {},
        hashset! {gesture_arena::SECONDARY_BUTTON},
    )
}

fn make_mouse_event(
    timestamp: zx::MonotonicInstant,
    movement_in_mm: Position,
    phase: MousePhase,
    affected_buttons: HashSet<MouseButton>,
    pressed_buttons: HashSet<MouseButton>,
) -> gesture_arena::MouseEvent {
    gesture_arena::MouseEvent {
        timestamp,
        mouse_data: MouseEvent::new(
            MouseLocation::Relative(RelativeLocation { millimeters: movement_in_mm }),
            /* wheel_delta_v= */ None,
            /* wheel_delta_h= */ None,
            phase,
            affected_buttons,
            pressed_buttons,
            /* is_precision_scroll= */ None,
        ),
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::touch_binding;
    use assert_matches::assert_matches;
    use test_case::test_case;

    fn make_touch_contact(id: u32, position: Position) -> touch_binding::TouchContact {
        touch_binding::TouchContact { id, position, pressure: None, contact_size: None }
    }

    const SPURIOUS_TO_INTENTIONAL_MOTION_THRESHOLD_MM: f32 = 10.0;
    const SPURIOUS_TO_INTENTIONAL_MOTION_THRESHOLD_BUTTON_CHANGE_MM: f32 = 20.0;
    const BUTTON_CHANGE_STATE_TIMEOUT: zx::MonotonicDuration =
        zx::MonotonicDuration::from_seconds(1);

    #[test_case(TouchpadEvent{
        timestamp: zx::MonotonicInstant::ZERO,
        pressed_buttons: vec![],
        contacts: vec![],
        filtered_palm_contacts: vec![],
    };"0 fingers")]
    #[test_case(TouchpadEvent{
        timestamp: zx::MonotonicInstant::ZERO,
        pressed_buttons: vec![1],
        contacts: vec![
            make_touch_contact(1, Position{x: 1.0, y: 1.0}),
        ],
        filtered_palm_contacts: vec![],
    };"1 fingers button down")]
    #[test_case(TouchpadEvent{
        timestamp: zx::MonotonicInstant::ZERO,
        pressed_buttons: vec![],
        contacts: vec![
            make_touch_contact(1, Position{x: 1.0, y: 1.0}),
            make_touch_contact(2, Position{x: 5.0, y: 5.0}),
            make_touch_contact(3, Position{x: 10.0, y: 10.0}),
        ],
        filtered_palm_contacts: vec![],
    };"3 fingers")]
    #[fuchsia::test]
    fn initial_contender_examine_event_mismatch(event: TouchpadEvent) {
        let contender: Box<dyn gesture_arena::Contender> = Box::new(InitialContender {
            spurious_to_intentional_motion_threshold_mm:
                SPURIOUS_TO_INTENTIONAL_MOTION_THRESHOLD_MM,
            spurious_to_intentional_motion_threshold_button_change_mm:
                SPURIOUS_TO_INTENTIONAL_MOTION_THRESHOLD_BUTTON_CHANGE_MM,
            button_change_state_timeout: BUTTON_CHANGE_STATE_TIMEOUT,
        });

        let got = contender.examine_event(&event);
        assert_matches!(got, ExamineEventResult::Mismatch(_));
    }

    #[test_case(TouchpadEvent{
        timestamp: zx::MonotonicInstant::ZERO,
        pressed_buttons: vec![],
        contacts: vec![
            make_touch_contact(1, Position{x: 1.0, y: 1.0}),
        ],
        filtered_palm_contacts: vec![],
    }, "input_pipeline_lib_test::gestures::secondary_button::OneFingerContactContender";"1 fingers")]
    #[test_case(TouchpadEvent{
        timestamp: zx::MonotonicInstant::ZERO,
        pressed_buttons: vec![],
        contacts: vec![
            make_touch_contact(1, Position{x: 1.0, y: 1.0}),
            make_touch_contact(2, Position{x: 5.0, y: 5.0}),
        ],
        filtered_palm_contacts: vec![],
    }, "input_pipeline_lib_test::gestures::secondary_button::TwoFingerContactContender";"2 fingers")]
    #[fuchsia::test]
    fn initial_contender_examine_event_contender(event: TouchpadEvent, contender_name: &str) {
        let contender: Box<dyn gesture_arena::Contender> = Box::new(InitialContender {
            spurious_to_intentional_motion_threshold_mm:
                SPURIOUS_TO_INTENTIONAL_MOTION_THRESHOLD_MM,
            spurious_to_intentional_motion_threshold_button_change_mm:
                SPURIOUS_TO_INTENTIONAL_MOTION_THRESHOLD_BUTTON_CHANGE_MM,
            button_change_state_timeout: BUTTON_CHANGE_STATE_TIMEOUT,
        });

        let got = contender.examine_event(&event);
        assert_matches!(got, ExamineEventResult::Contender(contender) => {
            pretty_assertions::assert_eq!(contender.get_type_name(), contender_name);
        });
    }

    #[fuchsia::test]
    fn initial_contender_examine_event_matched_contender() {
        let contender: Box<dyn gesture_arena::Contender> = Box::new(InitialContender {
            spurious_to_intentional_motion_threshold_mm:
                SPURIOUS_TO_INTENTIONAL_MOTION_THRESHOLD_MM,
            spurious_to_intentional_motion_threshold_button_change_mm:
                SPURIOUS_TO_INTENTIONAL_MOTION_THRESHOLD_BUTTON_CHANGE_MM,
            button_change_state_timeout: BUTTON_CHANGE_STATE_TIMEOUT,
        });
        let event = TouchpadEvent {
            timestamp: zx::MonotonicInstant::ZERO,
            pressed_buttons: vec![1],
            contacts: vec![
                make_touch_contact(1, Position { x: 1.0, y: 1.0 }),
                make_touch_contact(2, Position { x: 1.0, y: 5.0 }),
            ],
            filtered_palm_contacts: vec![],
        };

        let got = contender.examine_event(&event);
        assert_matches!(got, ExamineEventResult::MatchedContender(_));
    }

    #[test_case(TouchpadEvent{
        timestamp: zx::MonotonicInstant::ZERO,
        pressed_buttons: vec![],
        contacts: vec![],
        filtered_palm_contacts: vec![],
    };"0 fingers")]
    #[test_case(TouchpadEvent{
        timestamp: zx::MonotonicInstant::ZERO,
        pressed_buttons: vec![1],
        contacts: vec![
            make_touch_contact(1, Position{x: 1.0, y: 1.0}),
        ],
        filtered_palm_contacts: vec![],
    };"1 fingers button down")]
    #[test_case(TouchpadEvent{
        timestamp: zx::MonotonicInstant::ZERO,
        pressed_buttons: vec![],
        contacts: vec![
            make_touch_contact(1, Position{x: 1.0, y: 12.0}),
        ],
        filtered_palm_contacts: vec![],
    };"1 fingers move more than threshold")]
    #[test_case(TouchpadEvent{
        timestamp: zx::MonotonicInstant::ZERO,
        pressed_buttons: vec![],
        contacts: vec![
            make_touch_contact(1, Position{x: 1.0, y: 1.0}),
            make_touch_contact(2, Position{x: 5.0, y: 5.0}),
            make_touch_contact(3, Position{x: 10.0, y: 10.0}),
        ],
        filtered_palm_contacts: vec![],
    };"3 fingers")]
    #[fuchsia::test]
    fn one_finger_contact_contender_examine_event_mismatch(event: TouchpadEvent) {
        let contender: Box<dyn gesture_arena::Contender> = Box::new(OneFingerContactContender {
            one_finger_contact_event: TouchpadEvent {
                timestamp: zx::MonotonicInstant::ZERO,
                pressed_buttons: vec![],
                contacts: vec![make_touch_contact(1, Position { x: 1.0, y: 1.0 })],
                filtered_palm_contacts: vec![],
            },
            spurious_to_intentional_motion_threshold_mm:
                SPURIOUS_TO_INTENTIONAL_MOTION_THRESHOLD_MM,
            spurious_to_intentional_motion_threshold_button_change_mm:
                SPURIOUS_TO_INTENTIONAL_MOTION_THRESHOLD_BUTTON_CHANGE_MM,
            button_change_state_timeout: BUTTON_CHANGE_STATE_TIMEOUT,
        });

        let got = contender.examine_event(&event);
        assert_matches!(got, ExamineEventResult::Mismatch(_));
    }

    #[test_case(TouchpadEvent{
        timestamp: zx::MonotonicInstant::ZERO,
        pressed_buttons: vec![],
        contacts: vec![
            make_touch_contact(1, Position{x: 1.0, y: 1.0}),
        ],
        filtered_palm_contacts: vec![],
    };"1 fingers hold")]
    #[test_case(TouchpadEvent{
        timestamp: zx::MonotonicInstant::ZERO,
        pressed_buttons: vec![],
        contacts: vec![
            make_touch_contact(1, Position{x: 1.0, y: 10.0}),
        ],
        filtered_palm_contacts: vec![],
    };"1 fingers move less than threshold")]
    #[fuchsia::test]
    fn one_finger_contact_contender_examine_event_contender(event: TouchpadEvent) {
        let contender: Box<dyn gesture_arena::Contender> = Box::new(OneFingerContactContender {
            one_finger_contact_event: TouchpadEvent {
                timestamp: zx::MonotonicInstant::ZERO,
                pressed_buttons: vec![],
                contacts: vec![make_touch_contact(1, Position { x: 1.0, y: 1.0 })],
                filtered_palm_contacts: vec![],
            },
            spurious_to_intentional_motion_threshold_mm:
                SPURIOUS_TO_INTENTIONAL_MOTION_THRESHOLD_MM,
            spurious_to_intentional_motion_threshold_button_change_mm:
                SPURIOUS_TO_INTENTIONAL_MOTION_THRESHOLD_BUTTON_CHANGE_MM,
            button_change_state_timeout: BUTTON_CHANGE_STATE_TIMEOUT,
        });

        let got = contender.examine_event(&event);
        assert_matches!(got, ExamineEventResult::Contender(c) => {
            pretty_assertions::assert_eq!(c.get_type_name(), "input_pipeline_lib_test::gestures::secondary_button::OneFingerContactContender");
        });
    }

    #[fuchsia::test]
    fn one_finger_contact_contender_examine_event_two_finger_contact_contender() {
        let contender: Box<dyn gesture_arena::Contender> = Box::new(OneFingerContactContender {
            one_finger_contact_event: TouchpadEvent {
                timestamp: zx::MonotonicInstant::ZERO,
                pressed_buttons: vec![],
                contacts: vec![make_touch_contact(1, Position { x: 1.0, y: 1.0 })],
                filtered_palm_contacts: vec![],
            },
            spurious_to_intentional_motion_threshold_mm:
                SPURIOUS_TO_INTENTIONAL_MOTION_THRESHOLD_MM,
            spurious_to_intentional_motion_threshold_button_change_mm:
                SPURIOUS_TO_INTENTIONAL_MOTION_THRESHOLD_BUTTON_CHANGE_MM,
            button_change_state_timeout: BUTTON_CHANGE_STATE_TIMEOUT,
        });

        let event = TouchpadEvent {
            timestamp: zx::MonotonicInstant::ZERO,
            pressed_buttons: vec![],
            contacts: vec![
                make_touch_contact(1, Position { x: 1.0, y: 1.0 }),
                make_touch_contact(2, Position { x: 5.0, y: 5.0 }),
            ],
            filtered_palm_contacts: vec![],
        };
        let got = contender.examine_event(&event);
        assert_matches!(got, ExamineEventResult::Contender(c) => {
            pretty_assertions::assert_eq!(c.get_type_name(), "input_pipeline_lib_test::gestures::secondary_button::TwoFingerContactContender");
        });
    }

    #[test_case(TouchpadEvent{
        timestamp: zx::MonotonicInstant::ZERO,
        pressed_buttons: vec![],
        contacts: vec![],
        filtered_palm_contacts: vec![],
    };"0 fingers")]
    #[test_case(TouchpadEvent{
        timestamp: zx::MonotonicInstant::ZERO,
        pressed_buttons: vec![],
        contacts: vec![
            make_touch_contact(1, Position{x: 1.0, y: 1.0}),
        ],
        filtered_palm_contacts: vec![],
    };"1 fingers")]
    #[test_case(TouchpadEvent{
        timestamp: zx::MonotonicInstant::ZERO,
        pressed_buttons: vec![],
        contacts: vec![
            make_touch_contact(1, Position{x: 1.0, y: 12.0}),
            make_touch_contact(2, Position{x: 5.0, y: 5.0}),
        ],
        filtered_palm_contacts: vec![],
    };"2 fingers move more than threshold")]
    #[test_case(TouchpadEvent{
        timestamp: zx::MonotonicInstant::ZERO,
        pressed_buttons: vec![],
        contacts: vec![
            make_touch_contact(1, Position{x: 1.0, y: 1.0}),
            make_touch_contact(2, Position{x: 5.0, y: 5.0}),
            make_touch_contact(3, Position{x: 10.0, y: 10.0}),
        ],
        filtered_palm_contacts: vec![],
    };"3 fingers")]
    #[fuchsia::test]
    fn two_finger_contact_contender_examine_event_mismatch(event: TouchpadEvent) {
        let contender: Box<dyn gesture_arena::Contender> = Box::new(TwoFingerContactContender {
            two_finger_contact_event: TouchpadEvent {
                timestamp: zx::MonotonicInstant::ZERO,
                pressed_buttons: vec![],
                contacts: vec![
                    make_touch_contact(1, Position { x: 1.0, y: 1.0 }),
                    make_touch_contact(2, Position { x: 5.0, y: 5.0 }),
                ],
                filtered_palm_contacts: vec![],
            },
            spurious_to_intentional_motion_threshold_mm:
                SPURIOUS_TO_INTENTIONAL_MOTION_THRESHOLD_MM,
            spurious_to_intentional_motion_threshold_button_change_mm:
                SPURIOUS_TO_INTENTIONAL_MOTION_THRESHOLD_BUTTON_CHANGE_MM,
            button_change_state_timeout: BUTTON_CHANGE_STATE_TIMEOUT,
        });

        let got = contender.examine_event(&event);
        assert_matches!(got, ExamineEventResult::Mismatch(_));
    }

    #[test_case(TouchpadEvent{
        timestamp: zx::MonotonicInstant::ZERO,
        pressed_buttons: vec![],
        contacts: vec![
            make_touch_contact(1, Position{x: 1.0, y: 9.0}),
            make_touch_contact(2, Position{x: 5.0, y: 5.0}),
        ],
        filtered_palm_contacts: vec![],
    };"2 fingers move less than threshold")]
    #[test_case(TouchpadEvent{
        timestamp: zx::MonotonicInstant::ZERO,
        pressed_buttons: vec![],
        contacts: vec![
            make_touch_contact(1, Position{x: 1.0, y: 1.0}),
            make_touch_contact(2, Position{x: 5.0, y: 5.0}),
        ],
        filtered_palm_contacts: vec![],
    };"2 fingers hold")]
    #[fuchsia::test]
    fn two_finger_contact_contender_examine_event_contender(event: TouchpadEvent) {
        let contender: Box<dyn gesture_arena::Contender> = Box::new(TwoFingerContactContender {
            two_finger_contact_event: TouchpadEvent {
                timestamp: zx::MonotonicInstant::ZERO,
                pressed_buttons: vec![],
                contacts: vec![
                    make_touch_contact(1, Position { x: 1.0, y: 1.0 }),
                    make_touch_contact(2, Position { x: 5.0, y: 5.0 }),
                ],
                filtered_palm_contacts: vec![],
            },
            spurious_to_intentional_motion_threshold_mm:
                SPURIOUS_TO_INTENTIONAL_MOTION_THRESHOLD_MM,
            spurious_to_intentional_motion_threshold_button_change_mm:
                SPURIOUS_TO_INTENTIONAL_MOTION_THRESHOLD_BUTTON_CHANGE_MM,
            button_change_state_timeout: BUTTON_CHANGE_STATE_TIMEOUT,
        });

        let got = contender.examine_event(&event);
        assert_matches!(got, ExamineEventResult::Contender(c) => {
            pretty_assertions::assert_eq!(c.get_type_name(), "input_pipeline_lib_test::gestures::secondary_button::TwoFingerContactContender");
        });
    }

    #[fuchsia::test]
    fn two_finger_contact_contender_examine_event_matched_contender() {
        let contender: Box<dyn gesture_arena::Contender> = Box::new(TwoFingerContactContender {
            two_finger_contact_event: TouchpadEvent {
                timestamp: zx::MonotonicInstant::ZERO,
                pressed_buttons: vec![],
                contacts: vec![
                    make_touch_contact(1, Position { x: 1.0, y: 1.0 }),
                    make_touch_contact(2, Position { x: 5.0, y: 5.0 }),
                ],
                filtered_palm_contacts: vec![],
            },
            spurious_to_intentional_motion_threshold_mm:
                SPURIOUS_TO_INTENTIONAL_MOTION_THRESHOLD_MM,
            spurious_to_intentional_motion_threshold_button_change_mm:
                SPURIOUS_TO_INTENTIONAL_MOTION_THRESHOLD_BUTTON_CHANGE_MM,
            button_change_state_timeout: BUTTON_CHANGE_STATE_TIMEOUT,
        });

        let event = TouchpadEvent {
            timestamp: zx::MonotonicInstant::ZERO,
            pressed_buttons: vec![1],
            contacts: vec![
                make_touch_contact(1, Position { x: 1.0, y: 1.0 }),
                make_touch_contact(2, Position { x: 5.0, y: 5.0 }),
            ],
            filtered_palm_contacts: vec![],
        };
        let got = contender.examine_event(&event);
        assert_matches!(got, ExamineEventResult::MatchedContender(_));
    }

    #[fuchsia::test]
    fn matched_contender_process_buffered_events() {
        let contender: Box<dyn gesture_arena::MatchedContender> = Box::new(MatchedContender {
            spurious_to_intentional_motion_threshold_mm:
                SPURIOUS_TO_INTENTIONAL_MOTION_THRESHOLD_MM,
            spurious_to_intentional_motion_threshold_button_change_mm:
                SPURIOUS_TO_INTENTIONAL_MOTION_THRESHOLD_BUTTON_CHANGE_MM,
            button_change_state_timeout: BUTTON_CHANGE_STATE_TIMEOUT,
            pressed_event: TouchpadEvent {
                timestamp: zx::MonotonicInstant::from_nanos(41),
                pressed_buttons: vec![1],
                contacts: vec![
                    make_touch_contact(1, Position { x: 1.0, y: 1.0 }),
                    make_touch_contact(2, Position { x: 5.0, y: 5.0 }),
                ],
                filtered_palm_contacts: vec![],
            },
        });
        let events = vec![
            TouchpadEvent {
                timestamp: zx::MonotonicInstant::ZERO,
                pressed_buttons: vec![],
                contacts: vec![make_touch_contact(1, Position { x: 1.0, y: 1.0 })],
                filtered_palm_contacts: vec![],
            },
            // Small movement will be ignored.
            TouchpadEvent {
                timestamp: zx::MonotonicInstant::ZERO,
                pressed_buttons: vec![],
                contacts: vec![make_touch_contact(1, Position { x: 1.0, y: 9.0 })],
                filtered_palm_contacts: vec![],
            },
            TouchpadEvent {
                timestamp: zx::MonotonicInstant::from_nanos(21),
                pressed_buttons: vec![],
                contacts: vec![
                    make_touch_contact(1, Position { x: 1.0, y: 9.0 }),
                    make_touch_contact(2, Position { x: 5.0, y: 5.0 }),
                ],
                filtered_palm_contacts: vec![],
            },
            // Small movement will be ignored.
            TouchpadEvent {
                timestamp: zx::MonotonicInstant::from_nanos(21),
                pressed_buttons: vec![],
                contacts: vec![
                    make_touch_contact(1, Position { x: 1.0, y: 9.0 }),
                    make_touch_contact(2, Position { x: 5.0, y: 14.0 }),
                ],
                filtered_palm_contacts: vec![],
            },
            TouchpadEvent {
                timestamp: zx::MonotonicInstant::from_nanos(41),
                pressed_buttons: vec![1],
                contacts: vec![
                    make_touch_contact(1, Position { x: 1.0, y: 9.0 }),
                    make_touch_contact(2, Position { x: 5.0, y: 5.0 }),
                ],
                filtered_palm_contacts: vec![],
            },
        ];

        let got = contender.process_buffered_events(events);

        assert_matches!(got, ProcessBufferedEventsResult{
          generated_events,
          winner: Some(winner),
          recognized_gesture: RecognizedGesture::SecondaryButtonDown,
        } => {
          pretty_assertions::assert_eq!(generated_events, vec![
            gesture_arena::MouseEvent {
              timestamp:zx::MonotonicInstant::from_nanos(41),
              mouse_data: MouseEvent::new(
                  MouseLocation::Relative(RelativeLocation {
                      millimeters: Position { x: 0.0, y: 0.0 },
                  }),
                  /* wheel_delta_v= */ None,
                  /* wheel_delta_h= */ None,
                  MousePhase::Down,
                  /* affected_buttons= */ hashset!{gesture_arena::SECONDARY_BUTTON},
                  /* pressed_buttons= */ hashset!{gesture_arena::SECONDARY_BUTTON},
                  /* is_precision_scroll= */ None,
              ),
            }
          ]);
          pretty_assertions::assert_eq!(winner.get_type_name(), "input_pipeline_lib_test::gestures::secondary_button::ButtonDownWinner");
        });
    }

    #[test_case(TouchpadEvent{
        timestamp: zx::MonotonicInstant::from_nanos(41),
        pressed_buttons: vec![],
        contacts: vec![
            make_touch_contact(1, Position{x: 1.0, y: 1.0}),
        ],
        filtered_palm_contacts: vec![],
    };"1 finger button release")]
    #[test_case(TouchpadEvent{
        timestamp: zx::MonotonicInstant::from_nanos(41),
        pressed_buttons: vec![],
        contacts: vec![
            make_touch_contact(1, Position{x: 1.0, y: 1.0}),
            make_touch_contact(2, Position{x: 10.0, y: 10.0}),
        ],
        filtered_palm_contacts: vec![],
    };"2 finger button release")]
    #[test_case(TouchpadEvent{
        timestamp: zx::MonotonicInstant::from_nanos(41),
        pressed_buttons: vec![],
        contacts: vec![
            make_touch_contact(1, Position{x: 1.0, y: 1.0}),
            make_touch_contact(2, Position{x: 10.0, y: 10.0}),
            make_touch_contact(2, Position{x: 15.0, y: 15.0}),
        ],
        filtered_palm_contacts: vec![],
    };"3 finger button release")]
    #[test_case(TouchpadEvent{
        timestamp: zx::MonotonicInstant::from_nanos(41),
        pressed_buttons: vec![],
        contacts: vec![
            make_touch_contact(1, Position{x: 19.0, y: 1.0}),
            make_touch_contact(2, Position{x: 10.0, y: 10.0}),
        ],
        filtered_palm_contacts: vec![],
    };"move less than threshold in button change state")]
    #[test_case(TouchpadEvent{
        timestamp: zx::MonotonicInstant::ZERO + zx::MonotonicDuration::from_millis(1500),
        pressed_buttons: vec![],
        contacts: vec![
            make_touch_contact(1, Position{x: 9.0, y: 1.0}),
            make_touch_contact(2, Position{x: 10.0, y: 10.0}),
        ],
        filtered_palm_contacts: vec![],
    };"move less than threshold out of button change state")]
    #[test_case(TouchpadEvent{
        timestamp: zx::MonotonicInstant::from_nanos(41),
        pressed_buttons: vec![],
        contacts: vec![
            make_touch_contact(1, Position{x: 21.0, y: 1.0}),
            make_touch_contact(2, Position{x: 10.0, y: 10.0}),
        ],
        filtered_palm_contacts: vec![],
    };"move more than threshold in button change state")]
    #[test_case(TouchpadEvent{
        timestamp: zx::MonotonicInstant::ZERO + zx::MonotonicDuration::from_millis(1500),
        pressed_buttons: vec![],
        contacts: vec![
            make_touch_contact(1, Position{x: 11.0, y: 1.0}),
            make_touch_contact(2, Position{x: 10.0, y: 10.0}),
        ],
        filtered_palm_contacts: vec![],
    };"move more than threshold out of button change state")]
    #[fuchsia::test]
    fn button_down_winner_button_up(event: TouchpadEvent) {
        let winner: Box<dyn gesture_arena::Winner> = Box::new(ButtonDownWinner {
            spurious_to_intentional_motion_threshold_mm:
                SPURIOUS_TO_INTENTIONAL_MOTION_THRESHOLD_MM,
            spurious_to_intentional_motion_threshold_button_change_mm:
                SPURIOUS_TO_INTENTIONAL_MOTION_THRESHOLD_BUTTON_CHANGE_MM,
            button_change_state_timeout: BUTTON_CHANGE_STATE_TIMEOUT,
            pressed_event: TouchpadEvent {
                timestamp: zx::MonotonicInstant::ZERO,
                pressed_buttons: vec![1],
                contacts: vec![
                    make_touch_contact(1, Position { x: 1.0, y: 1.0 }),
                    make_touch_contact(2, Position { x: 10.0, y: 10.0 }),
                ],
                filtered_palm_contacts: vec![],
            },
        });

        let got = winner.process_new_event(event);
        assert_matches!(got, ProcessNewEventResult::ContinueGesture(Some(gesture_arena::MouseEvent {mouse_data, ..}), got_winner) => {
            pretty_assertions::assert_eq!(mouse_data, MouseEvent::new(
                MouseLocation::Relative(RelativeLocation {
                    millimeters: Position { x: 0.0, y: 0.0 },
                }),
                /* wheel_delta_v= */ None,
                /* wheel_delta_h= */ None,
                MousePhase::Up,
                /* affected_buttons= */ hashset!{gesture_arena::SECONDARY_BUTTON},
                /* pressed_buttons= */ hashset!{},
                /* is_precision_scroll= */ None,
            ));
            pretty_assertions::assert_eq!(got_winner.get_type_name(), "input_pipeline_lib_test::gestures::secondary_button::ButtonUpWinner");
        });
    }

    #[test_case(TouchpadEvent{
        timestamp: zx::MonotonicInstant::from_nanos(41),
        pressed_buttons: vec![1],
        contacts: vec![
            make_touch_contact(1, Position{x: 20.0, y: 1.0}),
            make_touch_contact(2, Position { x: 10.0, y: 10.0 }),
        ],
        filtered_palm_contacts: vec![],
    };"move less than threshold in button change state")]
    #[test_case(TouchpadEvent{
        timestamp: zx::MonotonicInstant::ZERO + zx::MonotonicDuration::from_millis(1500),
        pressed_buttons: vec![1],
        contacts: vec![
            make_touch_contact(1, Position{x: 10.0, y: 1.0}),
            make_touch_contact(2, Position { x: 10.0, y: 10.0 }),
        ],
        filtered_palm_contacts: vec![],
    };"move less than threshold out of button change state")]
    #[fuchsia::test]
    fn button_down_winner_continue(event: TouchpadEvent) {
        let winner: Box<dyn gesture_arena::Winner> = Box::new(ButtonDownWinner {
            spurious_to_intentional_motion_threshold_mm:
                SPURIOUS_TO_INTENTIONAL_MOTION_THRESHOLD_MM,
            spurious_to_intentional_motion_threshold_button_change_mm:
                SPURIOUS_TO_INTENTIONAL_MOTION_THRESHOLD_BUTTON_CHANGE_MM,
            button_change_state_timeout: BUTTON_CHANGE_STATE_TIMEOUT,
            pressed_event: TouchpadEvent {
                timestamp: zx::MonotonicInstant::ZERO,
                pressed_buttons: vec![1],
                contacts: vec![
                    make_touch_contact(1, Position { x: 1.0, y: 1.0 }),
                    make_touch_contact(2, Position { x: 10.0, y: 10.0 }),
                ],
                filtered_palm_contacts: vec![],
            },
        });

        let got = winner.process_new_event(event);
        assert_matches!(got, ProcessNewEventResult::ContinueGesture(None, got_winner)=>{
            pretty_assertions::assert_eq!(got_winner.get_type_name(), "input_pipeline_lib_test::gestures::secondary_button::ButtonDownWinner");
        });
    }

    #[test_case(TouchpadEvent{
        timestamp: zx::MonotonicInstant::from_nanos(41),
        pressed_buttons: vec![1],
        contacts: vec![
            make_touch_contact(1, Position{x: 21.0, y: 1.0}),
            make_touch_contact(2, Position { x: 10.0, y: 10.0 }),
        ],
        filtered_palm_contacts: vec![],
    };"move more than threshold in button change state")]
    #[test_case(TouchpadEvent{
        timestamp: zx::MonotonicInstant::ZERO + zx::MonotonicDuration::from_millis(1500),
        pressed_buttons: vec![1],
        contacts: vec![
            make_touch_contact(1, Position{x: 11.0, y: 1.0}),
            make_touch_contact(2, Position { x: 10.0, y: 10.0 }),
        ],
        filtered_palm_contacts: vec![],
    };"move more than threshold out of button change state")]
    #[fuchsia::test]
    fn button_down_winner_drag_winner_continue(event: TouchpadEvent) {
        let winner: Box<dyn gesture_arena::Winner> = Box::new(ButtonDownWinner {
            spurious_to_intentional_motion_threshold_mm:
                SPURIOUS_TO_INTENTIONAL_MOTION_THRESHOLD_MM,
            spurious_to_intentional_motion_threshold_button_change_mm:
                SPURIOUS_TO_INTENTIONAL_MOTION_THRESHOLD_BUTTON_CHANGE_MM,
            button_change_state_timeout: BUTTON_CHANGE_STATE_TIMEOUT,
            pressed_event: TouchpadEvent {
                timestamp: zx::MonotonicInstant::ZERO,
                pressed_buttons: vec![1],
                contacts: vec![
                    make_touch_contact(1, Position { x: 1.0, y: 1.0 }),
                    make_touch_contact(2, Position { x: 10.0, y: 10.0 }),
                ],
                filtered_palm_contacts: vec![],
            },
        });

        let got = winner.process_new_event(event);
        assert_matches!(got, ProcessNewEventResult::ContinueGesture(Some(gesture_arena::MouseEvent {mouse_data, ..}), got_winner)=>{
            pretty_assertions::assert_eq!(mouse_data.phase, MousePhase::Move);
            pretty_assertions::assert_eq!(got_winner.get_type_name(), "input_pipeline_lib_test::gestures::secondary_button::DragWinner");
        });
    }

    #[test_case(TouchpadEvent{
      timestamp: zx::MonotonicInstant::from_nanos(41),
      pressed_buttons: vec![],
      contacts: vec![
          make_touch_contact(1, Position{x: 1.0, y: 1.0}),
          make_touch_contact(2, Position { x: 10.0, y: 10.0 }),
      ],
      filtered_palm_contacts: vec![],
    };"button release")]
    #[test_case(TouchpadEvent{
        timestamp: zx::MonotonicInstant::from_nanos(41),
        pressed_buttons: vec![],
        contacts: vec![
            make_touch_contact(1, Position{x: 1.0, y: 1.0}),
        ],
        filtered_palm_contacts: vec![],
      };"1 finger button release")]
    #[test_case(TouchpadEvent{
      timestamp: zx::MonotonicInstant::from_nanos(41),
      pressed_buttons: vec![],
      contacts: vec![
          make_touch_contact(1, Position{x: 21.0, y: 1.0}),
          make_touch_contact(2, Position { x: 10.0, y: 10.0 }),
      ],
      filtered_palm_contacts: vec![],
    };"large move and button release")]
    #[fuchsia::test]
    fn drag_winner_button_up(event: TouchpadEvent) {
        let winner: Box<dyn gesture_arena::Winner> = Box::new(DragWinner {
            spurious_to_intentional_motion_threshold_button_change_mm:
                SPURIOUS_TO_INTENTIONAL_MOTION_THRESHOLD_BUTTON_CHANGE_MM,
            button_change_state_timeout: BUTTON_CHANGE_STATE_TIMEOUT,
            last_event: TouchpadEvent {
                timestamp: zx::MonotonicInstant::ZERO,
                pressed_buttons: vec![1],
                contacts: vec![
                    make_touch_contact(1, Position { x: 1.0, y: 1.0 }),
                    make_touch_contact(2, Position { x: 10.0, y: 10.0 }),
                ],
                filtered_palm_contacts: vec![],
            },
        });

        let got = winner.process_new_event(event);
        assert_matches!(got, ProcessNewEventResult::ContinueGesture(Some(gesture_arena::MouseEvent {mouse_data, ..}), got_winner) => {
            pretty_assertions::assert_eq!(mouse_data, MouseEvent::new(
                MouseLocation::Relative(RelativeLocation {
                    millimeters: Position { x: 0.0, y: 0.0 },
                }),
                /* wheel_delta_v= */ None,
                /* wheel_delta_h= */ None,
                MousePhase::Up,
                /* affected_buttons= */ hashset!{gesture_arena::SECONDARY_BUTTON},
                /* pressed_buttons= */ hashset!{},
                /* is_precision_scroll= */ None,
            ));
            pretty_assertions::assert_eq!(got_winner.get_type_name(), "input_pipeline_lib_test::gestures::secondary_button::ButtonUpWinner");
        });
    }

    #[test_case(TouchpadEvent{
        timestamp: zx::MonotonicInstant::from_nanos(41),
        pressed_buttons: vec![1],
        contacts: vec![
            make_touch_contact(1, Position{x: 19.0, y: 1.0}),
            make_touch_contact(2, Position { x: 10.0, y: 10.0 }),
        ],
        filtered_palm_contacts: vec![],
    };"2 finger")]
    #[test_case(TouchpadEvent{
        timestamp: zx::MonotonicInstant::from_nanos(41),
        pressed_buttons: vec![1],
        contacts: vec![
            make_touch_contact(1, Position{x: 19.0, y: 1.0}),
        ],
        filtered_palm_contacts: vec![],
    };"1 finger")]
    #[fuchsia::test]
    fn drag_winner_continue(event: TouchpadEvent) {
        let winner: Box<dyn gesture_arena::Winner> = Box::new(DragWinner {
            spurious_to_intentional_motion_threshold_button_change_mm:
                SPURIOUS_TO_INTENTIONAL_MOTION_THRESHOLD_BUTTON_CHANGE_MM,
            button_change_state_timeout: BUTTON_CHANGE_STATE_TIMEOUT,
            last_event: TouchpadEvent {
                timestamp: zx::MonotonicInstant::ZERO,
                pressed_buttons: vec![1],
                contacts: vec![
                    make_touch_contact(1, Position { x: 0.0, y: 0.0 }),
                    make_touch_contact(2, Position { x: 10.0, y: 10.0 }),
                ],
                filtered_palm_contacts: vec![],
            },
        });

        let got = winner.process_new_event(event);
        assert_matches!(got, ProcessNewEventResult::ContinueGesture(Some(gesture_arena::MouseEvent {mouse_data, ..}), got_winner)=>{
            pretty_assertions::assert_eq!(mouse_data, MouseEvent::new(
                MouseLocation::Relative(RelativeLocation {
                    millimeters: Position { x: 19.0, y: 1.0 },
                }),
                /* wheel_delta_v= */ None,
                /* wheel_delta_h= */ None,
                MousePhase::Move,
                /* affected_buttons= */ hashset!{},
                /* pressed_buttons= */ hashset!{gesture_arena::SECONDARY_BUTTON},
                /* is_precision_scroll= */ None,
            ));
            pretty_assertions::assert_eq!(got_winner.get_type_name(), "input_pipeline_lib_test::gestures::secondary_button::DragWinner");
        });
    }

    #[fuchsia::test]
    fn button_up_winner_continue() {
        let winner: Box<dyn gesture_arena::Winner> = Box::new(ButtonUpWinner {
            spurious_to_intentional_motion_threshold_button_change_mm:
                SPURIOUS_TO_INTENTIONAL_MOTION_THRESHOLD_BUTTON_CHANGE_MM,
            button_change_state_timeout: BUTTON_CHANGE_STATE_TIMEOUT,
            button_up_event: TouchpadEvent {
                timestamp: zx::MonotonicInstant::ZERO,
                pressed_buttons: vec![],
                contacts: vec![
                    make_touch_contact(1, Position { x: 0.0, y: 0.0 }),
                    make_touch_contact(2, Position { x: 10.0, y: 10.0 }),
                ],
                filtered_palm_contacts: vec![],
            },
        });

        let event = TouchpadEvent {
            timestamp: zx::MonotonicInstant::from_nanos(41),
            pressed_buttons: vec![],
            contacts: vec![
                make_touch_contact(1, Position { x: 10.0, y: 1.0 }),
                make_touch_contact(2, Position { x: 10.0, y: 10.0 }),
            ],
            filtered_palm_contacts: vec![],
        };

        let got = winner.process_new_event(event);
        assert_matches!(got, ProcessNewEventResult::ContinueGesture(None, got_winner)=>{
            pretty_assertions::assert_eq!(got_winner.get_type_name(), "input_pipeline_lib_test::gestures::secondary_button::ButtonUpWinner");
        });
    }

    #[test_case(TouchpadEvent{
        timestamp: zx::MonotonicInstant::ZERO + zx::MonotonicDuration::from_millis(1_001),
        pressed_buttons: vec![],
        contacts: vec![
            make_touch_contact(1, Position{x: 0.0, y: 0.0}),
        ],
        filtered_palm_contacts: vec![],
    };"timeout")]
    #[test_case(TouchpadEvent{
        timestamp: zx::MonotonicInstant::from_nanos(41),
        pressed_buttons: vec![1],
        contacts: vec![
            make_touch_contact(1, Position{x: 0.0, y: 0.0}),
        ],
        filtered_palm_contacts: vec![],
    };"button down")]
    #[test_case(TouchpadEvent{
        timestamp: zx::MonotonicInstant::from_nanos(41),
        pressed_buttons: vec![],
        contacts: vec![
            make_touch_contact(1, Position{x: 21.0, y: 0.0}),
        ],
        filtered_palm_contacts: vec![],
    };"move more than threshold")]
    #[test_case(TouchpadEvent{
        timestamp: zx::MonotonicInstant::from_nanos(41),
        pressed_buttons: vec![],
        contacts: vec![
            make_touch_contact(1, Position{x: 0.0, y: 0.0}),
            make_touch_contact(2, Position{x: 10.0, y: 10.0}),
            make_touch_contact(3, Position{x: 15.0, y: 15.0}),
        ],
        filtered_palm_contacts: vec![],
    };"more contacts")]
    #[test_case(TouchpadEvent{
      timestamp: zx::MonotonicInstant::from_nanos(41),
      pressed_buttons: vec![],
      contacts: vec![
          make_touch_contact(1, Position{x: 0.0, y: 0.0}),
      ],
      filtered_palm_contacts: vec![],
    };"less contacts")]
    #[test_case(TouchpadEvent{
        timestamp: zx::MonotonicInstant::from_nanos(41),
        pressed_buttons: vec![],
        contacts: vec![],
        filtered_palm_contacts: vec![],
    };"no contact")]
    #[fuchsia::test]
    fn button_up_winner_end(event: TouchpadEvent) {
        let winner: Box<dyn gesture_arena::Winner> = Box::new(ButtonUpWinner {
            spurious_to_intentional_motion_threshold_button_change_mm:
                SPURIOUS_TO_INTENTIONAL_MOTION_THRESHOLD_BUTTON_CHANGE_MM,
            button_change_state_timeout: BUTTON_CHANGE_STATE_TIMEOUT,
            button_up_event: TouchpadEvent {
                timestamp: zx::MonotonicInstant::ZERO,
                pressed_buttons: vec![],
                contacts: vec![
                    make_touch_contact(1, Position { x: 0.0, y: 0.0 }),
                    make_touch_contact(2, Position { x: 10.0, y: 10.0 }),
                ],
                filtered_palm_contacts: vec![],
            },
        });

        let got = winner.process_new_event(event);
        assert_matches!(
            got,
            ProcessNewEventResult::EndGesture(EndGestureEvent::UnconsumedEvent(_), _)
        );
    }
}
