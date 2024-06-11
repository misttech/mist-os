// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

mod tests {
    use super::super::utils;
    use crate::{input_device, mouse_binding, touch_binding, Position};
    use std::collections::HashSet;

    fn touchpad_event(
        positions: Vec<Position>,
        pressed_buttons: HashSet<mouse_binding::MouseButton>,
    ) -> input_device::InputEvent {
        let injector_contacts: Vec<touch_binding::TouchContact> = positions
            .iter()
            .enumerate()
            .map(|(i, p)| touch_binding::TouchContact {
                id: i as u32,
                position: *p,
                contact_size: None,
                pressure: None,
            })
            .collect();

        utils::make_touchpad_event(touch_binding::TouchpadEvent {
            injector_contacts,
            pressed_buttons,
        })
    }

    mod click {
        use super::super::super::utils;
        use super::touchpad_event;
        use crate::{input_device, mouse_binding, Position};
        use assert_matches::assert_matches;
        use maplit::hashset;
        use pretty_assertions::assert_eq;

        #[fuchsia::test(allow_stalls = false)]
        async fn secondary_click_keep_contact() {
            let finger1_pos0_um = Position { x: 2_000.0, y: 3_000.0 };
            let finger1_pos1_um = finger1_pos0_um.clone();
            let finger2_pos1_um = Position { x: 2_000.0, y: 5_000.0 };
            let finger1_pos2_um = finger1_pos1_um.clone();
            let finger2_pos2_um = finger2_pos1_um.clone();
            let finger1_pos3_um = finger1_pos2_um.clone();
            let finger2_pos3_um = finger2_pos2_um.clone();
            let finger1_pos4_um = finger1_pos3_um.clone();
            let finger2_pos4_um = finger2_pos3_um.clone();

            let inputs = vec![
                touchpad_event(vec![finger1_pos0_um], hashset! {}),
                touchpad_event(vec![finger1_pos1_um, finger2_pos1_um], hashset! {}),
                touchpad_event(vec![finger1_pos2_um, finger2_pos2_um], hashset! {1}),
                touchpad_event(vec![finger1_pos3_um, finger2_pos3_um], hashset! {}),
                // ensure no unexpected tap event.
                touchpad_event(vec![finger1_pos4_um, finger2_pos4_um], hashset! {}),
                touchpad_event(vec![], hashset! {}),
            ];

            let got = utils::run_gesture_arena_test(inputs).await;

            assert_eq!(got.len(), 6);
            assert_eq!(got[0].as_slice(), []);
            assert_eq!(got[1].as_slice(), []);
            assert_matches!(got[2].as_slice(), [
              utils::expect_mouse_event!(phase: phase_a, pressed_buttons: pressed_button_a, affected_buttons: affected_button_a, location: location_a),
            ] => {
              assert_eq!(phase_a, &mouse_binding::MousePhase::Down);
              assert_eq!(pressed_button_a, &hashset! {2});
              assert_eq!(affected_button_a, &hashset! {2});
              assert_eq!(location_a, &utils::NO_MOVEMENT_LOCATION);
            });
            assert_matches!(got[3].as_slice(), [
              utils::expect_mouse_event!(phase: phase_a, pressed_buttons: pressed_button_a, affected_buttons: affected_button_a, location: location_a),
            ] => {
              assert_eq!(phase_a, &mouse_binding::MousePhase::Up);
              assert_eq!(pressed_button_a, &hashset! {});
              assert_eq!(affected_button_a, &hashset! {2});
              assert_eq!(location_a, &utils::NO_MOVEMENT_LOCATION);
            });
            assert_eq!(got[4].as_slice(), []);
            assert_eq!(got[5].as_slice(), []);
        }

        #[fuchsia::test(allow_stalls = false)]
        async fn secondary_click_lift_one_then_other() {
            let finger1_pos0_um = Position { x: 2_000.0, y: 3_000.0 };
            let finger1_pos1_um = finger1_pos0_um.clone();
            let finger2_pos1_um = Position { x: 2_000.0, y: 5_000.0 };
            let finger1_pos2_um = finger1_pos1_um.clone();
            let finger2_pos2_um = finger2_pos1_um.clone();
            let finger1_pos3_um = finger1_pos2_um.clone();
            let finger2_pos3_um = finger2_pos2_um.clone();
            let finger1_pos4_um = finger1_pos3_um.clone();

            let inputs = vec![
                touchpad_event(vec![finger1_pos0_um], hashset! {}),
                touchpad_event(vec![finger1_pos1_um, finger2_pos1_um], hashset! {}),
                touchpad_event(vec![finger1_pos2_um, finger2_pos2_um], hashset! {1}),
                touchpad_event(vec![finger1_pos3_um, finger2_pos3_um], hashset! {}),
                // ensure no unexpected tap event.
                touchpad_event(vec![finger1_pos4_um], hashset! {}),
                touchpad_event(vec![], hashset! {}),
            ];

            let got = utils::run_gesture_arena_test(inputs).await;

            assert_eq!(got.len(), 6);
            assert_eq!(got[0].as_slice(), []);
            assert_eq!(got[1].as_slice(), []);
            assert_matches!(got[2].as_slice(), [
              utils::expect_mouse_event!(phase: phase_a, pressed_buttons: pressed_button_a, affected_buttons: affected_button_a, location: location_a),
            ] => {
              assert_eq!(phase_a, &mouse_binding::MousePhase::Down);
              assert_eq!(pressed_button_a, &hashset! {2});
              assert_eq!(affected_button_a, &hashset! {2});
              assert_eq!(location_a, &utils::NO_MOVEMENT_LOCATION);
            });
            assert_matches!(got[3].as_slice(), [
              utils::expect_mouse_event!(phase: phase_a, pressed_buttons: pressed_button_a, affected_buttons: affected_button_a, location: location_a),
            ] => {
              assert_eq!(phase_a, &mouse_binding::MousePhase::Up);
              assert_eq!(pressed_button_a, &hashset! {});
              assert_eq!(affected_button_a, &hashset! {2});
              assert_eq!(location_a, &utils::NO_MOVEMENT_LOCATION);
            });
            assert_eq!(got[4].as_slice(), []);
            assert_eq!(got[5].as_slice(), []);
        }

