// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#![cfg(test)]

use crate::utils::Position;
use crate::{
    consumer_controls_binding, input_device, input_handler, keyboard_binding, mouse_binding,
    touch_binding,
};
use assert_matches::assert_matches;
use futures::FutureExt as _;
use maplit::hashmap;
use std::collections::{HashMap, HashSet};
use {
    fidl_fuchsia_input_report as fidl_input_report, fidl_fuchsia_ui_input as fidl_ui_input,
    fidl_fuchsia_ui_input3 as fidl_ui_input3, fidl_fuchsia_ui_pointerinjector as pointerinjector,
    zx,
};

pub use diagnostics_assertions;

/// Returns the current time as an i64 for InputReports and zx::MonotonicInstant for InputEvents.
pub fn event_times() -> (i64, zx::MonotonicInstant) {
    let event_time = zx::MonotonicInstant::get();
    (event_time.into_nanos(), event_time)
}

/// Creates a [`fidl_input_report::InputReport`] with a keyboard report.
///
/// # Parameters
/// -`pressed_keys`: The input3 keys that will be added to the returned input report.
/// -`event_time`: The time in nanoseconds when the event was first recorded.
pub fn create_keyboard_input_report(
    pressed_keys: Vec<fidl_fuchsia_input::Key>,
    event_time: i64,
) -> fidl_input_report::InputReport {
    fidl_input_report::InputReport {
        event_time: Some(event_time),
        keyboard: Some(fidl_input_report::KeyboardInputReport {
            pressed_keys3: Some(pressed_keys),
            ..Default::default()
        }),
        mouse: None,
        touch: None,
        sensor: None,
        consumer_control: None,
        trace_id: None,
        ..Default::default()
    }
}

/// Creates a new [input_device::InputEvent] from the provided components.
pub fn create_input_event(
    keyboard_event: keyboard_binding::KeyboardEvent,
    device_descriptor: &input_device::InputDeviceDescriptor,
    event_time: zx::MonotonicInstant,
    handled: input_device::Handled,
) -> input_device::InputEvent {
    input_device::InputEvent {
        device_event: input_device::InputDeviceEvent::Keyboard(keyboard_event),
        device_descriptor: device_descriptor.clone(),
        event_time,
        handled,
        trace_id: None,
    }
}

/// Creates a [`keyboard_binding::KeyboardEvent`] with the provided keys, meaning, and handled state.
///
/// # Parameters
/// - `key`: The input3 key which changed state.
/// - `event_type`: The input3 key event type (e.g. pressed, released).
/// - `modifiers`: The input3 modifiers that are to be included as pressed.
/// - `event_time`: The timestamp in nanoseconds when the event was recorded.
/// - `device_descriptor`: The device descriptor to add to the event.
/// - `keymap`: The keymap which will be used to derive value for `key`.
/// - `handled`: Whether the event has been consumed by an upstream handler.
pub fn create_keyboard_event_with_handled(
    key: fidl_fuchsia_input::Key,
    event_type: fidl_fuchsia_ui_input3::KeyEventType,
    modifiers: Option<fidl_ui_input3::Modifiers>,
    event_time: zx::MonotonicInstant,
    device_descriptor: &input_device::InputDeviceDescriptor,
    keymap: Option<String>,
    key_meaning: Option<fidl_fuchsia_ui_input3::KeyMeaning>,
    handled: input_device::Handled,
) -> input_device::InputEvent {
    let keyboard_event = keyboard_binding::KeyboardEvent::new(key, event_type)
        .into_with_modifiers(modifiers)
        .into_with_keymap(keymap)
        .into_with_key_meaning(key_meaning);
    create_input_event(keyboard_event, device_descriptor, event_time, handled)
}

/// Creates a [`keyboard_binding::KeyboardEvent`] with the provided keys and meaning.
/// a repeat sequence.
///
/// # Parameters
/// - `key`: The input3 key which changed state.
/// - `event_type`: The input3 key event type (e.g. pressed, released).
/// - `modifiers`: The input3 modifiers that are to be included as pressed.
/// - `event_time`: The timestamp in nanoseconds when the event was recorded.
/// - `device_descriptor`: The device descriptor to add to the event.
/// - `keymap`: The keymap which will be used to derive value for `key`.
/// - `repeat_sequence`: The sequence of this key event in the autorepeat process.
pub fn create_keyboard_event_with_key_meaning_and_repeat_sequence(
    key: fidl_fuchsia_input::Key,
    event_type: fidl_fuchsia_ui_input3::KeyEventType,
    modifiers: Option<fidl_ui_input3::Modifiers>,
    event_time: zx::MonotonicInstant,
    device_descriptor: &input_device::InputDeviceDescriptor,
    keymap: Option<String>,
    key_meaning: Option<fidl_fuchsia_ui_input3::KeyMeaning>,
    repeat_sequence: u32,
) -> input_device::InputEvent {
    let device_event = keyboard_binding::KeyboardEvent::new(key, event_type)
        .into_with_modifiers(modifiers)
        .into_with_key_meaning(key_meaning)
        .into_with_keymap(keymap)
        .into_with_repeat_sequence(repeat_sequence);
    create_input_event(device_event, device_descriptor, event_time, input_device::Handled::No)
}

