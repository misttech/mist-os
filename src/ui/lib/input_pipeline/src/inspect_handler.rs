// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::input_device::{Handled, InputDeviceEvent, InputDeviceType, InputEvent};
use crate::input_handler::InputHandler;
use async_trait::async_trait;
use fuchsia_inspect::{
    self as inspect, ExponentialHistogramParams, HistogramProperty, Inspector, NumericProperty,
    Property,
};
use fuchsia_zircon as zx;
use futures::lock::Mutex;
use futures::FutureExt;
use std::cell::RefCell;
use std::collections::{HashMap, HashSet, VecDeque};
use std::rc::Rc;
use std::sync::Arc;

const MAX_RECENT_EVENT_LOG_SIZE: usize = 125;
const LATENCY_HISTOGRAM_PROPERTIES: ExponentialHistogramParams<i64> = ExponentialHistogramParams {
    floor: 0,
    initial_step: 1,
    step_multiplier: 10,
    // Seven buckets allows us to report
    // *      < 0 msec (added automatically by Inspect)
    // *      0-1 msec
    // *     1-10 msec
    // *   10-100 msec
    // * 100-1000 msec
    // *     1-10 sec
    // *   10-100 sec
    // * 100-1000 sec
    // *    >1000 sec (added automatically by Inspect)
    buckets: 7,
};

#[derive(Debug, Hash, PartialEq, Eq)]
enum EventType {
    Keyboard,
    LightSensor,
    ConsumerControls,
    Mouse,
    TouchScreen,
    Touchpad,
    #[cfg(test)]
    Fake,
}

impl std::fmt::Display for EventType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &*self {
            EventType::Keyboard => write!(f, "keyboard"),
            EventType::LightSensor => write!(f, "light_sensor"),
            EventType::ConsumerControls => write!(f, "consumer_controls"),
            EventType::Mouse => write!(f, "mouse"),
            EventType::TouchScreen => write!(f, "touch_screen"),
            EventType::Touchpad => write!(f, "touchpad"),
            #[cfg(test)]
            EventType::Fake => write!(f, "fake"),
        }
    }
}

impl EventType {
    /// Creates an `EventType` based on an [InputDeviceEvent].
    pub fn for_device_event(event: &InputDeviceEvent) -> Self {
        match event {
            InputDeviceEvent::Keyboard(_) => EventType::Keyboard,
            InputDeviceEvent::LightSensor(_) => EventType::LightSensor,
            InputDeviceEvent::ConsumerControls(_) => EventType::ConsumerControls,
            InputDeviceEvent::Mouse(_) => EventType::Mouse,
            InputDeviceEvent::TouchScreen(_) => EventType::TouchScreen,
            InputDeviceEvent::Touchpad(_) => EventType::Touchpad,
            #[cfg(test)]
            InputDeviceEvent::Fake => EventType::Fake,
        }
    }
}

#[derive(Debug)]
struct EventCounters {
    /// A node that contains the counters below.
    _node: inspect::Node,
    /// The number of total events that this handler has seen so far.
    events_count: inspect::UintProperty,
    /// The number of total handled events that this handler has seen so far.
    handled_events_count: inspect::UintProperty,
    /// The timestamp (in nanoseconds) when the last event was seen by this
    /// handler (not when the event itself was generated). 0 if unset.
    last_seen_timestamp_ns: inspect::IntProperty,
    /// The event time at which the last recorded event was generated.
    /// 0 if unset.
    last_generated_timestamp_ns: inspect::IntProperty,
}

impl EventCounters {
    fn add_new_into(
        map: &mut HashMap<EventType, EventCounters>,
        root: &inspect::Node,
        event_type: EventType,
    ) {
        let node = root.create_child(format!("{}", event_type));
        let events_count = node.create_uint("events_count", 0);
        let handled_events_count = node.create_uint("handled_events_count", 0);
        let last_seen_timestamp_ns = node.create_int("last_seen_timestamp_ns", 0);
        let last_generated_timestamp_ns = node.create_int("last_generated_timestamp_ns", 0);
        let new_counters = EventCounters {
            _node: node,
            events_count,
            handled_events_count,
            last_seen_timestamp_ns,
            last_generated_timestamp_ns,
        };
        map.insert(event_type, new_counters);
    }

    pub fn count_event(&self, time: zx::Time, event_time: zx::Time, handled: &Handled) {
        self.events_count.add(1);
        if *handled == Handled::Yes {
            self.handled_events_count.add(1);
        }
        self.last_seen_timestamp_ns.set(time.into_nanos());
        self.last_generated_timestamp_ns.set(event_time.into_nanos());
    }
}

struct CircularBuffer {
    // Size of CircularBuffer
    _size: usize,
    // VecDeque of recent events with capacity of `size`
    _events: VecDeque<InputEvent>,
}