        #[fuchsia::test(allow_stalls = false)]
        async fn secondary_click_lift_one_while_button_down() {
            let finger1_pos0_um = Position { x: 2_000.0, y: 3_000.0 };
            let finger1_pos1_um = finger1_pos0_um.clone();
            let finger2_pos1_um = Position { x: 2_000.0, y: 5_000.0 };
            let finger1_pos2_um = finger1_pos1_um.clone();
            let finger2_pos2_um = finger2_pos1_um.clone();
            let finger1_pos3_um = finger1_pos2_um.clone();
            let finger1_pos4_um = finger1_pos3_um.clone();

            let inputs = vec![
                touchpad_event(vec![finger1_pos0_um], hashset! {}),
                touchpad_event(vec![finger1_pos1_um, finger2_pos1_um], hashset! {}),
                touchpad_event(vec![finger1_pos2_um, finger2_pos2_um], hashset! {1}),
                touchpad_event(vec![finger1_pos3_um], hashset! {1}),
                touchpad_event(vec![finger1_pos4_um], hashset! {}),
            ];

            let got = utils::run_gesture_arena_test(inputs).await;

            assert_eq!(got.len(), 5);
            assert_eq!(got[0].as_slice(), []);
            assert_eq!(got[1].as_slice(), []);
            assert_matches!(got[2].as_slice(), [
              utils::expect_mouse_event!(phase: phase_a, pressed_buttons: pressed_button_a, affected_buttons: affected_button_a, location: location_a),
            ] => {
              assert_eq!(phase_a, &mouse_binding::MousePhase::Down);
              assert_eq!(pressed_button_a, &hashset! {2});
              assert_eq!(affected_button_a, &hashset! {2});
              assert_eq!(location_a, &utils::NO_MOVEMENT_LOCATION);
            });
            assert_eq!(got[3].as_slice(), []);
            assert_matches!(got[4].as_slice(), [
              utils::expect_mouse_event!(phase: phase_a, pressed_buttons: pressed_button_a, affected_buttons: affected_button_a, location: location_a),
            ] => {
              assert_eq!(phase_a, &mouse_binding::MousePhase::Up);
              assert_eq!(pressed_button_a, &hashset! {});
              assert_eq!(affected_button_a, &hashset! {2});
              assert_eq!(location_a, &utils::NO_MOVEMENT_LOCATION);
            });
        }
    }

    mod click_chain {
        use super::super::super::utils;
        use super::touchpad_event;
        use crate::gestures::args;
        use crate::{input_device, mouse_binding, Position};
        use assert_matches::assert_matches;
        use maplit::hashset;
        use pretty_assertions::assert_eq;
        use test_util::{assert_gt, assert_near};

        #[fuchsia::test(allow_stalls = false)]
        async fn secondary_click_then_one_finger_button_down() {
            let finger1_pos0_um = Position { x: 2_000.0, y: 3_000.0 };
            let finger1_pos1_um = finger1_pos0_um.clone();
            let finger2_pos1_um = Position { x: 2_000.0, y: 5_000.0 };
            let finger1_pos2_um = finger1_pos1_um.clone();
            let finger2_pos2_um = finger2_pos1_um.clone();
            let finger1_pos3_um = finger1_pos2_um.clone();
            let finger2_pos3_um = finger2_pos2_um.clone();
            let finger1_pos4_um = finger1_pos3_um.clone();
            let finger1_pos5_um = finger1_pos4_um.clone();

            let inputs = vec![
                touchpad_event(vec![finger1_pos0_um], hashset! {}),
                touchpad_event(vec![finger1_pos1_um, finger2_pos1_um], hashset! {}),
                touchpad_event(vec![finger1_pos2_um, finger2_pos2_um], hashset! {1}),
                touchpad_event(vec![finger1_pos3_um, finger2_pos3_um], hashset! {}),
                touchpad_event(vec![finger1_pos4_um], hashset! {}),
                touchpad_event(vec![finger1_pos5_um], hashset! {1}),
            ];

            let got = utils::run_gesture_arena_test(inputs).await;

            assert_eq!(got.len(), 6);
            assert_eq!(got[0].as_slice(), []);
            assert_eq!(got[1].as_slice(), []);
            assert_matches!(got[2].as_slice(), [
              utils::expect_mouse_event!(phase: phase_a, pressed_buttons: pressed_button_a, affected_buttons: affected_button_a, location: location_a),
            ] => {
              assert_eq!(phase_a, &mouse_binding::MousePhase::Down);
              assert_eq!(pressed_button_a, &hashset! {2});
              assert_eq!(affected_button_a, &hashset! {2});
              assert_eq!(location_a, &utils::NO_MOVEMENT_LOCATION);
            });
            assert_matches!(got[3].as_slice(), [
              utils::expect_mouse_event!(phase: phase_a, pressed_buttons: pressed_button_a, affected_buttons: affected_button_a, location: location_a),
            ] => {
              assert_eq!(phase_a, &mouse_binding::MousePhase::Up);
              assert_eq!(pressed_button_a, &hashset! {});
              assert_eq!(affected_button_a, &hashset! {2});
              assert_eq!(location_a, &utils::NO_MOVEMENT_LOCATION);
            });
            assert_eq!(got[4].as_slice(), []);
            assert_matches!(got[5].as_slice(), [
              utils::expect_mouse_event!(phase: phase_a, pressed_buttons: pressed_button_a, affected_buttons: affected_button_a, location: location_a),
            ] => {
              assert_eq!(phase_a, &mouse_binding::MousePhase::Down);
              assert_eq!(pressed_button_a, &hashset! {1});
              assert_eq!(affected_button_a, &hashset! {1});
              assert_eq!(location_a, &utils::NO_MOVEMENT_LOCATION);
            });
        }