/// Creates a [`keyboard_binding::KeyboardEvent`] with the provided keys and meaning.
///
/// # Parameters
/// - `key`: The input3 key which changed state.
/// - `event_type`: The input3 key event type (e.g. pressed, released).
/// - `modifiers`: The input3 modifiers that are to be included as pressed.
/// - `event_time`: The timestamp in nanoseconds when the event was recorded.
/// - `device_descriptor`: The device descriptor to add to the event.
/// - `keymap`: The keymap which will be used to derive value for `key`.
pub fn create_keyboard_event_with_key_meaning(
    key: fidl_fuchsia_input::Key,
    event_type: fidl_fuchsia_ui_input3::KeyEventType,
    modifiers: Option<fidl_ui_input3::Modifiers>,
    event_time: zx::MonotonicInstant,
    device_descriptor: &input_device::InputDeviceDescriptor,
    keymap: Option<String>,
    key_meaning: Option<fidl_fuchsia_ui_input3::KeyMeaning>,
) -> input_device::InputEvent {
    create_keyboard_event_with_key_meaning_and_repeat_sequence(
        key,
        event_type,
        modifiers,
        event_time,
        device_descriptor,
        keymap,
        key_meaning,
        0,
    )
}

/// Creates a [`keyboard_binding::KeyboardEvent`] with the provided keys.
///
/// # Parameters
/// - `key`: The input3 key which changed state.
/// - `event_type`: The input3 key event type (e.g. pressed, released).
/// - `modifiers`: The input3 modifiers that are to be included as pressed.
/// - `event_time`: The timestamp in nanoseconds when the event was recorded.
/// - `device_descriptor`: The device descriptor to add to the event.
/// - `keymap`: The keymap which will be used to derive value for `key`.
pub fn create_keyboard_event_with_time(
    key: fidl_fuchsia_input::Key,
    event_type: fidl_fuchsia_ui_input3::KeyEventType,
    modifiers: Option<fidl_ui_input3::Modifiers>,
    event_time: zx::MonotonicInstant,
    device_descriptor: &input_device::InputDeviceDescriptor,
    keymap: Option<String>,
) -> input_device::InputEvent {
    create_keyboard_event_with_key_meaning(
        key,
        event_type,
        modifiers,
        event_time,
        device_descriptor,
        keymap,
        None,
    )
}

/// Creates a [`keyboard_binding::KeyboardEvent`] with the provided keys.
///
/// # Parameters
/// - `key`: The input3 key which changed state.
/// - `event_type`: The input3 key event type (e.g. pressed, released).
/// - `modifiers`: The input3 modifiers that are to be included as pressed.
/// - `device_descriptor`: The device descriptor to add to the event.
/// - `keymap`: The keymap which will be used to derive value for `key`.
pub fn create_keyboard_event(
    key: fidl_fuchsia_input::Key,
    event_type: fidl_fuchsia_ui_input3::KeyEventType,
    modifiers: Option<fidl_ui_input3::Modifiers>,
    device_descriptor: &input_device::InputDeviceDescriptor,
    keymap: Option<String>,
) -> input_device::InputEvent {
    create_keyboard_event_with_time(
        key,
        event_type,
        modifiers,
        zx::MonotonicInstant::get(),
        device_descriptor,
        keymap,
    )
}

/// Creates a fake input event with the given event time.  Please do not
/// read into other event fields.
pub fn create_fake_input_event(event_time: zx::MonotonicInstant) -> input_device::InputEvent {
    input_device::InputEvent {
        event_time,
        device_event: input_device::InputDeviceEvent::Fake,
        device_descriptor: input_device::InputDeviceDescriptor::Fake,
        handled: input_device::Handled::No,
        trace_id: None,
    }
}

/// Creates a fake handled input event with the given event time.  Please do not
/// read into other event fields.
pub fn create_fake_handled_input_event(
    event_time: zx::MonotonicInstant,
) -> input_device::InputEvent {
    input_device::InputEvent {
        event_time,
        device_event: input_device::InputDeviceEvent::Fake,
        device_descriptor: input_device::InputDeviceDescriptor::Fake,
        handled: input_device::Handled::Yes,
        trace_id: None,
    }
}