impl CircularBuffer {
    fn new(size: usize) -> Self {
        let events = VecDeque::with_capacity(size);
        CircularBuffer { _size: size, _events: events }
    }

    fn push(&mut self, event: InputEvent) {
        if self._events.len() >= self._size {
            std::mem::drop(self._events.pop_front());
        }
        self._events.push_back(event);
    }

    fn record_all_lazy_inspect(&self, inspector: inspect::Inspector) -> inspect::Inspector {
        self._events.iter().enumerate().for_each(|(i, &ref event)| {
            let event_clone = event.clone();

            // Include leading zeros so Inspect will display events in correct numerical order.
            // Inspect displays nodes in alphabetical order by default.
            let mut prefix = "";
            if i < 10 {
                prefix = "00";
            } else if i < 100 {
                prefix = "0";
            }

            inspector.root().record_child(
                format!("{}{}_{}", prefix, i, event_clone.get_event_type()),
                move |node| event_clone.record_inspect(node),
            );
        });
        inspector
    }
}

/// A [InputHandler] that records various metrics about the flow of events.
/// All events are passed through unmodified.  Some properties of those events
/// may be exposed in the metrics.  No PII information should ever be exposed
/// this way.
#[derive(Debug)]
pub struct InspectHandler<F> {
    /// A function that obtains the current timestamp.
    now: RefCell<F>,
    /// A node that contains the statistics about this particular handler.
    _node: inspect::Node,
    /// The number of total events that this handler has seen so far.
    events_count: inspect::UintProperty,
    /// The timestamp (in nanoseconds) when the last event was seen by this
    /// handler (not when the event itself was generated). 0 if unset.
    last_seen_timestamp_ns: inspect::IntProperty,
    /// The event time at which the last recorded event was generated.
    /// 0 if unset.
    last_generated_timestamp_ns: inspect::IntProperty,
    /// An inventory of event counters by type.
    events_by_type: HashMap<EventType, EventCounters>,
    /// Log of recent events in the order they were received.
    recent_events_log: Option<Arc<Mutex<CircularBuffer>>>,
    /// Histogram of latency from the binding timestamp for an `InputEvent` until
    /// the time the `InputEvent` was observed by this handler. Reported in milliseconds,
    /// because values less than 1 msec aren't especially interesting.
    pipeline_latency_ms: inspect::IntExponentialHistogramProperty,
}

#[async_trait(?Send)]
impl<F: FnMut() -> zx::Time + 'static> InputHandler for InspectHandler<F> {
    async fn handle_input_event(self: Rc<Self>, input_event: InputEvent) -> Vec<InputEvent> {
        let event_time = input_event.event_time;
        let now = (self.now.borrow_mut())();
        self.events_count.add(1);
        self.last_seen_timestamp_ns.set(now.into_nanos());
        self.last_generated_timestamp_ns.set(event_time.into_nanos());
        let event_type = EventType::for_device_event(&input_event.device_event);
        self.events_by_type
            .get(&event_type)
            .unwrap_or_else(|| panic!("no event counters for {}", event_type))
            .count_event(now, event_time, &input_event.handled);
        if let Some(recent_events_log) = &self.recent_events_log {
            recent_events_log.lock().await.push(input_event.clone());
        }
        self.pipeline_latency_ms.insert((now - event_time).into_millis());
        vec![input_event]
    }
}

/// Creates a new inspect handler instance.
///
/// `node` is the inspect node that will receive the stats.
pub fn make_inspect_handler(
    node: inspect::Node,
    supported_input_devices: &HashSet<&InputDeviceType>,
    displays_recent_events: bool,
) -> Rc<InspectHandler<fn() -> zx::Time>> {
    InspectHandler::new_internal(
        node,
        zx::Time::get_monotonic,
        supported_input_devices,
        displays_recent_events,
    )
}