        #[fuchsia::test(allow_stalls = false)]
        async fn secondary_click_then_secondary_button_down() {
            let finger1_pos0_um = Position { x: 2_000.0, y: 3_000.0 };
            let finger1_pos1_um = finger1_pos0_um.clone();
            let finger2_pos1_um = Position { x: 2_000.0, y: 5_000.0 };
            let finger1_pos2_um = finger1_pos1_um.clone();
            let finger2_pos2_um = finger2_pos1_um.clone();
            let finger1_pos3_um = finger1_pos2_um.clone();
            let finger2_pos3_um = finger2_pos2_um.clone();
            let finger1_pos4_um = finger1_pos3_um.clone();
            let finger2_pos4_um = finger2_pos3_um.clone();
            let finger1_pos5_um = finger1_pos4_um.clone();
            let finger2_pos5_um = finger2_pos4_um.clone();

            let inputs = vec![
                touchpad_event(vec![finger1_pos0_um], hashset! {}),
                touchpad_event(vec![finger1_pos1_um, finger2_pos1_um], hashset! {}),
                touchpad_event(vec![finger1_pos2_um, finger2_pos2_um], hashset! {1}),
                touchpad_event(vec![finger1_pos3_um, finger2_pos3_um], hashset! {}),
                touchpad_event(vec![finger1_pos4_um, finger2_pos4_um], hashset! {}),
                touchpad_event(vec![finger1_pos5_um, finger2_pos5_um], hashset! {1}),
            ];

            let got = utils::run_gesture_arena_test(inputs).await;

            assert_eq!(got.len(), 6);
            assert_eq!(got[0].as_slice(), []);
            assert_eq!(got[1].as_slice(), []);
            assert_matches!(got[2].as_slice(), [
              utils::expect_mouse_event!(phase: phase_a, pressed_buttons: pressed_button_a, affected_buttons: affected_button_a, location: location_a),
            ] => {
              assert_eq!(phase_a, &mouse_binding::MousePhase::Down);
              assert_eq!(pressed_button_a, &hashset! {2});
              assert_eq!(affected_button_a, &hashset! {2});
              assert_eq!(location_a, &utils::NO_MOVEMENT_LOCATION);
            });
            assert_matches!(got[3].as_slice(), [
              utils::expect_mouse_event!(phase: phase_a, pressed_buttons: pressed_button_a, affected_buttons: affected_button_a, location: location_a),
            ] => {
              assert_eq!(phase_a, &mouse_binding::MousePhase::Up);
              assert_eq!(pressed_button_a, &hashset! {});
              assert_eq!(affected_button_a, &hashset! {2});
              assert_eq!(location_a, &utils::NO_MOVEMENT_LOCATION);
            });
            assert_eq!(got[4].as_slice(), []);
            assert_matches!(got[5].as_slice(), [
              utils::expect_mouse_event!(phase: phase_a, pressed_buttons: pressed_button_a, affected_buttons: affected_button_a, location: location_a),
            ] => {
              assert_eq!(phase_a, &mouse_binding::MousePhase::Down);
              assert_eq!(pressed_button_a, &hashset! {2});
              assert_eq!(affected_button_a, &hashset! {2});
              assert_eq!(location_a, &utils::NO_MOVEMENT_LOCATION);
            });
        }

        #[fuchsia::test(allow_stalls = false)]
        async fn secondary_click_then_motion() {
            let finger1_pos0_um = Position { x: 2_000.0, y: 3_000.0 };
            let finger1_pos1_um = finger1_pos0_um.clone();
            let finger2_pos1_um = Position { x: 2_000.0, y: 5_000.0 };
            let finger1_pos2_um = finger1_pos1_um.clone();
            let finger2_pos2_um = finger2_pos1_um.clone();
            let finger1_pos3_um = finger1_pos2_um.clone();
            let finger2_pos3_um = finger2_pos2_um.clone();
            let finger1_pos4_um = finger1_pos3_um.clone();
            let finger1_pos5_um = finger1_pos4_um
                + Position {
                    x: 0.0,
                    y: 1_000.0 + args::SPURIOUS_TO_INTENTIONAL_MOTION_THRESHOLD_MM * 1_000.0,
                };

            let inputs = vec![
                touchpad_event(vec![finger1_pos0_um], hashset! {}),
                touchpad_event(vec![finger1_pos1_um, finger2_pos1_um], hashset! {}),
                touchpad_event(vec![finger1_pos2_um, finger2_pos2_um], hashset! {1}),
                touchpad_event(vec![finger1_pos3_um, finger2_pos3_um], hashset! {}),
                touchpad_event(vec![finger1_pos4_um], hashset! {}),
                touchpad_event(vec![finger1_pos5_um], hashset! {}),
            ];

            let got = utils::run_gesture_arena_test(inputs).await;

            assert_eq!(got.len(), 6);
            assert_eq!(got[0].as_slice(), []);
            assert_eq!(got[1].as_slice(), []);
            assert_matches!(got[2].as_slice(), [
              utils::expect_mouse_event!(phase: phase_a, pressed_buttons: pressed_button_a, affected_buttons: affected_button_a, location: location_a),
            ] => {
              assert_eq!(phase_a, &mouse_binding::MousePhase::Down);
              assert_eq!(pressed_button_a, &hashset! {2});
              assert_eq!(affected_button_a, &hashset! {2});
              assert_eq!(location_a, &utils::NO_MOVEMENT_LOCATION);
            });
            assert_matches!(got[3].as_slice(), [
              utils::expect_mouse_event!(phase: phase_a, pressed_buttons: pressed_button_a, affected_buttons: affected_button_a, location: location_a),
            ] => {
              assert_eq!(phase_a, &mouse_binding::MousePhase::Up);
              assert_eq!(pressed_button_a, &hashset! {});
              assert_eq!(affected_button_a, &hashset! {2});
              assert_eq!(location_a, &utils::NO_MOVEMENT_LOCATION);
            });
            assert_eq!(got[4].as_slice(), []);
            assert_matches!(got[5].as_slice(), [
              utils::expect_mouse_event!(phase: phase_a, pressed_buttons: pressed_button_a, affected_buttons: affected_button_a, location: location_a),
            ] => {
              assert_eq!(phase_a, &mouse_binding::MousePhase::Move);
              assert_eq!(pressed_button_a, &hashset! {});
              assert_eq!(affected_button_a, &hashset! {});
              assert_near!(location_a.millimeters.x, 0.0, utils::EPSILON);
              assert_gt!(location_a.millimeters.y, 0.0);
            });
        }