/// Creates an [`input_device::InputDeviceDescriptor`] for a consumer controls device.
pub fn consumer_controls_device_descriptor() -> input_device::InputDeviceDescriptor {
    input_device::InputDeviceDescriptor::ConsumerControls(
        consumer_controls_binding::ConsumerControlsDeviceDescriptor {
            buttons: vec![
                fidl_input_report::ConsumerControlButton::CameraDisable,
                fidl_input_report::ConsumerControlButton::FactoryReset,
                fidl_input_report::ConsumerControlButton::Function,
                fidl_input_report::ConsumerControlButton::MicMute,
                fidl_input_report::ConsumerControlButton::Pause,
                fidl_input_report::ConsumerControlButton::Power,
                fidl_input_report::ConsumerControlButton::VolumeDown,
                fidl_input_report::ConsumerControlButton::VolumeUp,
            ],
            device_id: 0,
        },
    )
}

/// Creates a [`fidl_input_report::InputReport`] with a consumer control report.
///
/// # Parameters
/// - `buttons`: The buttons in the consumer control report.
/// - `event_time`: The time of event.
pub fn create_consumer_control_input_report(
    buttons: Vec<fidl_input_report::ConsumerControlButton>,
    event_time: i64,
) -> fidl_input_report::InputReport {
    fidl_input_report::InputReport {
        event_time: Some(event_time),
        keyboard: None,
        mouse: None,
        touch: None,
        sensor: None,
        consumer_control: Some(fidl_input_report::ConsumerControlInputReport {
            pressed_buttons: Some(buttons),
            ..Default::default()
        }),
        trace_id: None,
        ..Default::default()
    }
}

/// Creates a [`consumer_controls_binding::ConsumerControlsEvent`] with the provided parameters.
///
/// # Parameters
/// - `pressed_buttons`: The buttons to report in the event.
/// - `event_time`: The time of event.
/// - `device_descriptor`: The device descriptor to add to the event.
/// - `handled`: Whether the event has been consumed.
pub fn create_consumer_controls_event_with_handled(
    pressed_buttons: Vec<fidl_input_report::ConsumerControlButton>,
    event_time: zx::MonotonicInstant,
    device_descriptor: &input_device::InputDeviceDescriptor,
    handled: input_device::Handled,
) -> input_device::InputEvent {
    input_device::InputEvent {
        device_event: input_device::InputDeviceEvent::ConsumerControls(
            consumer_controls_binding::ConsumerControlsEvent::new(pressed_buttons),
        ),
        device_descriptor: device_descriptor.clone(),
        event_time,
        handled,
        trace_id: None,
    }
}

/// Creates a [`consumer_controls_binding::ConsumerControlsEvent`] with the provided parameters.
///
/// # Parameters
/// - `pressed_buttons`: The buttons to report in the event.
/// - `event_time`: The time of event.
/// - `device_descriptor`: The device descriptor to add to the event.
pub fn create_consumer_controls_event(
    pressed_buttons: Vec<fidl_input_report::ConsumerControlButton>,
    event_time: zx::MonotonicInstant,
    device_descriptor: &input_device::InputDeviceDescriptor,
) -> input_device::InputEvent {
    create_consumer_controls_event_with_handled(
        pressed_buttons,
        event_time,
        device_descriptor,
        input_device::Handled::No,
    )
}

/// Creates a [`fidl_input_report::InputReport`] with a mouse report.
///
/// # Parameters
/// - `location`: The position of the mouse report, in input device coordinates.
/// - `wheel_delta_v`: The wheel delta in vertical.
/// - `wheel_delta_h`: The wheel delta in horizontal.
/// - `buttons`: The buttons to report as pressed in the mouse report.
/// - `event_time`: The time of event.
pub fn create_mouse_input_report_absolute(
    location: Position,
    scroll_v: Option<i64>,
    scroll_h: Option<i64>,
    buttons: Vec<u8>,
    event_time: i64,
) -> fidl_input_report::InputReport {
    fidl_input_report::InputReport {
        event_time: Some(event_time),
        keyboard: None,
        mouse: Some(fidl_input_report::MouseInputReport {
            position_x: Some(location.x as i64),
            position_y: Some(location.y as i64),
            scroll_v,
            scroll_h,
            pressed_buttons: Some(buttons),
            ..Default::default()
        }),
        ..Default::default()
    }
}