impl<F> InspectHandler<F> {
    /// Creates a new inspect handler instance, using `now` to supply the current timestamp.
    /// Expected to be useful in testing mainly.
    fn new_internal(
        node: inspect::Node,
        now: F,
        supported_input_devices: &HashSet<&InputDeviceType>,
        displays_recent_events: bool,
    ) -> Rc<Self> {
        let event_count = node.create_uint("events_count", 0);
        let last_seen_timestamp_ns = node.create_int("last_seen_timestamp_ns", 0);
        let last_generated_timestamp_ns = node.create_int("last_generated_timestamp_ns", 0);

        let recent_events_log = match displays_recent_events {
            true => {
                let recent_events =
                    Arc::new(Mutex::new(CircularBuffer::new(MAX_RECENT_EVENT_LOG_SIZE)));
                record_lazy_recent_events(&node, Arc::clone(&recent_events));
                Some(recent_events)
            }
            false => None,
        };

        let pipeline_latency_ms = node
            .create_int_exponential_histogram("pipeline_latency_ms", LATENCY_HISTOGRAM_PROPERTIES);

        let mut events_by_type = HashMap::new();
        if supported_input_devices.contains(&InputDeviceType::Keyboard) {
            EventCounters::add_new_into(&mut events_by_type, &node, EventType::Keyboard);
        }
        if supported_input_devices.contains(&InputDeviceType::ConsumerControls) {
            EventCounters::add_new_into(&mut events_by_type, &node, EventType::ConsumerControls);
        }
        if supported_input_devices.contains(&InputDeviceType::LightSensor) {
            EventCounters::add_new_into(&mut events_by_type, &node, EventType::LightSensor);
        }
        if supported_input_devices.contains(&InputDeviceType::Mouse) {
            EventCounters::add_new_into(&mut events_by_type, &node, EventType::Mouse);
        }
        if supported_input_devices.contains(&InputDeviceType::Touch) {
            EventCounters::add_new_into(&mut events_by_type, &node, EventType::TouchScreen);
            EventCounters::add_new_into(&mut events_by_type, &node, EventType::Touchpad);
        }
        #[cfg(test)]
        EventCounters::add_new_into(&mut events_by_type, &node, EventType::Fake);

        Rc::new(Self {
            now: RefCell::new(now),
            _node: node,
            events_count: event_count,
            last_seen_timestamp_ns,
            last_generated_timestamp_ns,
            events_by_type,
            recent_events_log,
            pipeline_latency_ms,
        })
    }
}