        #[fuchsia::test(allow_stalls = false)]
        async fn secondary_click_then_scroll() {
            let finger1_pos0_um = Position { x: 2_000.0, y: 3_000.0 };
            let finger1_pos1_um = finger1_pos0_um.clone();
            let finger2_pos1_um = Position { x: 2_000.0, y: 5_000.0 };
            let finger1_pos2_um = finger1_pos1_um.clone();
            let finger2_pos2_um = finger2_pos1_um.clone();
            let finger1_pos3_um = finger1_pos2_um.clone();
            let finger2_pos3_um = finger2_pos2_um.clone();
            let finger1_pos4_um = finger1_pos3_um
                + Position {
                    x: 0.0,
                    y: 1_000.0
                        + args::SPURIOUS_TO_INTENTIONAL_MOTION_THRESHOLD_BUTTON_CHANGE_MM * 1_000.0,
                };
            let finger2_pos4_um = finger2_pos3_um.clone();
            let finger1_pos5_um = finger1_pos4_um
                + Position {
                    x: 0.0,
                    y: 1_000.0 + args::SPURIOUS_TO_INTENTIONAL_MOTION_THRESHOLD_MM * 1_000.0,
                };
            let finger2_pos5_um = finger2_pos4_um
                + Position {
                    x: 0.0,
                    y: 1_000.0 + args::SPURIOUS_TO_INTENTIONAL_MOTION_THRESHOLD_MM * 1_000.0,
                };

            let inputs = vec![
                touchpad_event(vec![finger1_pos0_um], hashset! {}),
                touchpad_event(vec![finger1_pos1_um, finger2_pos1_um], hashset! {}),
                touchpad_event(vec![finger1_pos2_um, finger2_pos2_um], hashset! {1}),
                touchpad_event(vec![finger1_pos3_um, finger2_pos3_um], hashset! {}),
                // need extra movement to exit ButtonUpWinner.
                touchpad_event(vec![finger1_pos4_um, finger2_pos4_um], hashset! {}),
                touchpad_event(vec![finger1_pos5_um, finger2_pos5_um], hashset! {}),
            ];

            let got = utils::run_gesture_arena_test(inputs).await;

            assert_eq!(got.len(), 6);
            assert_eq!(got[0].as_slice(), []);
            assert_eq!(got[1].as_slice(), []);
            assert_matches!(got[2].as_slice(), [
              utils::expect_mouse_event!(phase: phase_a, pressed_buttons: pressed_button_a, affected_buttons: affected_button_a, location: location_a),
            ] => {
              assert_eq!(phase_a, &mouse_binding::MousePhase::Down);
              assert_eq!(pressed_button_a, &hashset! {2});
              assert_eq!(affected_button_a, &hashset! {2});
              assert_eq!(location_a, &utils::NO_MOVEMENT_LOCATION);
            });
            assert_matches!(got[3].as_slice(), [
              utils::expect_mouse_event!(phase: phase_a, pressed_buttons: pressed_button_a, affected_buttons: affected_button_a, location: location_a),
            ] => {
              assert_eq!(phase_a, &mouse_binding::MousePhase::Up);
              assert_eq!(pressed_button_a, &hashset! {});
              assert_eq!(affected_button_a, &hashset! {2});
              assert_eq!(location_a, &utils::NO_MOVEMENT_LOCATION);
            });
            assert_eq!(got[4].as_slice(), []);
            assert_matches!(got[5].as_slice(), [
              utils::expect_mouse_event!(phase: phase, delta_v: delta_v, delta_h: delta_h, location: location),
            ] => {
              assert_eq!(phase, &mouse_binding::MousePhase::Wheel);
              assert_matches!(delta_v, utils::extract_wheel_delta!(delta) => {
                assert_gt!(*delta, 0.0);
              });
              assert_eq!(*delta_h, None);
              assert_eq!(location, &utils::NO_MOVEMENT_LOCATION);
            });
        }
    }

    mod drag {
        use super::super::super::utils;
        use super::touchpad_event;
        use crate::gestures::args;
        use crate::{input_device, mouse_binding, Position};
        use assert_matches::assert_matches;
        use maplit::hashset;
        use pretty_assertions::assert_eq;
        use test_util::{assert_gt, assert_near};

        #[fuchsia::test(allow_stalls = false)]
        async fn secondary_drag_keep_contact() {
            let finger1_pos0_um = Position { x: 2_000.0, y: 3_000.0 };
            let finger1_pos1_um = finger1_pos0_um.clone();
            let finger2_pos1_um = Position { x: 2_000.0, y: 5_000.0 };
            let finger1_pos2_um = finger1_pos1_um.clone();
            let finger2_pos2_um = finger2_pos1_um.clone();
            let finger1_pos3_um = finger1_pos2_um
                + Position {
                    x: 0.0,
                    y: 1_000.0
                        + args::SPURIOUS_TO_INTENTIONAL_MOTION_THRESHOLD_BUTTON_CHANGE_MM * 1_000.0,
                };
            let finger2_pos3_um = finger2_pos2_um.clone();
            let finger1_pos4_um = finger1_pos3_um.clone();
            let finger2_pos4_um = finger2_pos3_um.clone();
            let finger1_pos5_um = finger1_pos4_um.clone();
            let finger2_pos5_um = finger2_pos4_um.clone();

            let inputs = vec![
                touchpad_event(vec![finger1_pos0_um], hashset! {}),
                touchpad_event(vec![finger1_pos1_um, finger2_pos1_um], hashset! {}),
                touchpad_event(vec![finger1_pos2_um, finger2_pos2_um], hashset! {1}),
                touchpad_event(vec![finger1_pos3_um, finger2_pos3_um], hashset! {1}),
                touchpad_event(vec![finger1_pos4_um, finger2_pos4_um], hashset! {}),
                // ensure no unexpected tap event.
                touchpad_event(vec![finger1_pos5_um, finger2_pos5_um], hashset! {}),
                touchpad_event(vec![], hashset! {}),
            ];

            let got = utils::run_gesture_arena_test(inputs).await;

            assert_eq!(got.len(), 7);
            assert_eq!(got[0].as_slice(), []);
            assert_eq!(got[1].as_slice(), []);
            assert_matches!(got[2].as_slice(), [
              utils::expect_mouse_event!(phase: phase_a, pressed_buttons: pressed_button_a, affected_buttons: affected_button_a, location: location_a),
            ] => {
              assert_eq!(phase_a, &mouse_binding::MousePhase::Down);
              assert_eq!(pressed_button_a, &hashset! {2});
              assert_eq!(affected_button_a, &hashset! {2});
              assert_eq!(location_a, &utils::NO_MOVEMENT_LOCATION);
            });
            assert_matches!(got[3].as_slice(), [
              utils::expect_mouse_event!(phase: phase_a, pressed_buttons: pressed_button_a, affected_buttons: affected_button_a, location: location_a),
            ] => {
              assert_eq!(phase_a, &mouse_binding::MousePhase::Move);
              assert_eq!(pressed_button_a, &hashset! {2});
              assert_eq!(affected_button_a, &hashset! {});
              assert_near!(location_a.millimeters.x, 0.0, utils::EPSILON);
              assert_gt!(location_a.millimeters.y, 0.0);
            });
            assert_matches!(got[4].as_slice(), [
              utils::expect_mouse_event!(phase: phase_a, pressed_buttons: pressed_button_a, affected_buttons: affected_button_a, location: location_a),
            ] => {
              assert_eq!(phase_a, &mouse_binding::MousePhase::Up);
              assert_eq!(pressed_button_a, &hashset! {});
              assert_eq!(affected_button_a, &hashset! {2});
              assert_eq!(location_a, &utils::NO_MOVEMENT_LOCATION);
            });
            assert_eq!(got[5].as_slice(), []);
            assert_eq!(got[6].as_slice(), []);
        }