/// Creates a [`fidl_input_report::InputReport`] with a mouse report.
///
/// # Parameters
/// - `location`: The movement of the mouse report, in input device coordinates.
/// - `wheel_delta_v`: The wheel delta in vertical.
/// - `wheel_delta_h`: The wheel delta in horizontal.
/// - `buttons`: The buttons to report as pressed in the mouse report.
/// - `event_time`: The time of event.
pub fn create_mouse_input_report_relative(
    movement: Position,
    scroll_v: Option<i64>,
    scroll_h: Option<i64>,
    buttons: Vec<u8>,
    event_time: i64,
) -> fidl_input_report::InputReport {
    fidl_input_report::InputReport {
        event_time: Some(event_time),
        keyboard: None,
        mouse: Some(fidl_input_report::MouseInputReport {
            movement_x: Some(movement.x as i64),
            movement_y: Some(movement.y as i64),
            position_x: None,
            position_y: None,
            scroll_v: scroll_v,
            scroll_h: scroll_h,
            pressed_buttons: Some(buttons),
            ..Default::default()
        }),
        touch: None,
        sensor: None,
        consumer_control: None,
        trace_id: None,
        ..Default::default()
    }
}

/// Returns an |input_device::InputDeviceDescriptor::Mouse|.
pub fn get_mouse_device_descriptor() -> input_device::InputDeviceDescriptor {
    const COUNTS_PER_MM: u32 = 12;
    input_device::InputDeviceDescriptor::Mouse(mouse_binding::MouseDeviceDescriptor {
        device_id: 1,
        absolute_x_range: Some(fidl_input_report::Range { min: -50, max: 50 }),
        absolute_y_range: Some(fidl_input_report::Range { min: -50, max: 50 }),
        wheel_v_range: None,
        wheel_h_range: None,
        buttons: None,
        counts_per_mm: COUNTS_PER_MM,
    })
}

/// Creates a [`mouse_binding::MouseEvent`] with the provided parameters.
///
/// # Parameters
/// - `location`: The mouse location to report in the event.
/// - `wheel_delta_v`: The wheel delta in vertical.
/// - `wheel_delta_h`: The wheel delta in horizontal.
/// - `is_precision_scroll`: the wheel event has precision scroll delta.
/// - `phase`: The phase of the buttons in the event.
/// - `buttons`: The buttons to report in the event.
/// - `event_time`: The time of event.
/// - `device_descriptor`: The device descriptor to add to the event.
pub fn create_mouse_event_with_handled(
    location: mouse_binding::MouseLocation,
    wheel_delta_v: Option<mouse_binding::WheelDelta>,
    wheel_delta_h: Option<mouse_binding::WheelDelta>,
    is_precision_scroll: Option<mouse_binding::PrecisionScroll>,
    phase: mouse_binding::MousePhase,
    affected_buttons: HashSet<mouse_binding::MouseButton>,
    pressed_buttons: HashSet<mouse_binding::MouseButton>,
    event_time: zx::MonotonicInstant,
    device_descriptor: &input_device::InputDeviceDescriptor,
    handled: input_device::Handled,
) -> input_device::InputEvent {
    input_device::InputEvent {
        device_event: input_device::InputDeviceEvent::Mouse(mouse_binding::MouseEvent::new(
            location,
            wheel_delta_v,
            wheel_delta_h,
            phase,
            affected_buttons,
            pressed_buttons,
            is_precision_scroll,
        )),
        device_descriptor: device_descriptor.clone(),
        event_time,
        handled,
        trace_id: Some(0.into()),
    }
}

/// Creates a [`mouse_binding::MouseEvent`] with the provided parameters.
///
/// # Parameters
/// - `location`: The mouse location to report in the event.
/// - `wheel_delta_v`: The wheel delta in vertical.
/// - `wheel_delta_h`: The wheel delta in horizontal.
/// - `is_precision_scroll`: the wheel event has precision scroll delta.
/// - `phase`: The phase of the buttons in the event.
/// - `buttons`: The buttons to report in the event.
/// - `event_time`: The time of event.
/// - `device_descriptor`: The device descriptor to add to the event.
pub fn create_mouse_event(
    location: mouse_binding::MouseLocation,
    wheel_delta_v: Option<mouse_binding::WheelDelta>,
    wheel_delta_h: Option<mouse_binding::WheelDelta>,
    is_precision_scroll: Option<mouse_binding::PrecisionScroll>,
    phase: mouse_binding::MousePhase,
    affected_buttons: HashSet<mouse_binding::MouseButton>,
    pressed_buttons: HashSet<mouse_binding::MouseButton>,
    event_time: zx::MonotonicInstant,
    device_descriptor: &input_device::InputDeviceDescriptor,
) -> input_device::InputEvent {
    create_mouse_event_with_handled(
        location,
        wheel_delta_v,
        wheel_delta_h,
        is_precision_scroll,
        phase,
        affected_buttons,
        pressed_buttons,
        event_time,
        device_descriptor,
        input_device::Handled::No,
    )
}

