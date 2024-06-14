// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use super::gesture_arena::TouchpadEvent;
use crate::utils::{euclidean_distance, Position};
use std::collections::HashMap;

/// Result of movement_from_events.
#[derive(Debug, PartialEq)]
pub(super) struct MovementDetail {
    pub(super) euclidean_distance: f32,
    pub(super) movement: Position,
}

/// `movement_from_events` is used to compute the movement in click-drag events.
/// It take the finger that moves the furthest as events' movement.
pub(super) fn movement_from_events(
    previous_event: &TouchpadEvent,
    new_event: &TouchpadEvent,
) -> MovementDetail {
    let mut previous_contacts: HashMap<u32, Position> = HashMap::new();
    for c in &previous_event.contacts {
        previous_contacts.insert(c.id, c.position.clone());
    }

    let mut movement = Position { x: 0.0, y: 0.0 };
    let mut max_distance = 0.0;
    for new_contact in &new_event.contacts {
        let previous = previous_contacts.get(&new_contact.id);
        match previous {
            None => {}
            Some(&previous) => {
                let dis = euclidean_distance(new_contact.position, previous.clone());
                if dis > max_distance {
                    max_distance = dis;
                    movement = new_contact.position - previous;
                }
            }
        }
    }

    MovementDetail { euclidean_distance: max_distance, movement }
}

#[cfg(test)]
mod tests {

    use super::*;
    use crate::gestures::gesture_arena::TouchpadEvent;
    use crate::touch_binding;
    use crate::utils::Position;
    use fuchsia_zircon as fx;
    use test_case::test_case;

    fn make_touch_contact(id: u32, position: Position) -> touch_binding::TouchContact {
        touch_binding::TouchContact { id, position, pressure: None, contact_size: None }
    }

    #[test_case(
        TouchpadEvent {
            timestamp: fx::Time::ZERO,
            pressed_buttons: vec![],
            contacts: vec![
                make_touch_contact(1, Position{x: 1.0, y: 1.0}),
                make_touch_contact(2, Position{x: 5.0, y: 5.0}),
            ],
            filtered_palm_contacts: vec![],
        }; "previous contact stay, place new finger")]
    #[test_case(
        TouchpadEvent {
            timestamp: fx::Time::ZERO,
            pressed_buttons: vec![],
            contacts: vec![
                make_touch_contact(2, Position{x: 5.0, y: 5.0}),
            ],
            filtered_palm_contacts: vec![],
        }; "previous contact lift, place new finger")]
    #[test_case(
        TouchpadEvent {
            timestamp: fx::Time::ZERO,
            pressed_buttons: vec![],
            contacts: vec![],
            filtered_palm_contacts: vec![],
        }; "previous contact lift")]
    #[fuchsia::test]
    fn movement_from_events_no_movement(new_event: TouchpadEvent) {
        let previous_event = TouchpadEvent {
            timestamp: fx::Time::ZERO,
            pressed_buttons: vec![],
            contacts: vec![make_touch_contact(1, Position { x: 1.0, y: 1.0 })],
            filtered_palm_contacts: vec![],
        };

        let got = movement_from_events(&previous_event, &new_event);
        let want = MovementDetail { euclidean_distance: 0.0, movement: Position::zero() };
        pretty_assertions::assert_eq!(got, want);
    }

    #[test_case(
        TouchpadEvent {
            timestamp: fx::Time::ZERO,
            pressed_buttons: vec![],
            contacts: vec![
                make_touch_contact(1, Position{x: 1.0, y: 6.0}),
                make_touch_contact(2, Position{x: 5.0, y: 5.0}),
            ],
            filtered_palm_contacts: vec![],
        }; "contact 1 move, contact 2 stay")]
    #[test_case(
        TouchpadEvent {
            timestamp: fx::Time::ZERO,
            pressed_buttons: vec![],
            contacts: vec![
                make_touch_contact(1, Position{x: 1.0, y: 1.0}),
                make_touch_contact(2, Position{x: 5.0, y: 10.0}),
            ],
            filtered_palm_contacts: vec![],
        }; "contact 2 move, contact 1 stay")]
    #[test_case(
        TouchpadEvent {
            timestamp: fx::Time::ZERO,
            pressed_buttons: vec![],
            contacts: vec![
                make_touch_contact(1, Position{x: 1.0, y: 6.0}),
                make_touch_contact(2, Position{x: 3.0, y: 5.0}),
            ],
            filtered_palm_contacts: vec![],
        }; "contact 1 movement larger")]
    #[test_case(
        TouchpadEvent {
            timestamp: fx::Time::ZERO,
            pressed_buttons: vec![],
            contacts: vec![
                make_touch_contact(1, Position{x: 1.0, y: 6.0}),
            ],
            filtered_palm_contacts: vec![],
        }; "1 contact lift")]
    #[fuchsia::test]
    fn movement_from_events_furthest_movement(new_event: TouchpadEvent) {
        let previous_event = TouchpadEvent {
            timestamp: fx::Time::ZERO,
            pressed_buttons: vec![],
            contacts: vec![
                make_touch_contact(1, Position { x: 1.0, y: 1.0 }),
                make_touch_contact(2, Position { x: 5.0, y: 5.0 }),
            ],
            filtered_palm_contacts: vec![],
        };

        let got = movement_from_events(&previous_event, &new_event);
        let want =
            MovementDetail { euclidean_distance: 5.0, movement: Position { x: 0.0, y: 5.0 } };
        pretty_assertions::assert_eq!(got, want);
    }
}