        #[fuchsia::test(allow_stalls = false)]
        async fn secondary_drag_lift_one_then_other() {
            let finger1_pos0_um = Position { x: 2_000.0, y: 3_000.0 };
            let finger1_pos1_um = finger1_pos0_um.clone();
            let finger2_pos1_um = Position { x: 2_000.0, y: 5_000.0 };
            let finger1_pos2_um = finger1_pos1_um.clone();
            let finger2_pos2_um = finger2_pos1_um.clone();
            let finger1_pos3_um = finger1_pos2_um
                + Position {
                    x: 0.0,
                    y: 1_000.0
                        + args::SPURIOUS_TO_INTENTIONAL_MOTION_THRESHOLD_BUTTON_CHANGE_MM * 1_000.0,
                };
            let finger2_pos3_um = finger2_pos2_um.clone();
            let finger1_pos4_um = finger1_pos3_um.clone();
            let finger2_pos4_um = finger2_pos3_um.clone();
            let finger1_pos5_um = finger1_pos4_um.clone();

            let inputs = vec![
                touchpad_event(vec![finger1_pos0_um], hashset! {}),
                touchpad_event(vec![finger1_pos1_um, finger2_pos1_um], hashset! {}),
                touchpad_event(vec![finger1_pos2_um, finger2_pos2_um], hashset! {1}),
                touchpad_event(vec![finger1_pos3_um, finger2_pos3_um], hashset! {1}),
                touchpad_event(vec![finger1_pos4_um, finger2_pos4_um], hashset! {}),
                // ensure no unexpected tap event.
                touchpad_event(vec![finger1_pos5_um], hashset! {}),
                touchpad_event(vec![], hashset! {}),
            ];

            let got = utils::run_gesture_arena_test(inputs).await;

            assert_eq!(got.len(), 7);
            assert_eq!(got[0].as_slice(), []);
            assert_eq!(got[1].as_slice(), []);
            assert_matches!(got[2].as_slice(), [
              utils::expect_mouse_event!(phase: phase_a, pressed_buttons: pressed_button_a, affected_buttons: affected_button_a, location: location_a),
            ] => {
              assert_eq!(phase_a, &mouse_binding::MousePhase::Down);
              assert_eq!(pressed_button_a, &hashset! {2});
              assert_eq!(affected_button_a, &hashset! {2});
              assert_eq!(location_a, &utils::NO_MOVEMENT_LOCATION);
            });
            assert_matches!(got[3].as_slice(), [
              utils::expect_mouse_event!(phase: phase_a, pressed_buttons: pressed_button_a, affected_buttons: affected_button_a, location: location_a),
            ] => {
              assert_eq!(phase_a, &mouse_binding::MousePhase::Move);
              assert_eq!(pressed_button_a, &hashset! {2});
              assert_eq!(affected_button_a, &hashset! {});
              assert_near!(location_a.millimeters.x, 0.0, utils::EPSILON);
              assert_gt!(location_a.millimeters.y, 0.0);
            });
            assert_matches!(got[4].as_slice(), [
              utils::expect_mouse_event!(phase: phase_a, pressed_buttons: pressed_button_a, affected_buttons: affected_button_a, location: location_a),
            ] => {
              assert_eq!(phase_a, &mouse_binding::MousePhase::Up);
              assert_eq!(pressed_button_a, &hashset! {});
              assert_eq!(affected_button_a, &hashset! {2});
              assert_eq!(location_a, &utils::NO_MOVEMENT_LOCATION);
            });
            assert_eq!(got[5].as_slice(), []);
            assert_eq!(got[6].as_slice(), []);
        }

        #[fuchsia::test(allow_stalls = false)]
        async fn secondary_drag_use_1_finger() {
            let finger1_pos0_um = Position { x: 2_000.0, y: 3_000.0 };
            let finger1_pos1_um = finger1_pos0_um.clone();
            let finger2_pos1_um = Position { x: 2_000.0, y: 5_000.0 };
            let finger1_pos2_um = finger1_pos1_um.clone();
            let finger2_pos2_um = finger2_pos1_um.clone();
            let finger1_pos3_um = finger1_pos2_um.clone();
            let finger1_pos4_um = finger1_pos3_um
                + Position {
                    x: 0.0,
                    y: 1_000.0
                        + args::SPURIOUS_TO_INTENTIONAL_MOTION_THRESHOLD_BUTTON_CHANGE_MM * 1_000.0,
                };
            let finger1_pos5_um = finger1_pos4_um.clone();

            let inputs = vec![
                touchpad_event(vec![finger1_pos0_um], hashset! {}),
                touchpad_event(vec![finger1_pos1_um, finger2_pos1_um], hashset! {}),
                touchpad_event(vec![finger1_pos2_um, finger2_pos2_um], hashset! {1}),
                touchpad_event(vec![finger1_pos3_um], hashset! {1}),
                touchpad_event(vec![finger1_pos4_um], hashset! {1}),
                touchpad_event(vec![finger1_pos5_um], hashset! {}),
                touchpad_event(vec![], hashset! {}),
            ];

            let got = utils::run_gesture_arena_test(inputs).await;

            assert_eq!(got.len(), 7);
            assert_eq!(got[0].as_slice(), []);
            assert_eq!(got[1].as_slice(), []);
            assert_matches!(got[2].as_slice(), [
              utils::expect_mouse_event!(phase: phase_a, pressed_buttons: pressed_button_a, affected_buttons: affected_button_a, location: location_a),
            ] => {
              assert_eq!(phase_a, &mouse_binding::MousePhase::Down);
              assert_eq!(pressed_button_a, &hashset! {2});
              assert_eq!(affected_button_a, &hashset! {2});
              assert_eq!(location_a, &utils::NO_MOVEMENT_LOCATION);
            });
            assert_eq!(got[3].as_slice(), []);
            assert_matches!(got[4].as_slice(), [
              utils::expect_mouse_event!(phase: phase_a, pressed_buttons: pressed_button_a, affected_buttons: affected_button_a, location: location_a),
            ] => {
              assert_eq!(phase_a, &mouse_binding::MousePhase::Move);
              assert_eq!(pressed_button_a, &hashset! {2});
              assert_eq!(affected_button_a, &hashset! {});
              assert_near!(location_a.millimeters.x, 0.0, utils::EPSILON);
              assert_gt!(location_a.millimeters.y, 0.0);
            });
            assert_matches!(got[5].as_slice(), [
              utils::expect_mouse_event!(phase: phase_a, pressed_buttons: pressed_button_a, affected_buttons: affected_button_a, location: location_a),
            ] => {
              assert_eq!(phase_a, &mouse_binding::MousePhase::Up);
              assert_eq!(pressed_button_a, &hashset! {});
              assert_eq!(affected_button_a, &hashset! {2});
              assert_eq!(location_a, &utils::NO_MOVEMENT_LOCATION);
            });
            assert_eq!(got[6].as_slice(), []);
        }
    }