/// Creates a [`pointerinjector::Event`] representing a mouse event.
///
/// # Parameters
/// - `phase`: The phase of the touch contact.
/// - `contact`: The touch contact to create the event for.
/// - `position`: The position of the contact in the viewport space.
/// - `relative_motion`: The relative motion fopr the event.
/// - `wheel_delta_v`: The wheel delta in vertical.
/// - `wheel_delta_h`: The wheel delta in horizontal.
/// - `wheel_delta_v_physical_pixel`: The wheel delta in vertical in physical pixel.
/// - `wheel_delta_h_physical_pixel`: The wheel delta in horizontal in physical pixel.
/// - `is_precision_scroll`: the wheel event has precision scroll delta.
/// - `event_time`: The time in nanoseconds when the event was first recorded.
pub fn create_mouse_pointer_sample_event_with_wheel_physical_pixel(
    phase: pointerinjector::EventPhase,
    buttons: Vec<mouse_binding::MouseButton>,
    position: crate::utils::Position,
    relative_motion: Option<[f32; 2]>,
    wheel_delta_v: Option<i64>,
    wheel_delta_h: Option<i64>,
    wheel_delta_v_physical_pixel: Option<f64>,
    wheel_delta_h_physical_pixel: Option<f64>,
    is_precision_scroll: Option<bool>,
    event_time: zx::MonotonicInstant,
) -> pointerinjector::Event {
    let pointer_sample = pointerinjector::PointerSample {
        pointer_id: Some(0),
        phase: Some(phase),
        position_in_viewport: Some([position.x, position.y]),
        scroll_v: wheel_delta_v,
        scroll_h: wheel_delta_h,
        scroll_v_physical_pixel: wheel_delta_v_physical_pixel,
        scroll_h_physical_pixel: wheel_delta_h_physical_pixel,
        is_precision_scroll,
        pressed_buttons: Some(buttons),
        relative_motion,
        ..Default::default()
    };
    let data = pointerinjector::Data::PointerSample(pointer_sample);

    pointerinjector::Event {
        timestamp: Some(event_time.into_nanos()),
        data: Some(data),
        trace_flow_id: Some(0),
        ..Default::default()
    }
}

/// Creates a [`pointerinjector::Event`] representing a mouse event.
///
/// # Parameters
/// - `phase`: The phase of the touch contact.
/// - `contact`: The touch contact to create the event for.
/// - `position`: The position of the contact in the viewport space.
/// - `relative_motion`: The relative motion fopr the event.
/// - `wheel_delta_v`: The wheel delta in vertical.
/// - `wheel_delta_h`: The wheel delta in horizontal.
/// - `is_precision_scroll`: the wheel event has precision scroll delta.
/// - `event_time`: The time in nanoseconds when the event was first recorded.
pub fn create_mouse_pointer_sample_event(
    phase: pointerinjector::EventPhase,
    buttons: Vec<mouse_binding::MouseButton>,
    position: crate::utils::Position,
    relative_motion: Option<[f32; 2]>,
    wheel_delta_v: Option<i64>,
    wheel_delta_h: Option<i64>,
    is_precision_scroll: Option<bool>,
    event_time: zx::MonotonicInstant,
) -> pointerinjector::Event {
    create_mouse_pointer_sample_event_with_wheel_physical_pixel(
        phase,
        buttons,
        position,
        relative_motion,
        wheel_delta_v,
        wheel_delta_h,
        None,
        None,
        is_precision_scroll,
        event_time,
    )
}

// mouse event with phase Add means the injector first seen this mouse.
pub fn create_mouse_pointer_sample_event_phase_add(
    buttons: Vec<mouse_binding::MouseButton>,
    position: crate::utils::Position,
    event_time: zx::MonotonicInstant,
) -> pointerinjector::Event {
    let mut e = create_mouse_pointer_sample_event(
        pointerinjector::EventPhase::Add,
        buttons,
        position,
        None, /*relative_motion*/
        None, /*wheel_delta_v*/
        None, /*wheel_delta_h*/
        None, /*is_precision_scroll*/
        event_time,
    );

    e.trace_flow_id = None;
    e
}

/// Creates a [`fidl_input_report::InputReport`] with a touch report.
///
/// # Parameters
/// - `contacts`: The contacts in the touch report.
/// - `event_time`: The time of event.
pub fn create_touch_input_report(
    contacts: Vec<fidl_input_report::ContactInputReport>,
    pressed_buttons: Option<Vec<fidl_input_report::TouchButton>>,
    event_time: i64,
) -> fidl_input_report::InputReport {
    fidl_input_report::InputReport {
        event_time: Some(event_time),
        keyboard: None,
        mouse: None,
        touch: Some(fidl_input_report::TouchInputReport {
            contacts: Some(contacts),
            pressed_buttons,
            ..Default::default()
        }),
        sensor: None,
        consumer_control: None,
        trace_id: None,
        ..Default::default()
    }
}