fn record_lazy_recent_events(node: &inspect::Node, recent_events: Arc<Mutex<CircularBuffer>>) {
    node.record_lazy_child("recent_events_log", move || {
        let recent_events_clone = Arc::clone(&recent_events);
        async move {
            let inspector = Inspector::default();
            Ok(recent_events_clone.lock().await.record_all_lazy_inspect(inspector))
        }
        .boxed()
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        input_device::{self, InputDeviceDescriptor, InputDeviceEvent, InputEvent},
        keyboard_binding::KeyboardDeviceDescriptor,
        light_sensor::types::Rgbc,
        light_sensor_binding::{LightSensorDeviceDescriptor, LightSensorEvent},
        mouse_binding::{
            MouseDeviceDescriptor, MouseLocation, MousePhase, PrecisionScroll, RawWheelDelta,
            WheelDelta,
        },
        testing_utilities::{
            consumer_controls_device_descriptor, create_consumer_controls_event,
            create_fake_handled_input_event, create_fake_input_event, create_keyboard_event,
            create_mouse_event, create_touch_contact, create_touch_screen_event,
            create_touchpad_event,
        },
        touch_binding::{TouchScreenDeviceDescriptor, TouchpadDeviceDescriptor},
        utils::Position,
    };
    use fidl::endpoints::create_proxy_and_stream;
    use fidl_fuchsia_input_report::InputDeviceMarker;
    use fuchsia_async as fasync;
    use fuchsia_inspect::{assert_data_tree, AnyProperty};
    use maplit::{hashmap, hashset};
    use std::collections::HashSet;
    use test_case::test_case;

    fn fixed_now() -> zx::Time {
        zx::Time::ZERO + zx::Duration::from_nanos(42)
    }

    #[fasync::run_singlethreaded(test)]
    async fn circular_buffer_no_overflow() {
        let mut circular_buffer = CircularBuffer::new(MAX_RECENT_EVENT_LOG_SIZE);
        assert_eq!(circular_buffer._size, MAX_RECENT_EVENT_LOG_SIZE);

        let first_event_time = zx::Time::get_monotonic();
        circular_buffer.push(create_fake_input_event(first_event_time));
        let second_event_time = zx::Time::get_monotonic();
        circular_buffer.push(create_fake_input_event(second_event_time));

        // Fill up `events` VecDeque
        for _i in 2..MAX_RECENT_EVENT_LOG_SIZE {
            let curr_event_time = zx::Time::get_monotonic();
            circular_buffer.push(create_fake_input_event(curr_event_time));
            match circular_buffer._events.back() {
                Some(event) => assert_eq!(event.event_time, curr_event_time),
                None => assert!(false),
            }
        }

        // Verify first event at the front
        match circular_buffer._events.front() {
            Some(event) => assert_eq!(event.event_time, first_event_time),
            None => assert!(false),
        }

        // CircularBuffer `events` should be full, pushing another event should remove the first event.
        let last_event_time = zx::Time::get_monotonic();
        circular_buffer.push(create_fake_input_event(last_event_time));
        match circular_buffer._events.front() {
            Some(event) => assert_eq!(event.event_time, second_event_time),
            None => assert!(false),
        }
        match circular_buffer._events.back() {
            Some(event) => assert_eq!(event.event_time, last_event_time),
            None => assert!(false),
        }
    }

    #[fasync::run_singlethreaded(test)]
    async fn recent_events_log_records_inspect() {
        let inspector = fuchsia_inspect::Inspector::default();

        let recent_events_log =
            Arc::new(Mutex::new(CircularBuffer::new(MAX_RECENT_EVENT_LOG_SIZE)));
        record_lazy_recent_events(inspector.root(), Arc::clone(&recent_events_log));

        let keyboard_descriptor = InputDeviceDescriptor::Keyboard(KeyboardDeviceDescriptor {
            keys: vec![fidl_fuchsia_input::Key::A, fidl_fuchsia_input::Key::B],
            ..Default::default()
        });
        let mouse_descriptor = InputDeviceDescriptor::Mouse(MouseDeviceDescriptor {
            device_id: 1u32,
            absolute_x_range: None,
            absolute_y_range: None,
            wheel_v_range: None,
            wheel_h_range: None,
            buttons: None,
            counts_per_mm: 12u32,
        });
        let touch_screen_descriptor =
            InputDeviceDescriptor::TouchScreen(TouchScreenDeviceDescriptor {
                device_id: 1,
                contacts: vec![],
            });
        let touchpad_descriptor = InputDeviceDescriptor::Touchpad(TouchpadDeviceDescriptor {
            device_id: 1,
            contacts: vec![],
        });

        let pressed_buttons = HashSet::from([1u8, 21u8, 15u8]);
        let mut pressed_buttons_vec: Vec<u64> = vec![];
        pressed_buttons.iter().for_each(|button| {
            pressed_buttons_vec.push(*button as u64);
        });

        let (light_sensor_proxy, _) =
            create_proxy_and_stream::<InputDeviceMarker>().expect("proxy created");

        let recent_events = vec![
            create_keyboard_event(
                fidl_fuchsia_input::Key::A,
                fidl_fuchsia_ui_input3::KeyEventType::Pressed,
                None,
                &keyboard_descriptor,
                None,
            ),
            create_consumer_controls_event(
                vec![
                    fidl_fuchsia_input_report::ConsumerControlButton::VolumeUp,
                    fidl_fuchsia_input_report::ConsumerControlButton::VolumeUp,
                    fidl_fuchsia_input_report::ConsumerControlButton::Pause,
                    fidl_fuchsia_input_report::ConsumerControlButton::VolumeDown,
                    fidl_fuchsia_input_report::ConsumerControlButton::MicMute,
                    fidl_fuchsia_input_report::ConsumerControlButton::CameraDisable,
                    fidl_fuchsia_input_report::ConsumerControlButton::FactoryReset,
                    fidl_fuchsia_input_report::ConsumerControlButton::Reboot,
                ],
                zx::Time::get_monotonic(),
                &consumer_controls_device_descriptor(),
            ),
            create_mouse_event(
                MouseLocation::Absolute(Position { x: 7.0f32, y: 15.0f32 }),
                Some(WheelDelta {
                    raw_data: RawWheelDelta::Ticks(5i64),
                    physical_pixel: Some(8.0f32),
                }),
                Some(WheelDelta {
                    raw_data: RawWheelDelta::Millimeters(10.0f32),
                    physical_pixel: Some(8.0f32),
                }),
                Some(PrecisionScroll::Yes),
                MousePhase::Move,
                HashSet::from([1u8]),
                pressed_buttons.clone(),
                zx::Time::get_monotonic(),
                &mouse_descriptor,
            ),
            create_touch_screen_event(
                hashmap! {
                    fidl_fuchsia_ui_input::PointerEventPhase::Add
                        => vec![create_touch_contact(1u32, Position { x: 10.0, y: 30.0 })],
                    fidl_fuchsia_ui_input::PointerEventPhase::Move
                        => vec![create_touch_contact(1u32, Position { x: 11.0, y: 31.0 })],
                },
                zx::Time::get_monotonic(),
                &touch_screen_descriptor,
            ),
            create_touchpad_event(
                vec![
                    create_touch_contact(1u32, Position { x: 0.0, y: 0.0 }),
                    create_touch_contact(2u32, Position { x: 10.0, y: 10.0 }),
                ],
                pressed_buttons,
                zx::Time::get_monotonic(),
                &touchpad_descriptor,
            ),
            InputEvent {
                device_event: InputDeviceEvent::LightSensor(LightSensorEvent {
                    device_proxy: light_sensor_proxy,
                    rgbc: Rgbc { red: 1, green: 2, blue: 3, clear: 14747 },
                }),
                device_descriptor: InputDeviceDescriptor::LightSensor(
                    LightSensorDeviceDescriptor {
                        vendor_id: 1,
                        product_id: 2,
                        device_id: 3,
                        sensor_layout: Rgbc { red: 1, green: 2, blue: 3, clear: 4 },
                    },
                ),
                event_time: zx::Time::get_monotonic(),
                handled: input_device::Handled::No,
                trace_id: None,
            },
            create_keyboard_event(
                fidl_fuchsia_input::Key::B,
                fidl_fuchsia_ui_input3::KeyEventType::Pressed,
                None,
                &keyboard_descriptor,
                None,
            ),
        ];

        for event in recent_events.into_iter() {
            recent_events_log.lock().await.push(event);
        }

        assert_data_tree!(inspector, root: {
            recent_events_log: {
                "000_keyboard_event": {
                    event_time: AnyProperty,
                },
                "001_consumer_controls_event": {
                    event_time: AnyProperty,
                    pressed_buttons: vec!["volume_up", "volume_up", "pause", "volume_down", "mic_mute", "camera_disable", "factory_reset", "reboot"],
                },
                "002_mouse_event": {
                    event_time: AnyProperty,
                    location_absolute: { x: 7.0f64, y: 15.0f64},
                    wheel_delta_v: {
                        ticks: 5i64,
                        physical_pixel: 8.0f64,
                    },
                    wheel_delta_h: {
                        millimeters: 10.0f64,
                        physical_pixel: 8.0f64,
                    },
                    is_precision_scroll: "yes",
                    phase: "move",
                    affected_buttons: vec![1u64],
                    pressed_buttons: pressed_buttons_vec.clone(),
                },
                "003_touch_screen_event": {
                    event_time: AnyProperty,
                    injector_contacts: {
                        add: {
                            "1": {
                                position_x_mm: 10.0f64,
                                position_y_mm: 30.0f64,
                            },
                        },
                        change: {
                            "1": {
                                position_x_mm: 11.0f64,
                                position_y_mm: 31.0f64,
                            },
                        },
                        remove: {},
                    },
                },
                "004_touchpad_event": {
                    event_time: AnyProperty,
                    pressed_buttons: pressed_buttons_vec,
                    injector_contacts: {
                        "1": {
                            position_x_mm: 0.0f64,
                            position_y_mm: 0.0f64,
                        },
                        "2": {
                            position_x_mm: 10.0f64,
                            position_y_mm: 10.0f64,
                        },
                    },
                },
                "005_light_sensor_event": {
                    event_time: AnyProperty,
                    red: 1u64,
                    green: 2u64,
                    blue: 3u64,
                    clear: 14747u64,
                },
                "006_keyboard_event": {
                    event_time: AnyProperty,
                },
            }
        });
    }

    #[fasync::run_singlethreaded(test)]
    async fn verify_inspect_no_recent_events_log() {
        let inspector = inspect::Inspector::default();
        let root = inspector.root();
        let test_node = root.create_child("test_node");
        let supported_input_devices: HashSet<&InputDeviceType> = HashSet::from([
            &input_device::InputDeviceType::Keyboard,
            &input_device::InputDeviceType::ConsumerControls,
            &input_device::InputDeviceType::LightSensor,
            &input_device::InputDeviceType::Mouse,
            &input_device::InputDeviceType::Touch,
        ]);

        let handler = super::InspectHandler::new_internal(
            test_node,
            fixed_now,
            &supported_input_devices,
            /* displays_recent_events = */ false,
        );
        assert_data_tree!(inspector, root: {
            test_node: contains {
                events_count: 0u64,
                last_seen_timestamp_ns: 0i64,
                last_generated_timestamp_ns: 0i64,
                consumer_controls: {
                     events_count: 0u64,
                     handled_events_count: 0u64,
                     last_generated_timestamp_ns: 0i64,
                     last_seen_timestamp_ns: 0i64,
                },
                fake: {
                     events_count: 0u64,
                     handled_events_count: 0u64,
                     last_generated_timestamp_ns: 0i64,
                     last_seen_timestamp_ns: 0i64,
                },
                keyboard: {
                     events_count: 0u64,
                     handled_events_count: 0u64,
                     last_generated_timestamp_ns: 0i64,
                     last_seen_timestamp_ns: 0i64,
                },
                light_sensor: {
                     events_count: 0u64,
                     handled_events_count: 0u64,
                     last_generated_timestamp_ns: 0i64,
                     last_seen_timestamp_ns: 0i64,
                },
                mouse: {
                     events_count: 0u64,
                     handled_events_count: 0u64,
                     last_generated_timestamp_ns: 0i64,
                     last_seen_timestamp_ns: 0i64,
                },
                touch_screen: {
                     events_count: 0u64,
                     handled_events_count: 0u64,
                     last_generated_timestamp_ns: 0i64,
                     last_seen_timestamp_ns: 0i64,
                },
                touchpad: {
                    events_count: 0u64,
                    handled_events_count: 0u64,
                    last_generated_timestamp_ns: 0i64,
                    last_seen_timestamp_ns: 0i64,
               },
           }
        });

        handler
            .clone()
            .handle_input_event(create_fake_input_event(zx::Time::from_nanos(43i64)))
            .await;
        assert_data_tree!(inspector, root: {
            test_node: contains {
                events_count: 1u64,
                last_seen_timestamp_ns: 42i64,
                last_generated_timestamp_ns: 43i64,
                consumer_controls: {
                     events_count: 0u64,
                     handled_events_count: 0u64,
                     last_generated_timestamp_ns: 0i64,
                     last_seen_timestamp_ns: 0i64,
                },
                fake: {
                     events_count: 1u64,
                     handled_events_count: 0u64,
                     last_generated_timestamp_ns: 43i64,
                     last_seen_timestamp_ns: 42i64,
                },
                keyboard: {
                     events_count: 0u64,
                     handled_events_count: 0u64,
                     last_generated_timestamp_ns: 0i64,
                     last_seen_timestamp_ns: 0i64,
                },
                light_sensor: {
                     events_count: 0u64,
                     handled_events_count: 0u64,
                     last_generated_timestamp_ns: 0i64,
                     last_seen_timestamp_ns: 0i64,
                },
                mouse: {
                     events_count: 0u64,
                     handled_events_count: 0u64,
                     last_generated_timestamp_ns: 0i64,
                     last_seen_timestamp_ns: 0i64,
                },
                touch_screen: {
                     events_count: 0u64,
                     handled_events_count: 0u64,
                     last_generated_timestamp_ns: 0i64,
                     last_seen_timestamp_ns: 0i64,
                },
                touchpad: {
                    events_count: 0u64,
                    handled_events_count: 0u64,
                    last_generated_timestamp_ns: 0i64,
                    last_seen_timestamp_ns: 0i64,
               },
            }
        });

        handler
            .clone()
            .handle_input_event(create_fake_input_event(zx::Time::from_nanos(44i64)))
            .await;
        assert_data_tree!(inspector, root: {
            test_node: contains {
                events_count: 2u64,
                last_seen_timestamp_ns: 42i64,
                last_generated_timestamp_ns: 44i64,
                consumer_controls: {
                     events_count: 0u64,
                     handled_events_count: 0u64,
                     last_generated_timestamp_ns: 0i64,
                     last_seen_timestamp_ns: 0i64,
                },
                fake: {
                     events_count: 2u64,
                     handled_events_count: 0u64,
                     last_generated_timestamp_ns: 44i64,
                     last_seen_timestamp_ns: 42i64,
                },
                keyboard: {
                     events_count: 0u64,
                     handled_events_count: 0u64,
                     last_generated_timestamp_ns: 0i64,
                     last_seen_timestamp_ns: 0i64,
                },
                light_sensor: {
                     events_count: 0u64,
                     handled_events_count: 0u64,
                     last_generated_timestamp_ns: 0i64,
                     last_seen_timestamp_ns: 0i64,
                },
                mouse: {
                     events_count: 0u64,
                     handled_events_count: 0u64,
                     last_generated_timestamp_ns: 0i64,
                     last_seen_timestamp_ns: 0i64,
                },
                touch_screen: {
                     events_count: 0u64,
                     handled_events_count: 0u64,
                     last_generated_timestamp_ns: 0i64,
                     last_seen_timestamp_ns: 0i64,
                },
                touchpad: {
                    events_count: 0u64,
                    handled_events_count: 0u64,
                    last_generated_timestamp_ns: 0i64,
                    last_seen_timestamp_ns: 0i64,
               },
            }
        });

        handler
            .clone()
            .handle_input_event(create_fake_handled_input_event(zx::Time::from_nanos(44)))
            .await;
        assert_data_tree!(inspector, root: {
            test_node: contains {
                events_count: 3u64,
                last_seen_timestamp_ns: 42i64,
                last_generated_timestamp_ns: 44i64,
                consumer_controls: {
                     events_count: 0u64,
                     handled_events_count: 0u64,
                     last_generated_timestamp_ns: 0i64,
                     last_seen_timestamp_ns: 0i64,
                },
                fake: {
                     events_count: 3u64,
                     handled_events_count: 1u64,
                     last_generated_timestamp_ns: 44i64,
                     last_seen_timestamp_ns: 42i64,
                },
                keyboard: {
                     events_count: 0u64,
                     handled_events_count: 0u64,
                     last_generated_timestamp_ns: 0i64,
                     last_seen_timestamp_ns: 0i64,
                },
                light_sensor: {
                     events_count: 0u64,
                     handled_events_count: 0u64,
                     last_generated_timestamp_ns: 0i64,
                     last_seen_timestamp_ns: 0i64,
                },
                mouse: {
                     events_count: 0u64,
                     handled_events_count: 0u64,
                     last_generated_timestamp_ns: 0i64,
                     last_seen_timestamp_ns: 0i64,
                },
                touch_screen: {
                     events_count: 0u64,
                     handled_events_count: 0u64,
                     last_generated_timestamp_ns: 0i64,
                     last_seen_timestamp_ns: 0i64,
                },
                touchpad: {
                    events_count: 0u64,
                    handled_events_count: 0u64,
                    last_generated_timestamp_ns: 0i64,
                    last_seen_timestamp_ns: 0i64,
               },
            }
        });
    }

    #[fasync::run_singlethreaded(test)]
    async fn verify_inspect_with_recent_events_log() {
        let inspector = inspect::Inspector::default();
        let root = inspector.root();
        let test_node = root.create_child("test_node");
        let supported_input_devices: HashSet<&InputDeviceType> = HashSet::from([
            &input_device::InputDeviceType::Keyboard,
            &input_device::InputDeviceType::ConsumerControls,
            &input_device::InputDeviceType::LightSensor,
            &input_device::InputDeviceType::Mouse,
            &input_device::InputDeviceType::Touch,
        ]);

        let handler = super::InspectHandler::new_internal(
            test_node,
            fixed_now,
            &supported_input_devices,
            /* displays_recent_events = */ true,
        );
        assert_data_tree!(inspector, root: {
            test_node: contains {
                events_count: 0u64,
                last_seen_timestamp_ns: 0i64,
                last_generated_timestamp_ns: 0i64,
                recent_events_log: {},
                consumer_controls: {
                     events_count: 0u64,
                     handled_events_count: 0u64,
                     last_generated_timestamp_ns: 0i64,
                     last_seen_timestamp_ns: 0i64,
                },
                fake: {
                     events_count: 0u64,
                     handled_events_count: 0u64,
                     last_generated_timestamp_ns: 0i64,
                     last_seen_timestamp_ns: 0i64,
                },
                keyboard: {
                     events_count: 0u64,
                     handled_events_count: 0u64,
                     last_generated_timestamp_ns: 0i64,
                     last_seen_timestamp_ns: 0i64,
                },
                light_sensor: {
                     events_count: 0u64,
                     handled_events_count: 0u64,
                     last_generated_timestamp_ns: 0i64,
                     last_seen_timestamp_ns: 0i64,
                },
                mouse: {
                     events_count: 0u64,
                     handled_events_count: 0u64,
                     last_generated_timestamp_ns: 0i64,
                     last_seen_timestamp_ns: 0i64,
                },
                touch_screen: {
                     events_count: 0u64,
                     handled_events_count: 0u64,
                     last_generated_timestamp_ns: 0i64,
                     last_seen_timestamp_ns: 0i64,
                },
                touchpad: {
                    events_count: 0u64,
                    handled_events_count: 0u64,
                    last_generated_timestamp_ns: 0i64,
                    last_seen_timestamp_ns: 0i64,
               },
           }
        });

        handler
            .clone()
            .handle_input_event(create_fake_input_event(zx::Time::from_nanos(43i64)))
            .await;
        assert_data_tree!(inspector, root: {
            test_node: contains {
                events_count: 1u64,
                last_seen_timestamp_ns: 42i64,
                last_generated_timestamp_ns: 43i64,
                recent_events_log: {
                    "000_fake_event": {
                        event_time: 43i64,
                    },
                },
                consumer_controls: {
                     events_count: 0u64,
                     handled_events_count: 0u64,
                     last_generated_timestamp_ns: 0i64,
                     last_seen_timestamp_ns: 0i64,
                },
                fake: {
                     events_count: 1u64,
                     handled_events_count: 0u64,
                     last_generated_timestamp_ns: 43i64,
                     last_seen_timestamp_ns: 42i64,
                },
                keyboard: {
                     events_count: 0u64,
                     handled_events_count: 0u64,
                     last_generated_timestamp_ns: 0i64,
                     last_seen_timestamp_ns: 0i64,
                },
                light_sensor: {
                     events_count: 0u64,
                     handled_events_count: 0u64,
                     last_generated_timestamp_ns: 0i64,
                     last_seen_timestamp_ns: 0i64,
                },
                mouse: {
                     events_count: 0u64,
                     handled_events_count: 0u64,
                     last_generated_timestamp_ns: 0i64,
                     last_seen_timestamp_ns: 0i64,
                },
                touch_screen: {
                     events_count: 0u64,
                     handled_events_count: 0u64,
                     last_generated_timestamp_ns: 0i64,
                     last_seen_timestamp_ns: 0i64,
                },
                touchpad: {
                    events_count: 0u64,
                    handled_events_count: 0u64,
                    last_generated_timestamp_ns: 0i64,
                    last_seen_timestamp_ns: 0i64,
               },
            }
        });

        handler
            .clone()
            .handle_input_event(create_fake_input_event(zx::Time::from_nanos(44i64)))
            .await;
        assert_data_tree!(inspector, root: {
            test_node: contains {
                events_count: 2u64,
                last_seen_timestamp_ns: 42i64,
                last_generated_timestamp_ns: 44i64,
                recent_events_log: {
                    "000_fake_event": {
                        event_time: 43i64,
                    },
                    "001_fake_event": {
                        event_time: 44i64,
                    },
                },
                consumer_controls: {
                     events_count: 0u64,
                     handled_events_count: 0u64,
                     last_generated_timestamp_ns: 0i64,
                     last_seen_timestamp_ns: 0i64,
                },
                fake: {
                     events_count: 2u64,
                     handled_events_count: 0u64,
                     last_generated_timestamp_ns: 44i64,
                     last_seen_timestamp_ns: 42i64,
                },
                keyboard: {
                     events_count: 0u64,
                     handled_events_count: 0u64,
                     last_generated_timestamp_ns: 0i64,
                     last_seen_timestamp_ns: 0i64,
                },
                light_sensor: {
                     events_count: 0u64,
                     handled_events_count: 0u64,
                     last_generated_timestamp_ns: 0i64,
                     last_seen_timestamp_ns: 0i64,
                },
                mouse: {
                     events_count: 0u64,
                     handled_events_count: 0u64,
                     last_generated_timestamp_ns: 0i64,
                     last_seen_timestamp_ns: 0i64,
                },
                touch_screen: {
                     events_count: 0u64,
                     handled_events_count: 0u64,
                     last_generated_timestamp_ns: 0i64,
                     last_seen_timestamp_ns: 0i64,
                },
                touchpad: {
                    events_count: 0u64,
                    handled_events_count: 0u64,
                    last_generated_timestamp_ns: 0i64,
                    last_seen_timestamp_ns: 0i64,
               },
            }
        });

        handler
            .clone()
            .handle_input_event(create_fake_handled_input_event(zx::Time::from_nanos(44)))
            .await;
        assert_data_tree!(inspector, root: {
            test_node: contains {
                events_count: 3u64,
                last_seen_timestamp_ns: 42i64,
                last_generated_timestamp_ns: 44i64,
                recent_events_log: {
                    "000_fake_event": {
                        event_time: 43i64,
                    },
                    "001_fake_event": {
                        event_time: 44i64,
                    },
                    "002_fake_event": {
                        event_time: 44i64,
                    },
                },
                consumer_controls: {
                     events_count: 0u64,
                     handled_events_count: 0u64,
                     last_generated_timestamp_ns: 0i64,
                     last_seen_timestamp_ns: 0i64,
                },
                fake: {
                     events_count: 3u64,
                     handled_events_count: 1u64,
                     last_generated_timestamp_ns: 44i64,
                     last_seen_timestamp_ns: 42i64,
                },
                keyboard: {
                     events_count: 0u64,
                     handled_events_count: 0u64,
                     last_generated_timestamp_ns: 0i64,
                     last_seen_timestamp_ns: 0i64,
                },
                light_sensor: {
                     events_count: 0u64,
                     handled_events_count: 0u64,
                     last_generated_timestamp_ns: 0i64,
                     last_seen_timestamp_ns: 0i64,
                },
                mouse: {
                     events_count: 0u64,
                     handled_events_count: 0u64,
                     last_generated_timestamp_ns: 0i64,
                     last_seen_timestamp_ns: 0i64,
                },
                touch_screen: {
                     events_count: 0u64,
                     handled_events_count: 0u64,
                     last_generated_timestamp_ns: 0i64,
                     last_seen_timestamp_ns: 0i64,
                },
                touchpad: {
                    events_count: 0u64,
                    handled_events_count: 0u64,
                    last_generated_timestamp_ns: 0i64,
                    last_seen_timestamp_ns: 0i64,
               },
            }
        });
    }

    #[test_case([i64::MIN]; "min value")]
    #[test_case([-1]; "negative value")]
    #[test_case([0]; "zero")]
    #[test_case([1]; "positive value")]
    #[test_case([i64::MAX]; "max value")]
    #[test_case([1_000_000, 10_000_000, 100_000_000, 1000_000_000]; "multiple values")]
    #[fuchsia::test(allow_stalls = false)]
    async fn updates_latency_histogram(
        latencies_nsec: impl IntoIterator<Item = i64> + Clone + 'static,
    ) {
        let inspector = inspect::Inspector::default();
        let root = inspector.root();
        let test_node = root.create_child("test_node");

        let mut seen_timestamps = latencies_nsec.clone().into_iter().map(zx::Time::from_nanos);
        let now = move || {
            seen_timestamps.next().expect("internal error: test has more events than latencies")
        };
        let handler = super::InspectHandler::new_internal(
            test_node,
            now,
            &hashset! {},
            /* displays_recent_events = */ false,
        );
        for _latency in latencies_nsec.clone() {
            handler.clone().handle_input_event(create_fake_input_event(zx::Time::ZERO)).await;
        }

        let mut histogram_assertion =
            fuchsia_inspect::HistogramAssertion::exponential(super::LATENCY_HISTOGRAM_PROPERTIES);
        histogram_assertion
            .insert_values(latencies_nsec.into_iter().map(|nsec| nsec / 1000 / 1000));
        assert_data_tree!(inspector, root: {
            test_node: contains {
                pipeline_latency_ms: histogram_assertion
            }
        })
    }
}