    mod drag_chain {
        use super::super::super::utils;
        use super::touchpad_event;
        use crate::gestures::args;
        use crate::{input_device, mouse_binding, Position};
        use assert_matches::assert_matches;
        use maplit::hashset;
        use pretty_assertions::assert_eq;
        use test_util::{assert_gt, assert_near};

        #[fuchsia::test(allow_stalls = false)]
        async fn secondary_drag_then_one_finger_button_down() {
            let finger1_pos0_um = Position { x: 2_000.0, y: 3_000.0 };
            let finger1_pos1_um = finger1_pos0_um.clone();
            let finger2_pos1_um = Position { x: 2_000.0, y: 5_000.0 };
            let finger1_pos2_um = finger1_pos1_um.clone();
            let finger2_pos2_um = finger2_pos1_um.clone();
            let finger1_pos3_um = finger1_pos2_um
                + Position {
                    x: 0.0,
                    y: 1_000.0
                        + args::SPURIOUS_TO_INTENTIONAL_MOTION_THRESHOLD_BUTTON_CHANGE_MM * 1_000.0,
                };
            let finger2_pos3_um = finger2_pos2_um.clone();
            let finger1_pos4_um = finger1_pos3_um.clone();
            let finger2_pos4_um = finger2_pos3_um.clone();
            let finger1_pos5_um = finger1_pos4_um.clone();
            let finger1_pos6_um = finger1_pos5_um.clone();

            let inputs = vec![
                touchpad_event(vec![finger1_pos0_um], hashset! {}),
                touchpad_event(vec![finger1_pos1_um, finger2_pos1_um], hashset! {}),
                touchpad_event(vec![finger1_pos2_um, finger2_pos2_um], hashset! {1}),
                touchpad_event(vec![finger1_pos3_um, finger2_pos3_um], hashset! {1}),
                touchpad_event(vec![finger1_pos4_um, finger2_pos4_um], hashset! {}),
                touchpad_event(vec![finger1_pos5_um], hashset! {}),
                touchpad_event(vec![finger1_pos6_um], hashset! {1}),
            ];

            let got = utils::run_gesture_arena_test(inputs).await;

            assert_eq!(got.len(), 7);
            assert_eq!(got[0].as_slice(), []);
            assert_eq!(got[1].as_slice(), []);
            assert_matches!(got[2].as_slice(), [
              utils::expect_mouse_event!(phase: phase_a, pressed_buttons: pressed_button_a, affected_buttons: affected_button_a, location: location_a),
            ] => {
              assert_eq!(phase_a, &mouse_binding::MousePhase::Down);
              assert_eq!(pressed_button_a, &hashset! {2});
              assert_eq!(affected_button_a, &hashset! {2});
              assert_eq!(location_a, &utils::NO_MOVEMENT_LOCATION);
            });
            assert_matches!(got[3].as_slice(), [
              utils::expect_mouse_event!(phase: phase_a, pressed_buttons: pressed_button_a, affected_buttons: affected_button_a, location: location_a),
            ] => {
              assert_eq!(phase_a, &mouse_binding::MousePhase::Move);
              assert_eq!(pressed_button_a, &hashset! {2});
              assert_eq!(affected_button_a, &hashset! {});
              assert_near!(location_a.millimeters.x, 0.0, utils::EPSILON);
              assert_gt!(location_a.millimeters.y, 0.0);
            });
            assert_matches!(got[4].as_slice(), [
              utils::expect_mouse_event!(phase: phase_a, pressed_buttons: pressed_button_a, affected_buttons: affected_button_a, location: location_a),
            ] => {
              assert_eq!(phase_a, &mouse_binding::MousePhase::Up);
              assert_eq!(pressed_button_a, &hashset! {});
              assert_eq!(affected_button_a, &hashset! {2});
              assert_eq!(location_a, &utils::NO_MOVEMENT_LOCATION);
            });
            assert_eq!(got[5].as_slice(), []);
            assert_matches!(got[6].as_slice(), [
              utils::expect_mouse_event!(phase: phase_a, pressed_buttons: pressed_button_a, affected_buttons: affected_button_a, location: location_a),
            ] => {
              assert_eq!(phase_a, &mouse_binding::MousePhase::Down);
              assert_eq!(pressed_button_a, &hashset! {1});
              assert_eq!(affected_button_a, &hashset! {1});
              assert_eq!(location_a, &utils::NO_MOVEMENT_LOCATION);
            });
        }