pub fn create_touch_contact(id: u32, position: Position) -> touch_binding::TouchContact {
    touch_binding::TouchContact { id, position, pressure: None, contact_size: None }
}

/// Returns an |input_device::InputDeviceDescriptor::TouchScreen|.
pub fn get_touch_screen_device_descriptor() -> input_device::InputDeviceDescriptor {
    input_device::InputDeviceDescriptor::TouchScreen(touch_binding::TouchScreenDeviceDescriptor {
        device_id: 1,
        contacts: vec![touch_binding::ContactDeviceDescriptor {
            x_range: fidl_input_report::Range { min: 0, max: 100 },
            y_range: fidl_input_report::Range { min: 0, max: 100 },
            x_unit: fidl_input_report::Unit {
                type_: fidl_input_report::UnitType::Meters,
                exponent: -6,
            },
            y_unit: fidl_input_report::Unit {
                type_: fidl_input_report::UnitType::Meters,
                exponent: -6,
            },
            pressure_range: None,
            width_range: None,
            height_range: None,
        }],
    })
}

/// Creates a [`touch_binding::TouchScreenEvent`] with the provided parameters.
///
/// # Parameters
/// - `contacts`: The contacts in the touch report.
/// - `event_time`: The time of event.
/// - `device_descriptor`: The device descriptor to add to the event.
/// - `handled`: Whether the event has been consumed.
pub fn create_touch_screen_event_with_handled(
    mut contacts: HashMap<fidl_ui_input::PointerEventPhase, Vec<touch_binding::TouchContact>>,
    event_time: zx::MonotonicInstant,
    device_descriptor: &input_device::InputDeviceDescriptor,
    handled: input_device::Handled,
) -> input_device::InputEvent {
    contacts.entry(fidl_ui_input::PointerEventPhase::Add).or_insert(vec![]);
    contacts.entry(fidl_ui_input::PointerEventPhase::Down).or_insert(vec![]);
    contacts.entry(fidl_ui_input::PointerEventPhase::Move).or_insert(vec![]);
    contacts.entry(fidl_ui_input::PointerEventPhase::Up).or_insert(vec![]);
    contacts.entry(fidl_ui_input::PointerEventPhase::Remove).or_insert(vec![]);

    let injector_contacts = hashmap! {
        pointerinjector::EventPhase::Add =>
        contacts.get(&fidl_ui_input::PointerEventPhase::Add).unwrap().clone(),
        pointerinjector::EventPhase::Change =>
        contacts.get(&fidl_ui_input::PointerEventPhase::Move).unwrap().clone(),
        pointerinjector::EventPhase::Remove =>
        contacts.get(&fidl_ui_input::PointerEventPhase::Remove).unwrap().clone(),
    };
    input_device::InputEvent {
        device_event: input_device::InputDeviceEvent::TouchScreen(
            touch_binding::TouchScreenEvent { contacts, injector_contacts },
        ),
        device_descriptor: device_descriptor.clone(),
        event_time,
        handled: handled,
        trace_id: None,
    }
}

/// Creates a [`touch_binding::TouchScreenEvent`] with the provided parameters.
///
/// # Parameters
/// - `contacts`: The contacts in the touch report.
/// - `event_time`: The time of event.
/// - `device_descriptor`: The device descriptor to add to the event.
pub fn create_touch_screen_event(
    contacts: HashMap<fidl_ui_input::PointerEventPhase, Vec<touch_binding::TouchContact>>,
    event_time: zx::MonotonicInstant,
    device_descriptor: &input_device::InputDeviceDescriptor,
) -> input_device::InputEvent {
    create_touch_screen_event_with_handled(
        contacts,
        event_time,
        device_descriptor,
        input_device::Handled::No,
    )
}

/// Creates a [`touch_binding::TouchpadEvent`] with the provided parameters.
///
/// # Parameters
/// - `injector_contacts`: The contacts in the touch report.
/// - `pressed_buttons`: The buttons to report in the event.
/// - `event_time`: The time of event.
/// - `device_descriptor`: The device descriptor to add to the event.
/// - `handled`: Whether the event has been consumed.
pub fn create_touchpad_event_with_handled(
    injector_contacts: Vec<touch_binding::TouchContact>,
    pressed_buttons: HashSet<fidl_input_report::TouchButton>,
    event_time: zx::MonotonicInstant,
    device_descriptor: &input_device::InputDeviceDescriptor,
    handled: input_device::Handled,
) -> input_device::InputEvent {
    let buttons: HashSet<mouse_binding::MouseButton> =
        HashSet::from_iter(pressed_buttons.iter().filter_map(|button| match button {
            fidl_fuchsia_input_report::TouchButton::Palm => Some(1),
            _ => None,
        }));

    input_device::InputEvent {
        device_event: input_device::InputDeviceEvent::Touchpad(touch_binding::TouchpadEvent {
            injector_contacts,
            pressed_buttons: buttons,
        }),
        device_descriptor: device_descriptor.clone(),
        event_time,
        handled: handled,
        trace_id: None,
    }
}