        #[fuchsia::test(allow_stalls = false)]
        async fn secondary_drag_then_secondary_button_down() {
            let finger1_pos0_um = Position { x: 2_000.0, y: 3_000.0 };
            let finger1_pos1_um = finger1_pos0_um.clone();
            let finger2_pos1_um = Position { x: 2_000.0, y: 5_000.0 };
            let finger1_pos2_um = finger1_pos1_um.clone();
            let finger2_pos2_um = finger2_pos1_um.clone();
            let finger1_pos3_um = finger1_pos2_um
                + Position {
                    x: 0.0,
                    y: 1_000.0
                        + args::SPURIOUS_TO_INTENTIONAL_MOTION_THRESHOLD_BUTTON_CHANGE_MM * 1_000.0,
                };
            let finger2_pos3_um = finger2_pos2_um.clone();
            let finger1_pos4_um = finger1_pos3_um.clone();
            let finger2_pos4_um = finger2_pos3_um.clone();
            let finger1_pos5_um = finger1_pos4_um.clone();
            let finger2_pos5_um = finger2_pos4_um.clone();

            let inputs = vec![
                touchpad_event(vec![finger1_pos0_um], hashset! {}),
                touchpad_event(vec![finger1_pos1_um, finger2_pos1_um], hashset! {}),
                touchpad_event(vec![finger1_pos2_um, finger2_pos2_um], hashset! {1}),
                touchpad_event(vec![finger1_pos3_um, finger2_pos3_um], hashset! {1}),
                touchpad_event(vec![finger1_pos4_um, finger2_pos4_um], hashset! {}),
                touchpad_event(vec![finger1_pos5_um, finger2_pos5_um], hashset! {1}),
            ];

            let got = utils::run_gesture_arena_test(inputs).await;

            assert_eq!(got.len(), 6);
            assert_eq!(got[0].as_slice(), []);
            assert_eq!(got[1].as_slice(), []);
            assert_matches!(got[2].as_slice(), [
              utils::expect_mouse_event!(phase: phase_a, pressed_buttons: pressed_button_a, affected_buttons: affected_button_a, location: location_a),
            ] => {
              assert_eq!(phase_a, &mouse_binding::MousePhase::Down);
              assert_eq!(pressed_button_a, &hashset! {2});
              assert_eq!(affected_button_a, &hashset! {2});
              assert_eq!(location_a, &utils::NO_MOVEMENT_LOCATION);
            });
            assert_matches!(got[3].as_slice(), [
              utils::expect_mouse_event!(phase: phase_a, pressed_buttons: pressed_button_a, affected_buttons: affected_button_a, location: location_a),
            ] => {
              assert_eq!(phase_a, &mouse_binding::MousePhase::Move);
              assert_eq!(pressed_button_a, &hashset! {2});
              assert_eq!(affected_button_a, &hashset! {});
              assert_near!(location_a.millimeters.x, 0.0, utils::EPSILON);
              assert_gt!(location_a.millimeters.y, 0.0);
            });
            assert_matches!(got[4].as_slice(), [
              utils::expect_mouse_event!(phase: phase_a, pressed_buttons: pressed_button_a, affected_buttons: affected_button_a, location: location_a),
            ] => {
              assert_eq!(phase_a, &mouse_binding::MousePhase::Up);
              assert_eq!(pressed_button_a, &hashset! {});
              assert_eq!(affected_button_a, &hashset! {2});
              assert_eq!(location_a, &utils::NO_MOVEMENT_LOCATION);
            });
            assert_matches!(got[5].as_slice(), [
              utils::expect_mouse_event!(phase: phase_a, pressed_buttons: pressed_button_a, affected_buttons: affected_button_a, location: location_a),
            ] => {
              assert_eq!(phase_a, &mouse_binding::MousePhase::Down);
              assert_eq!(pressed_button_a, &hashset! {2});
              assert_eq!(affected_button_a, &hashset! {2});
              assert_eq!(location_a, &utils::NO_MOVEMENT_LOCATION);
            });
        }

        #[fuchsia::test(allow_stalls = false)]
        async fn secondary_drag_then_motion() {
            let finger1_pos0_um = Position { x: 2_000.0, y: 3_000.0 };
            let finger1_pos1_um = finger1_pos0_um.clone();
            let finger2_pos1_um = Position { x: 2_000.0, y: 5_000.0 };
            let finger1_pos2_um = finger1_pos1_um.clone();
            let finger2_pos2_um = finger2_pos1_um.clone();
            let finger1_pos3_um = finger1_pos2_um
                + Position {
                    x: 0.0,
                    y: 1_000.0
                        + args::SPURIOUS_TO_INTENTIONAL_MOTION_THRESHOLD_BUTTON_CHANGE_MM * 1_000.0,
                };
            let finger2_pos3_um = finger2_pos2_um.clone();
            let finger1_pos4_um = finger1_pos3_um.clone();
            let finger2_pos4_um = finger2_pos3_um.clone();
            let finger1_pos5_um = finger1_pos4_um.clone();
            let finger1_pos6_um = finger1_pos5_um
                + Position {
                    x: 0.0,
                    y: 1_000.0 + args::SPURIOUS_TO_INTENTIONAL_MOTION_THRESHOLD_MM * 1_000.0,
                };

            let inputs = vec![
                touchpad_event(vec![finger1_pos0_um], hashset! {}),
                touchpad_event(vec![finger1_pos1_um, finger2_pos1_um], hashset! {}),
                touchpad_event(vec![finger1_pos2_um, finger2_pos2_um], hashset! {1}),
                touchpad_event(vec![finger1_pos3_um, finger2_pos3_um], hashset! {1}),
                touchpad_event(vec![finger1_pos4_um, finger2_pos4_um], hashset! {}),
                touchpad_event(vec![finger1_pos5_um], hashset! {}),
                touchpad_event(vec![finger1_pos6_um], hashset! {}),
            ];

            let got = utils::run_gesture_arena_test(inputs).await;

            assert_eq!(got.len(), 7);
            assert_eq!(got[0].as_slice(), []);
            assert_eq!(got[1].as_slice(), []);
            assert_matches!(got[2].as_slice(), [
              utils::expect_mouse_event!(phase: phase_a, pressed_buttons: pressed_button_a, affected_buttons: affected_button_a, location: location_a),
            ] => {
              assert_eq!(phase_a, &mouse_binding::MousePhase::Down);
              assert_eq!(pressed_button_a, &hashset! {2});
              assert_eq!(affected_button_a, &hashset! {2});
              assert_eq!(location_a, &utils::NO_MOVEMENT_LOCATION);
            });
            assert_matches!(got[3].as_slice(), [
              utils::expect_mouse_event!(phase: phase_a, pressed_buttons: pressed_button_a, affected_buttons: affected_button_a, location: location_a),
            ] => {
              assert_eq!(phase_a, &mouse_binding::MousePhase::Move);
              assert_eq!(pressed_button_a, &hashset! {2});
              assert_eq!(affected_button_a, &hashset! {});
              assert_near!(location_a.millimeters.x, 0.0, utils::EPSILON);
              assert_gt!(location_a.millimeters.y, 0.0);
            });
            assert_matches!(got[4].as_slice(), [
              utils::expect_mouse_event!(phase: phase_a, pressed_buttons: pressed_button_a, affected_buttons: affected_button_a, location: location_a),
            ] => {
              assert_eq!(phase_a, &mouse_binding::MousePhase::Up);
              assert_eq!(pressed_button_a, &hashset! {});
              assert_eq!(affected_button_a, &hashset! {2});
              assert_eq!(location_a, &utils::NO_MOVEMENT_LOCATION);
            });
            assert_eq!(got[5].as_slice(), []);
            assert_matches!(got[6].as_slice(), [
              utils::expect_mouse_event!(phase: phase_a, pressed_buttons: pressed_button_a, affected_buttons: affected_button_a, location: location_a),
            ] => {
              assert_eq!(phase_a, &mouse_binding::MousePhase::Move);
              assert_eq!(pressed_button_a, &hashset! {});
              assert_eq!(affected_button_a, &hashset! {});
              assert_near!(location_a.millimeters.x, 0.0, utils::EPSILON);
              assert_gt!(location_a.millimeters.y, 0.0);
            });
        }

        #[fuchsia::test(allow_stalls = false)]
        async fn secondary_drag_then_scroll() {
            let finger1_pos0_um = Position { x: 2_000.0, y: 3_000.0 };
            let finger1_pos1_um = finger1_pos0_um.clone();
            let finger2_pos1_um = Position { x: 2_000.0, y: 5_000.0 };
            let finger1_pos2_um = finger1_pos1_um.clone();
            let finger2_pos2_um = finger2_pos1_um.clone();
            let finger1_pos3_um = finger1_pos2_um.clone()
                + Position {
                    x: 0.0,
                    y: 1_000.0
                        + args::SPURIOUS_TO_INTENTIONAL_MOTION_THRESHOLD_BUTTON_CHANGE_MM * 1_000.0,
                };
            let finger2_pos3_um = finger2_pos2_um.clone();
            let finger1_pos4_um = finger1_pos3_um.clone();
            let finger2_pos4_um = finger2_pos3_um.clone();
            let finger1_pos5_um = finger1_pos4_um
                + Position {
                    x: 0.0,
                    y: 1_000.0
                        + args::SPURIOUS_TO_INTENTIONAL_MOTION_THRESHOLD_BUTTON_CHANGE_MM * 1_000.0,
                };
            let finger2_pos5_um = finger2_pos4_um.clone();
            let finger1_pos6_um = finger1_pos5_um
                + Position {
                    x: 0.0,
                    y: 1_000.0 + args::SPURIOUS_TO_INTENTIONAL_MOTION_THRESHOLD_MM * 1_000.0,
                };
            let finger2_pos6_um = finger2_pos5_um
                + Position {
                    x: 0.0,
                    y: 1_000.0 + args::SPURIOUS_TO_INTENTIONAL_MOTION_THRESHOLD_MM * 1_000.0,
                };

            let inputs = vec![
                touchpad_event(vec![finger1_pos0_um], hashset! {}),
                touchpad_event(vec![finger1_pos1_um, finger2_pos1_um], hashset! {}),
                touchpad_event(vec![finger1_pos2_um, finger2_pos2_um], hashset! {1}),
                touchpad_event(vec![finger1_pos3_um, finger2_pos3_um], hashset! {1}),
                touchpad_event(vec![finger1_pos4_um, finger2_pos4_um], hashset! {}),
                touchpad_event(vec![finger1_pos5_um, finger2_pos5_um], hashset! {}),
                touchpad_event(vec![finger1_pos6_um, finger2_pos6_um], hashset! {}),
            ];

            let got = utils::run_gesture_arena_test(inputs).await;

            assert_eq!(got.len(), 7);
            assert_eq!(got[0].as_slice(), []);
            assert_eq!(got[1].as_slice(), []);
            assert_matches!(got[2].as_slice(), [
              utils::expect_mouse_event!(phase: phase_a, pressed_buttons: pressed_button_a, affected_buttons: affected_button_a, location: location_a),
            ] => {
              assert_eq!(phase_a, &mouse_binding::MousePhase::Down);
              assert_eq!(pressed_button_a, &hashset! {2});
              assert_eq!(affected_button_a, &hashset! {2});
              assert_eq!(location_a, &utils::NO_MOVEMENT_LOCATION);
            });
            assert_matches!(got[3].as_slice(), [
              utils::expect_mouse_event!(phase: phase_a, pressed_buttons: pressed_button_a, affected_buttons: affected_button_a, location: location_a),
            ] => {
              assert_eq!(phase_a, &mouse_binding::MousePhase::Move);
              assert_eq!(pressed_button_a, &hashset! {2});
              assert_eq!(affected_button_a, &hashset! {});
              assert_near!(location_a.millimeters.x, 0.0, utils::EPSILON);
              assert_gt!(location_a.millimeters.y, 0.0);
            });
            assert_matches!(got[4].as_slice(), [
              utils::expect_mouse_event!(phase: phase_a, pressed_buttons: pressed_button_a, affected_buttons: affected_button_a, location: location_a),
            ] => {
              assert_eq!(phase_a, &mouse_binding::MousePhase::Up);
              assert_eq!(pressed_button_a, &hashset! {});
              assert_eq!(affected_button_a, &hashset! {2});
              assert_eq!(location_a, &utils::NO_MOVEMENT_LOCATION);
            });
            assert_eq!(got[5].as_slice(), []);
            assert_matches!(got[6].as_slice(), [
              utils::expect_mouse_event!(phase: phase, delta_v: delta_v, delta_h: delta_h, location: location),
            ] => {
              assert_eq!(phase, &mouse_binding::MousePhase::Wheel);
              assert_matches!(delta_v, utils::extract_wheel_delta!(delta) => {
                assert_gt!(*delta, 0.0);
              });
              assert_eq!(*delta_h, None);
              assert_eq!(location, &utils::NO_MOVEMENT_LOCATION);
            });
        }
    }
}