/// Creates a [`touch_binding::TouchpadEvent`] with the provided parameters.
///
/// # Parameters
/// - `contacts`: The contacts in the touch report.
/// - `pressed_buttons`: The buttons to report in the event.
/// - `event_time`: The time of event.
/// - `device_descriptor`: The device descriptor to add to the event.
pub fn create_touchpad_event(
    contacts: Vec<touch_binding::TouchContact>,
    pressed_buttons: HashSet<fidl_input_report::TouchButton>,
    event_time: zx::MonotonicInstant,
    device_descriptor: &input_device::InputDeviceDescriptor,
) -> input_device::InputEvent {
    create_touchpad_event_with_handled(
        contacts,
        pressed_buttons,
        event_time,
        device_descriptor,
        input_device::Handled::No,
    )
}

/// Creates a [`fidl_ui_scenic::Command`] representing the given touch contact.
///
/// # Parameters
/// - `phase`: The phase of the touch contact.
/// - `contact`: The touch contact to create the event for.
/// - `position`: The position of the contact in the viewport space.
/// - `event_time`: The time in nanoseconds when the event was first recorded.
pub fn create_touch_pointer_sample_event(
    phase: pointerinjector::EventPhase,
    contact: &touch_binding::TouchContact,
    position: crate::utils::Position,
    event_time: zx::MonotonicInstant,
) -> pointerinjector::Event {
    let pointer_sample = pointerinjector::PointerSample {
        pointer_id: Some(contact.id),
        phase: Some(phase),
        position_in_viewport: Some([position.x, position.y]),
        scroll_v: None,
        scroll_h: None,
        pressed_buttons: None,
        ..Default::default()
    };
    let data = pointerinjector::Data::PointerSample(pointer_sample);

    pointerinjector::Event {
        timestamp: Some(event_time.into_nanos()),
        data: Some(data),
        trace_flow_id: Some(fuchsia_trace::Id::new().into()),
        ..Default::default()
    }
}

/// Asserts that the given sequence of input reports generates the provided input events
/// when the reports are processed by the given device type.
#[macro_export]
macro_rules! assert_input_report_sequence_generates_events {
    (
        // The input reports to process.
        input_reports: $input_reports:expr,
        // The events which are expected.
        expected_events: $expected_events:expr,
        // The descriptor for the device that is sent to the input processor.
        device_descriptor: $device_descriptor:expr,
        // The type of device generating the events.
        device_type: $DeviceType:ty,
    ) => {
        let mut previous_report: Option<fidl_fuchsia_input_report::InputReport> = None;
        let num_reports = $input_reports.len();
        let num_events = $expected_events.len();
        let (event_sender, mut event_receiver) = futures::channel::mpsc::unbounded();

        // Create fake inspect_status, needed for process_reports()
        let inspector = fuchsia_inspect::Inspector::default();
        let test_node = inspector.root().create_child("TestDevice");
        let mut inspect_status = InputDeviceStatus::new(test_node);
        inspect_status.health_node.set_ok();

        let mut expected_last_received_timestamp = 0u64;
        let mut expected_last_generated_timestamp = 0u64;

        // Send all the reports prior to verifying the received events.
        for report in $input_reports {
            if let Some(report_time) = report.event_time {
                expected_last_received_timestamp = report_time.try_into().unwrap();
            }
            let inspect_receiver: Option<UnboundedReceiver<InputEvent>>;
            (previous_report, inspect_receiver) = <$DeviceType>::process_reports(
                report,
                previous_report,
                &$device_descriptor,
                &mut event_sender.clone(),
                &inspect_status,
                &metrics::MetricsLogger::default(),
            );

            // If a report generates multiple events asynchronously, we send them over a mpsc channel
            // to inspect_receiver. We update the event count on inspect_status here since we cannot
            // pass a reference to inspect_status to an async task in process_reports().
            match inspect_receiver {
                Some(mut receiver) => {
                    while let Some(event) = receiver.next().await {
                        inspect_status.count_generated_event(event);
                    }
                },
                None => (),
            };
        }

        for mut expected_event in $expected_events {
            let input_event = event_receiver.next().await;
            match input_event {
                Some(received_event) => {
                    // Overwrite the expected_event's event_time, because an InputEvent's event_time
                    // is set at the time InputReports are processed into InputEvents.
                    expected_event.event_time = received_event.event_time;
                    expected_last_generated_timestamp = received_event.event_time.into_nanos().try_into().unwrap();
                    expected_event.trace_id = received_event.trace_id;
                    pretty_assertions::assert_eq!(expected_event, received_event)
                }
                _ => assert!(false),
            };
        }

        $crate::testing_utilities::diagnostics_assertions::assert_data_tree!(inspector, root: {
            "TestDevice": contains {
                reports_received_count: num_reports as u64,
                reports_filtered_count: $crate::testing_utilities::diagnostics_assertions::AnyProperty,
                events_generated: num_events as u64,
                last_received_timestamp_ns: expected_last_received_timestamp,
                last_generated_timestamp_ns: expected_last_generated_timestamp,
            }
        });
    };
}

/// Asserts that the given sequence of input events generates the provided Scenic commands when the
/// events are processed by the given input handler.
#[macro_export]
macro_rules! assert_input_event_sequence_generates_scenic_events {
    (
        // The input handler that will handle input events.
        input_handler: $input_handler:expr,
        // The InputEvents to handle.
        input_events: $input_events:expr,
        // The commands the Scenic session should receive.
        expected_commands: $expected_commands:expr,
        // The Scenic session request stream.
        scenic_session_request_stream: $scenic_session_request_stream:expr,
        // A function to validate the Scenic commands.
        assert_command: $assert_command:expr,
    ) => {
        for input_event in $input_events {
            assert_matches!(
                $input_handler.clone().handle_input_event(input_event).await.as_slice(),
                [input_device::InputEvent { handled: input_device::Handled::Yes, .. }]
            );
        }

        let mut expected_command_iter = $expected_commands.into_iter().peekable();
        while let Some(request) = $scenic_session_request_stream.next().await {
            match request {
                Ok(fidl_ui_scenic::SessionRequest::Enqueue { cmds, control_handle: _ }) => {
                    let mut command_iter = cmds.into_iter().peekable();
                    while let Some(command) = command_iter.next() {
                        let expected_command = expected_command_iter.next().unwrap();
                        $assert_command(command, expected_command);

                        // All the expected events have been received, so make sure no more events
                        // are present before returning.
                        if expected_command_iter.peek().is_none() {
                            assert!(command_iter.peek().is_none());
                            return;
                        }
                    }
                }
                _ => {
                    assert!(false);
                }
            }
        }
    };
}

/// Asserts that the given sequence of input events generates the provided media buttons events when
/// the input events are processed by the given input handler.
#[macro_export]
macro_rules! assert_input_event_sequence_generates_media_buttons_events {
    (
        // The input handler that will handle input events.
        input_handler: $input_handler:expr,
        // The InputEvents to handle.
        input_events: $input_events:expr,
        // The events the listeners should receive.
        expected_events: $expected_events:expr,
        // The media buttons listener request stream(s).
        media_buttons_listener_request_stream: $media_buttons_listener_request_stream:expr,
    ) => {
        fasync::Task::local(async move {
            for input_event in $input_events {
                assert_matches!(
                    $input_handler.clone().handle_input_event(input_event).await.as_slice(),
                    [input_device::InputEvent { handled: input_device::Handled::Yes, .. }]
                );
            }
        })
        .detach();

        for mut stream in $media_buttons_listener_request_stream {
            let mut expected_command_iter = $expected_events.clone().into_iter().peekable();
            while let Some(request) = stream.next().await {
                match request {
                    Ok(fidl_ui_policy::MediaButtonsListenerRequest::OnEvent {
                        event,
                        responder,
                    }) => {
                        let expected_command = expected_command_iter.next().unwrap();
                        pretty_assertions::assert_eq!(event, expected_command);
                        let _ = responder.send();

                        // All the expected events have been received, so make sure no more
                        // events are present before continuing to the next stream.
                        if expected_command_iter.peek().is_none() {
                            break;
                        }
                    }
                    _ => assert!(false),
                }
            }
        }
    };
}

/// Asserts that the given sequence of input events are ignored by the provided handler and request stream.
pub async fn assert_handler_ignores_input_event_sequence(
    // The handler processing events.
    input_handler: std::rc::Rc<dyn input_handler::InputHandler>,
    // The InputEvents to handle.
    input_events: Vec<input_device::InputEvent>,
    // The listener request stream.
    mut injector_request_stream: impl futures::StreamExt + std::marker::Unpin,
) {
    for input_event in input_events {
        assert_matches!(
            input_handler.clone().handle_input_event(input_event).await.as_slice(),
            [input_device::InputEvent { handled: input_device::Handled::Yes, .. }]
        );
    }

    // Request streams should not receive any events.
    assert!(injector_request_stream.next().now_or_never().is_none());
}
