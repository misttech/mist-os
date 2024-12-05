// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::uinput;
use fidl_fuchsia_input::Key;
use fidl_fuchsia_ui_input::MediaButtonsEvent;
use fidl_fuchsia_ui_pointer::{
    EventPhase as FidlEventPhase, TouchEvent as FidlTouchEvent, TouchPointerSample,
};
use starnix_logging::log_warn;
use starnix_types::time::{time_from_timeval, timeval_from_time};
use starnix_uapi::errors::Errno;
use starnix_uapi::{error, uapi};
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::sync::LazyLock;
use {fidl_fuchsia_input_report as fir, fidl_fuchsia_ui_input3 as fuiinput};

type SlotId = usize;
type TrackingId = u32;
type TimeNanos = i64;

/// TRACKING_ID changed to -1 means the contact is lifted.
const LIFTED_TRACKING_ID: i32 = -1;

/// For unify conversion for ABS_MT_POSITION_X, ABS_MT_POSITION_Y.
enum MtPosition {
    X(i64),
    Y(i64),
}

/// A state machine accepts uapi::input_event, produces fir::InputReport
/// when (Touch Event.. + Sync Event) received.
///
/// This parser currently only supports "Type B" protocol in:
/// https://www.kernel.org/doc/Documentation/input/multi-touch-protocol.txt
///
/// Each event report contains a sequence of packets (uapi::input_event).
/// EV_EYN means the report is completed.
///
/// There may be multiple contacts inside the sequence of packets, each contact
/// data started by a MT_SLOT event with slot_id.
///
/// In the initiated contact, slot (X) will include a ABS_MT_TRACKING_ID
/// event.
/// In following events in slot (X) will continue use the same TRACKING_ID.
/// Slot (X) with TRACKING_ID (-1) means the contact is lifted.
///
/// Warning output, clean state and return errno when received events:
/// - unknown events type / code.
/// - "Type A" events: SYN_MT_REPORT.
/// - invalid event.
/// - not follow "Type B" pattern.
#[derive(Debug, Default, PartialEq)]
pub struct LinuxTouchEventParser {
    /// Store received events while conversion still ongoing.
    cached_events: Vec<uapi::input_event>,
    /// Store slot id -> tracking id mapping for Type B protocol. Remove the
    /// mapping when contact lifted.
    slot_id_to_tracking_id: HashMap<SlotId, TrackingId>,

    // Following states only when start parsing one event sequence (SYN_REPORT
    // received).
    /// There will be multiple slots in one event sequence, this field records
    /// the current parsing slot's id.
    current_slot_id: Option<SlotId>,
    /// The contact information of current slot.
    current_contact: Option<fir::ContactInputReport>,
    /// This record processed slots' id to check if duplicated slot id appear
    /// in one event sequence.
    processed_slots: HashSet<SlotId>,
    /// This store parsed contacts.
    contacts: Vec<fir::ContactInputReport>,
}

impl LinuxTouchEventParser {
    /// Create the LinuxTouchEventParser.
    pub fn create() -> Self {
        Self {
            cached_events: vec![],
            slot_id_to_tracking_id: HashMap::new(),
            current_slot_id: None,
            current_contact: None,
            processed_slots: HashSet::new(),
            contacts: vec![],
        }
    }

    /// Clean states stored in the parser, call when parser got any error.
    fn reset_state(&mut self) {
        self.cached_events = vec![];
        self.slot_id_to_tracking_id = HashMap::new();
        self.reset_sequence_state();
    }

    /// Clean state for parsing sequence, call for parsing sequence begin and end.
    fn reset_sequence_state(&mut self) {
        self.current_slot_id = None;
        self.current_contact = None;
        self.processed_slots = HashSet::new();
        self.contacts = vec![];
    }

    /// call when input_event for current_contact is end:
    /// - MT_SLOT: new slot begins.
    /// - SYN_REPORT: sequence is ended.
    ///
    /// This checks if current_contact have enough information. If not, return errno.
    /// If contact is lifted, don't add to the list.
    fn add_current_contact_to_list(&mut self) -> Result<(), Errno> {
        match &self.current_contact {
            Some(current) => {
                if !validate_contact_input_report(&current) {
                    log_warn!("current_contact does not have required information, current_contact = {:?}", current);
                    self.reset_state();
                    return error!(EINVAL);
                }

                self.contacts.push(current.clone());
            }
            None => {}
        }
        Ok(())
    }

    /// There are 2 possible state for a MT_SLOT event:
    /// - This is the first slot so no previous slot.
    /// - This is the end of previous slot.
    ///   * Return errno if duplicated slot found.
    ///   * Add the current contact to the list if the current contact is
    ///     valid, otherwise return errno.
    ///
    /// Add the slot id to processed_slots, current_slot_id and reset
    /// current_contact.
    fn mt_slot(&mut self, new_slot_id: SlotId) -> Result<(), Errno> {
        if self.processed_slots.contains(&new_slot_id) {
            log_warn!("duplicated slot_id in one sequence, slot_id = {}", new_slot_id);
            self.reset_state();
            return error!(EINVAL);
        }

        match self.current_slot_id {
            // This is the first slot in the sequence.
            None => {}
            // Complete the previous slot.
            Some(_) => {
                self.add_current_contact_to_list()?;
            }
        }

        self.processed_slots.insert(new_slot_id);
        self.current_slot_id = Some(new_slot_id);
        self.current_contact = Some(fir::ContactInputReport {
            contact_id: self.slot_id_to_tracking_id.get(&new_slot_id).copied(),
            ..fir::ContactInputReport::default()
        });

        Ok(())
    }

    /// Type B requires ABS events leading by a MT_SLOT.
    /// Returns SlotId if the requirement meet, or returns errno.
    fn get_current_slot_id_or_err(&mut self, curr_event: &str) -> Result<SlotId, Errno> {
        match self.current_slot_id {
            Some(slot_id) => Ok(slot_id),
            None => {
                log_warn!("{:?} must be following ABS_MT_SLOT", curr_event);
                self.reset_state();
                error!(EINVAL)
            }
        }
    }

    /// MT_TRACKING_ID associate tracking id with slot id, this event is must
    /// have for a slot first appear in event sequences. Add slot id ->
    /// tracking id mapping and add set tracking id as contact id in this case.
    ///
    /// Tracking id = -1 means the contact is lifted, the slot id -> tracking
    /// id mapping should also be removed after this.
    ///
    /// Tracking id should not change otherwise, return errno.
    ///
    /// Returns errno if no leading MT_SLOT.
    fn mt_tracking_id(&mut self, tracking_id: i32) -> Result<(), Errno> {
        let slot_id = self.get_current_slot_id_or_err("ABS_MT_TRACKING_ID")?;

        if tracking_id < LIFTED_TRACKING_ID {
            // TRACKING_ID < -1, invalid value.
            log_warn!("invalid TRACKING_ID {}", tracking_id);
            self.reset_state();
            return error!(EINVAL);
        }

        if tracking_id == LIFTED_TRACKING_ID {
            self.slot_id_to_tracking_id.remove(&slot_id);
            self.current_contact = None;

            return Ok(());
        }

        // A valid TRACKING_ID. Check if it is changed.
        let tid = tracking_id as TrackingId;
        match self.slot_id_to_tracking_id.get(&slot_id) {
            Some(id) => {
                if tid != *id {
                    log_warn!(
                        "TRACKING_ID changed form {} to {} for unknown reason for slot {}",
                        *id,
                        tid,
                        slot_id
                    );
                    self.reset_state();
                    return error!(EINVAL);
                }
            }
            None => {
                self.slot_id_to_tracking_id.insert(slot_id, tid);
            }
        }
        match &self.current_contact {
            Some(contact) => {
                self.current_contact =
                    Some(fir::ContactInputReport { contact_id: Some(tid), ..contact.clone() });
            }
            None => {
                log_warn!("current_contact is None when set TRACKING_ID, this should never reach");
                self.reset_state();
                return error!(EINVAL);
            }
        }

        Ok(())
    }

    /// Set contact position. Returns errno if:
    /// - no leading MT_SLOT.
    /// - contact is lifted.
    fn mt_position_x_y(&mut self, mt_position: MtPosition) -> Result<(), Errno> {
        let ty = match mt_position {
            MtPosition::X(_) => "ABS_MT_POSITION_X",
            MtPosition::Y(_) => "ABS_MT_POSITION_Y",
        };
        let _ = self.get_current_slot_id_or_err(ty)?;

        match &self.current_contact {
            Some(contact) => {
                match mt_position {
                    MtPosition::X(x) => {
                        self.current_contact = Some(fir::ContactInputReport {
                            position_x: Some(x),
                            ..contact.clone()
                        });
                    }
                    MtPosition::Y(y) => {
                        self.current_contact = Some(fir::ContactInputReport {
                            position_y: Some(y),
                            ..contact.clone()
                        });
                    }
                }
                Ok(())
            }
            None => {
                log_warn!("current_contact is None when set position");
                self.reset_state();
                return error!(EINVAL);
            }
        }
    }

    fn produce_input_report(
        &mut self,
        event_time: zx::MonotonicInstant,
    ) -> Result<Option<fir::InputReport>, Errno> {
        self.reset_sequence_state();

        let cached_events = self.cached_events.clone();

        for e in cached_events {
            match e.code as u32 {
                uapi::ABS_MT_SLOT => {
                    let slot_id = e.value as SlotId;
                    self.mt_slot(slot_id)?;
                }
                uapi::ABS_MT_TRACKING_ID => {
                    self.mt_tracking_id(e.value)?;
                }
                uapi::ABS_MT_POSITION_X => {
                    self.mt_position_x_y(MtPosition::X(e.value as i64))?;
                }
                uapi::ABS_MT_POSITION_Y => {
                    self.mt_position_x_y(MtPosition::Y(e.value as i64))?;
                }
                _ => {
                    // handle() ensure only 4 event_code above will be stored in cached_events.
                    unreachable!();
                }
            }
        }

        // The last event.
        self.add_current_contact_to_list()?;

        // All events are processed
        self.cached_events = vec![];

        let res = Ok(Some(fir::InputReport {
            event_time: Some(event_time.into_nanos()),
            touch: Some(fir::TouchInputReport {
                contacts: Some(self.contacts.clone()),
                ..Default::default()
            }),
            ..Default::default()
        }));

        self.reset_sequence_state();

        res
    }

    /// Handle received input_event, only produce event when SYN_REPORT is received.
    pub fn handle(&mut self, e: uapi::input_event) -> Result<Option<fir::InputReport>, Errno> {
        let event_code = e.code as u32;
        match e.type_ as u32 {
            uapi::EV_SYN => match event_code {
                uapi::SYN_REPORT => self.produce_input_report(time_from_timeval(e.time)?),
                uapi::SYN_MT_REPORT => {
                    log_warn!("Touchscreen got 'Type A' event SYN_MT_REPORT");
                    self.reset_state();
                    error!(EINVAL)
                }
                _ => {
                    log_warn!("Touchscreen got unexpected EV_SYN, event = {:?}", e);
                    self.reset_state();
                    error!(EINVAL)
                }
            },
            uapi::EV_ABS => match event_code {
                uapi::ABS_MT_SLOT
                | uapi::ABS_MT_TRACKING_ID
                | uapi::ABS_MT_POSITION_X
                | uapi::ABS_MT_POSITION_Y => {
                    self.cached_events.push(e);
                    Ok(None)
                }
                _ => {
                    log_warn!("Touchscreen got unexpected EV_ABS, event = {:?}", e);
                    self.reset_state();
                    error!(EINVAL)
                }
            },
            uapi::EV_KEY => {
                match event_code {
                    // For "Type B" protocol, BTN_TOUCH can be ignored.
                    uapi::BTN_TOUCH => Ok(None),
                    _ => {
                        log_warn!("Touchscreen got unexpected EV_KEY, event = {:?}", e);
                        self.reset_state();
                        error!(EINVAL)
                    }
                }
            }
            _ => {
                log_warn!("Touchscreen got unexpected event type, got = {:?}", e);
                self.reset_state();
                error!(EINVAL)
            }
        }
    }
}

/// ContactInputReport should contains X, Y, Contact ID
fn validate_contact_input_report(c: &fir::ContactInputReport) -> bool {
    match c {
        &fir::ContactInputReport {
            contact_id: Some(_),
            position_x: Some(_),
            position_y: Some(_),
            ..
        } => true,
        _ => false,
    }
}

#[derive(Debug, thiserror::Error)]
enum TouchEventConversionError {
    #[error("Event does not include enough information")]
    NotEnoughInformation,
    #[error("no more available slot id")]
    NoMoreAvailableSlotId,
    #[error("receive pointer add already added")]
    PointerAdded,
    #[error("receive pointer change/remove before added")]
    PointerNotFound,
    #[error("Input pipeline does not send out Cancel")]
    PointerCancel,
}

#[derive(Debug, Clone)]
struct TouchEvent {
    time_nanos: TimeNanos,
    pointer_id: TrackingId,
    phase: FidlEventPhase,
    x: i32,
    y: i32,
}

impl TryFrom<FidlTouchEvent> for TouchEvent {
    type Error = TouchEventConversionError;
    fn try_from(e: FidlTouchEvent) -> Result<TouchEvent, Self::Error> {
        match e {
            FidlTouchEvent {
                timestamp: Some(time_nanos),
                pointer_sample:
                    Some(TouchPointerSample {
                        position_in_viewport: Some([x, y]),
                        phase: Some(phase),
                        interaction: Some(id),
                        ..
                    }),
                ..
            } => Ok(TouchEvent {
                time_nanos,
                pointer_id: id.pointer_id,
                phase,
                x: x as i32,
                y: y as i32,
            }),
            _ => Err(TouchEventConversionError::NotEnoughInformation),
        }
    }
}

/// FuchsiaTouchEventToLinuxTouchEventConverter handles fuchsia.ui.pointer.TouchEvents
/// and converts them to Linux uapi::input_event in Multi Touch Protocol B.
#[derive(Debug, Default, PartialEq)]
pub struct FuchsiaTouchEventToLinuxTouchEventConverter {
    pointer_id_to_slot_id: HashMap<TrackingId, SlotId>,
}

const MAX_TOUCH_CONTACT: usize = 10;

pub struct LinuxTouchEventBatch {
    // Linux Multi Touch Protocol B events
    pub events: VecDeque<uapi::input_event>,
    pub last_event_time_ns: i64,
    pub count_converted_fidl_events: u64,
    pub count_ignored_fidl_events: u64,
    pub count_unexpected_fidl_events: u64,
}

impl LinuxTouchEventBatch {
    pub fn new() -> Self {
        Self {
            events: VecDeque::new(),
            last_event_time_ns: 0,
            count_converted_fidl_events: 0,
            count_ignored_fidl_events: 0,
            count_unexpected_fidl_events: 0,
        }
    }
}

impl FuchsiaTouchEventToLinuxTouchEventConverter {
    pub fn create() -> Self {
        Self { pointer_id_to_slot_id: HashMap::new() }
    }

    /// In Protocol B, the driver should only advertise as many slots as the hardware can report
    /// so this converter uses `available_slot_id` to find the first available slot id.
    fn available_slot_id(&self) -> Option<SlotId> {
        let mut used_slot_ids = bit_vec::BitVec::<u32>::from_elem(MAX_TOUCH_CONTACT, false);
        for slot_id in self.pointer_id_to_slot_id.values() {
            used_slot_ids.set(*slot_id, true);
        }

        used_slot_ids.iter().position(|used| !used)
    }

    /// Converts fidl touch events to a batch of Linux Multi Touch Protocol B events.
    ///
    /// One vector of fidl touch events may convert to multiple Linux Multi Touch Protocol B
    /// sequences because:
    /// - Same pointer happens multiple times in the vector of fidl touch events.
    /// - Linux Multi Touch Protocol B does not allow slot with same id appear multiple times
    ///   one sequence.
    pub fn handle(&mut self, events: Vec<FidlTouchEvent>) -> LinuxTouchEventBatch {
        let mut batch = LinuxTouchEventBatch::new();

        // TODO(https://fxbug.dev/348726475): Group events by timestamp here because events from
        // fuchsia.ui.pointer.touch.Watch may not sorted by timestamp.
        let mut sequences: BTreeMap<TimeNanos, Vec<TouchEvent>> = BTreeMap::new();
        for event in events.into_iter() {
            match TouchEvent::try_from(event) {
                Ok(e) => {
                    sequences.entry(e.time_nanos).or_default().push(e);
                }
                Err(_) => {
                    batch.count_ignored_fidl_events += 1;
                }
            }
        }

        if sequences.is_empty() {
            return batch;
        }

        batch.last_event_time_ns = *sequences.last_key_value().unwrap().0;

        for (time_nanos, seq) in sequences.iter() {
            let count_events = seq.len() as u64;
            match self.translate_sequence(*time_nanos, seq) {
                Ok(mut res) => {
                    batch.events.append(&mut res);
                    batch.count_converted_fidl_events += count_events;
                }
                Err(e) => {
                    batch.count_unexpected_fidl_events += count_events;
                    self.reset_state();
                    log_warn!("{}", e);
                }
            }
        }

        batch
    }

    /// Translates a vec of fidl FidlTouchEvent to Linux Multi Touch Protocol B sequence. Caller
    /// ensures the given vec does not include duplicated pointer, and all event includes same
    /// timestamp.
    ///
    /// Return err for unexpected events which should be filtered in earlier component:
    /// input-pipeline and scenic. If 1 event is unexpected, translate_sequence() drops all events
    /// from the same scan from driver.
    fn translate_sequence(
        &mut self,
        time_nanos: TimeNanos,
        events: &Vec<TouchEvent>,
    ) -> Result<VecDeque<uapi::input_event>, TouchEventConversionError> {
        let mut existing_slot: VecDeque<uapi::input_event> = VecDeque::new();
        let mut new_slots: VecDeque<uapi::input_event> = VecDeque::new();

        let time = timeval_from_time(zx::MonotonicInstant::from_nanos(time_nanos));

        let no_contact_before_process_events = self.pointer_id_to_slot_id.is_empty();
        let mut need_btn_touch_down = false;
        let mut need_btn_touch_up = false;

        // TODO(https://fxbug.dev/314151713): use event.device_info to route event to different
        // device file.

        for (index, event) in events.iter().enumerate() {
            let pointer_id = event.pointer_id;
            let slot_id = self.pointer_id_to_slot_id.get(&pointer_id).copied();

            match event.phase {
                FidlEventPhase::Add => match slot_id {
                    None => {
                        let new_slot_id = match self.available_slot_id() {
                            Some(index) => index,
                            None => {
                                return Err(TouchEventConversionError::NoMoreAvailableSlotId);
                            }
                        };

                        if no_contact_before_process_events {
                            need_btn_touch_down = true;
                        }

                        self.pointer_id_to_slot_id.insert(pointer_id, new_slot_id);

                        new_slots.push_back(uapi::input_event {
                            time,
                            type_: uapi::EV_ABS as u16,
                            code: uapi::ABS_MT_SLOT as u16,
                            value: new_slot_id as i32,
                        });

                        new_slots.push_back(uapi::input_event {
                            time,
                            type_: uapi::EV_ABS as u16,
                            code: uapi::ABS_MT_TRACKING_ID as u16,
                            value: pointer_id as i32,
                        });

                        new_slots.push_back(uapi::input_event {
                            time,
                            type_: uapi::EV_ABS as u16,
                            code: uapi::ABS_MT_POSITION_X as u16,
                            value: event.x,
                        });

                        new_slots.push_back(uapi::input_event {
                            time,
                            type_: uapi::EV_ABS as u16,
                            code: uapi::ABS_MT_POSITION_Y as u16,
                            value: event.y,
                        });
                    }
                    Some(_) => {
                        return Err(TouchEventConversionError::PointerAdded);
                    }
                },
                FidlEventPhase::Change => match slot_id {
                    None => {
                        return Err(TouchEventConversionError::PointerNotFound);
                    }
                    Some(slot_id) => {
                        existing_slot.push_back(uapi::input_event {
                            time,
                            type_: uapi::EV_ABS as u16,
                            code: uapi::ABS_MT_SLOT as u16,
                            value: slot_id as i32,
                        });

                        existing_slot.push_back(uapi::input_event {
                            time,
                            type_: uapi::EV_ABS as u16,
                            code: uapi::ABS_MT_POSITION_X as u16,
                            value: event.x,
                        });

                        existing_slot.push_back(uapi::input_event {
                            time,
                            type_: uapi::EV_ABS as u16,
                            code: uapi::ABS_MT_POSITION_Y as u16,
                            value: event.y,
                        });
                    }
                },
                FidlEventPhase::Remove => match slot_id {
                    None => {
                        return Err(TouchEventConversionError::PointerNotFound);
                    }
                    Some(slot_id) => {
                        self.pointer_id_to_slot_id.remove(&pointer_id);

                        // Ensure BTN_TOUCH up event is only sent when the last pointer is lifted.
                        // Here check if the event is the last event from the vec prevents a false
                        // BTN_TOUCH up event if any error reset_state of this converter.
                        if index == events.len() - 1 && self.pointer_id_to_slot_id.is_empty() {
                            need_btn_touch_up = true;
                        }

                        existing_slot.push_back(uapi::input_event {
                            time,
                            type_: uapi::EV_ABS as u16,
                            code: uapi::ABS_MT_SLOT as u16,
                            value: slot_id as i32,
                        });

                        existing_slot.push_back(uapi::input_event {
                            time,
                            type_: uapi::EV_ABS as u16,
                            code: uapi::ABS_MT_TRACKING_ID as u16,
                            value: LIFTED_TRACKING_ID,
                        });
                    }
                },
                FidlEventPhase::Cancel => {
                    return Err(TouchEventConversionError::PointerCancel);
                }
            }
        }

        let mut result: VecDeque<uapi::input_event> = VecDeque::new();

        if need_btn_touch_down {
            result.push_back(uapi::input_event {
                time,
                type_: uapi::EV_KEY as u16,
                code: uapi::BTN_TOUCH as u16,
                value: 1,
            });
        } else if need_btn_touch_up {
            result.push_back(uapi::input_event {
                time,
                type_: uapi::EV_KEY as u16,
                code: uapi::BTN_TOUCH as u16,
                value: 0,
            });
        }

        result.append(&mut existing_slot);
        result.append(&mut new_slots);

        if result.len() > 0 {
            result.push_back(uapi::input_event {
                time,
                type_: uapi::EV_SYN as u16,
                code: uapi::SYN_REPORT as u16,
                value: 0,
            });
        }

        Ok(result)
    }

    fn reset_state(&mut self) {
        self.pointer_id_to_slot_id = HashMap::new();
    }
}

/// Converts fuchsia KeyEvent to a vector of `uapi::input_events`.
///
/// A single `KeyEvent` may translate into multiple `uapi::input_events`.
/// 1 key event and 1 sync event.
///
/// If translation fails an empty vector is returned.
pub fn parse_fidl_keyboard_event_to_linux_input_event(
    e: &fuiinput::KeyEvent,
) -> Vec<uapi::input_event> {
    #[allow(clippy::vec_init_then_push, reason = "mass allow for https://fxbug.dev/381896734")]
    match e {
        &fuiinput::KeyEvent {
            timestamp: Some(time_nanos),
            type_: Some(event_type),
            key: Some(key),
            ..
        } => {
            let lkey = KEY_MAP.fuchsia_input_key_to_linux_keycode(key);
            // return empty for unknown keycode.
            if lkey == uapi::KEY_RESERVED {
                return vec![];
            }
            let lkey = match lkey {
                // TODO(b/312467059): keep this ESC -> Power workaround for debug.
                uapi::KEY_ESC => {
                    if uinput::uinput_running() {
                        uapi::KEY_ESC
                    } else {
                        uapi::KEY_POWER
                    }
                }
                k => k,
            };

            let time = timeval_from_time(zx::MonotonicInstant::from_nanos(time_nanos));
            let key_event = uapi::input_event {
                time,
                type_: uapi::EV_KEY as u16,
                code: lkey as u16,
                value: if event_type == fuiinput::KeyEventType::Pressed { 1 } else { 0 },
            };

            let sync_event = uapi::input_event {
                // See https://www.kernel.org/doc/Documentation/input/event-codes.rst.
                time,
                type_: uapi::EV_SYN as u16,
                code: uapi::SYN_REPORT as u16,
                value: 0,
            };

            let mut events = vec![];
            events.push(key_event);
            events.push(sync_event);
            events
        }
        _ => vec![],
    }
}

/// A state machine accepts uapi::input_event, produces fir::InputReport
/// when (Key Event + Sync Event) received. It also maintain the currently
/// pressing key list.
///
/// Warning output, clean state and return errno when received events:
/// - unknown keycode.
/// - invalid event.
/// - not follow (Key Event + Sync Event) pattern.
#[derive(Debug, PartialEq)]
pub struct LinuxKeyboardEventParser {
    cached_event: Option<uapi::input_event>,
    pressing_keys: Vec<Key>,
}

impl LinuxKeyboardEventParser {
    pub fn create() -> Self {
        Self { cached_event: None, pressing_keys: vec![] }
    }

    fn reset_state(&mut self) {
        self.cached_event = None;
        self.pressing_keys = vec![];
    }

    fn produce_input_report(
        &mut self,
        e: uapi::input_event,
    ) -> Result<Option<fir::InputReport>, Errno> {
        self.cached_event = None;

        let fkey = KEY_MAP.linux_keycode_to_fuchsia_input_key(e.code as u32);
        // produce no input report for unknown key, there is a warning log from
        // linux_keycode_to_fuchsia_input_key().
        if fkey == Key::Unknown {
            self.reset_state();
            return error!(EINVAL);
        }
        match e.value {
            // Press
            1 => {
                if self.pressing_keys.contains(&fkey) {
                    log_warn!("keyboard receive a press key event while the key is already pressing, key = {:?}", fkey);
                    self.reset_state();
                    return error!(EINVAL);
                }
                self.pressing_keys.push(fkey);
            }
            // Release
            0 => {
                if !self.pressing_keys.contains(&fkey) {
                    log_warn!("keyboard receive a release key event while the key is not pressing, key = {:?}", fkey);
                    self.reset_state();
                    return error!(EINVAL);
                }
                // remove the released key.
                self.pressing_keys =
                    self.pressing_keys.clone().into_iter().filter(|x| *x != fkey).collect();
            }
            _ => {
                log_warn!("key event has invalid value field, event = {:?}", e);
                self.reset_state();
                return error!(EINVAL);
            }
        }

        let keyboard_report = fir::KeyboardInputReport {
            pressed_keys3: Some(self.pressing_keys.clone()),
            ..Default::default()
        };

        Ok(Some(fir::InputReport {
            event_time: Some(time_from_timeval::<zx::MonotonicTimeline>(e.time)?.into_nanos()),
            keyboard: Some(keyboard_report),
            ..Default::default()
        }))
    }

    pub fn handle(&mut self, e: uapi::input_event) -> Result<Option<fir::InputReport>, Errno> {
        match self.cached_event {
            Some(key_event) => match e.type_ as u32 {
                uapi::EV_SYN => self.produce_input_report(key_event),
                _ => {
                    self.reset_state();
                    log_warn!("keyboard expect EV_SYN event but got = {:?}", e);
                    error!(EINVAL)
                }
            },
            None => match e.type_ as u32 {
                uapi::EV_KEY => {
                    self.cached_event = Some(e);
                    Ok(None)
                }
                _ => {
                    self.reset_state();
                    log_warn!("keyboard expect EV_KEY event but got = {:?}", e);
                    error!(EINVAL)
                }
            },
        }
    }
}

static KEY_MAP: LazyLock<KeyMap> = LazyLock::new(|| init_key_map());

/// linux <-> fuchsia key map allow search from 2 way.
pub struct KeyMap {
    linux_to_fuchsia: HashMap<u32, Key>,
    fuchsia_to_linux: HashMap<Key, u32>,
}

impl KeyMap {
    fn insert(&mut self, linux_keycode: u32, fuchsia_key: Key) {
        // Should not have any conflict keys.
        assert!(
            !self.linux_to_fuchsia.contains_key(&linux_keycode),
            "conflicted linux keycode={} fuchsia keycode={:?}",
            linux_keycode,
            fuchsia_key
        );
        assert!(
            !self.fuchsia_to_linux.contains_key(&fuchsia_key),
            "conflicted fuchsia keycode={:?}, linux keycode={}",
            fuchsia_key,
            linux_keycode
        );

        self.linux_to_fuchsia.insert(linux_keycode, fuchsia_key);
        self.fuchsia_to_linux.insert(fuchsia_key, linux_keycode);
    }

    fn linux_keycode_to_fuchsia_input_key(&self, key: u32) -> Key {
        match self.linux_to_fuchsia.get(&key) {
            Some(k) => *k,
            None => {
                log_warn!("unknown linux keycode {}", key);
                Key::Unknown
            }
        }
    }

    fn fuchsia_input_key_to_linux_keycode(&self, key: Key) -> u32 {
        match self.fuchsia_to_linux.get(&key) {
            Some(k) => *k,
            None => {
                log_warn!("unknown fuchsia key {:?}", key);
                // this is the invalid key code 0
                uapi::KEY_RESERVED
            }
        }
    }
}

fn init_key_map() -> KeyMap {
    let mut m = KeyMap { linux_to_fuchsia: HashMap::new(), fuchsia_to_linux: HashMap::new() };

    m.insert(uapi::KEY_ESC, Key::Escape);
    m.insert(uapi::KEY_1, Key::Key1);
    m.insert(uapi::KEY_2, Key::Key2);
    m.insert(uapi::KEY_3, Key::Key3);
    m.insert(uapi::KEY_4, Key::Key4);
    m.insert(uapi::KEY_5, Key::Key5);
    m.insert(uapi::KEY_6, Key::Key6);
    m.insert(uapi::KEY_7, Key::Key7);
    m.insert(uapi::KEY_8, Key::Key8);
    m.insert(uapi::KEY_9, Key::Key9);
    m.insert(uapi::KEY_0, Key::Key0);
    m.insert(uapi::KEY_MINUS, Key::Minus);
    m.insert(uapi::KEY_EQUAL, Key::Equals);
    m.insert(uapi::KEY_BACKSPACE, Key::Backspace);
    m.insert(uapi::KEY_TAB, Key::Tab);
    m.insert(uapi::KEY_Q, Key::Q);
    m.insert(uapi::KEY_W, Key::W);
    m.insert(uapi::KEY_E, Key::E);
    m.insert(uapi::KEY_R, Key::R);
    m.insert(uapi::KEY_T, Key::T);
    m.insert(uapi::KEY_Y, Key::Y);
    m.insert(uapi::KEY_U, Key::U);
    m.insert(uapi::KEY_I, Key::I);
    m.insert(uapi::KEY_O, Key::O);
    m.insert(uapi::KEY_P, Key::P);
    m.insert(uapi::KEY_LEFTBRACE, Key::LeftBrace);
    m.insert(uapi::KEY_RIGHTBRACE, Key::RightBrace);
    m.insert(uapi::KEY_ENTER, Key::Enter);
    m.insert(uapi::KEY_LEFTCTRL, Key::LeftCtrl);
    m.insert(uapi::KEY_A, Key::A);
    m.insert(uapi::KEY_S, Key::S);
    m.insert(uapi::KEY_D, Key::D);
    m.insert(uapi::KEY_F, Key::F);
    m.insert(uapi::KEY_G, Key::G);
    m.insert(uapi::KEY_H, Key::H);
    m.insert(uapi::KEY_J, Key::J);
    m.insert(uapi::KEY_K, Key::K);
    m.insert(uapi::KEY_L, Key::L);
    m.insert(uapi::KEY_SEMICOLON, Key::Semicolon);
    m.insert(uapi::KEY_APOSTROPHE, Key::Apostrophe);
    m.insert(uapi::KEY_GRAVE, Key::GraveAccent);
    m.insert(uapi::KEY_LEFTSHIFT, Key::LeftShift);
    m.insert(uapi::KEY_BACKSLASH, Key::Backslash);
    m.insert(uapi::KEY_Z, Key::Z);
    m.insert(uapi::KEY_X, Key::X);
    m.insert(uapi::KEY_C, Key::C);
    m.insert(uapi::KEY_V, Key::V);
    m.insert(uapi::KEY_B, Key::B);
    m.insert(uapi::KEY_N, Key::N);
    m.insert(uapi::KEY_M, Key::M);
    m.insert(uapi::KEY_COMMA, Key::Comma);
    m.insert(uapi::KEY_DOT, Key::Dot);
    m.insert(uapi::KEY_SLASH, Key::Slash);
    m.insert(uapi::KEY_RIGHTSHIFT, Key::RightShift);
    m.insert(uapi::KEY_KPASTERISK, Key::KeypadAsterisk);
    m.insert(uapi::KEY_LEFTALT, Key::LeftAlt);
    m.insert(uapi::KEY_SPACE, Key::Space);
    m.insert(uapi::KEY_CAPSLOCK, Key::CapsLock);
    m.insert(uapi::KEY_F1, Key::F1);
    m.insert(uapi::KEY_F2, Key::F2);
    m.insert(uapi::KEY_F3, Key::F3);
    m.insert(uapi::KEY_F4, Key::F4);
    m.insert(uapi::KEY_F5, Key::F5);
    m.insert(uapi::KEY_F6, Key::F6);
    m.insert(uapi::KEY_F7, Key::F7);
    m.insert(uapi::KEY_F8, Key::F8);
    m.insert(uapi::KEY_F9, Key::F9);
    m.insert(uapi::KEY_F10, Key::F10);
    m.insert(uapi::KEY_NUMLOCK, Key::NumLock);
    m.insert(uapi::KEY_SCROLLLOCK, Key::ScrollLock);
    m.insert(uapi::KEY_KP7, Key::Keypad7);
    m.insert(uapi::KEY_KP8, Key::Keypad8);
    m.insert(uapi::KEY_KP9, Key::Keypad9);
    m.insert(uapi::KEY_KPMINUS, Key::KeypadMinus);
    m.insert(uapi::KEY_KP4, Key::Keypad4);
    m.insert(uapi::KEY_KP5, Key::Keypad5);
    m.insert(uapi::KEY_KP6, Key::Keypad6);
    m.insert(uapi::KEY_KPPLUS, Key::KeypadPlus);
    m.insert(uapi::KEY_KP1, Key::Keypad1);
    m.insert(uapi::KEY_KP2, Key::Keypad2);
    m.insert(uapi::KEY_KP3, Key::Keypad3);
    m.insert(uapi::KEY_KP0, Key::Keypad0);
    m.insert(uapi::KEY_KPDOT, Key::KeypadDot);
    // Germany Keyboard layout.
    //
    // m.insert(uapi::KEY_ZENKAKUHANKAKU,);
    // m.insert(uapi::KEY_102ND,);
    m.insert(uapi::KEY_F11, Key::F11);
    m.insert(uapi::KEY_F12, Key::F12);
    // Japan Keyboard layout.
    //
    // m.insert(uapi::KEY_RO,);
    // m.insert(uapi::KEY_KATAKANA,);
    // m.insert(uapi::KEY_HIRAGANA,);
    // m.insert(uapi::KEY_HENKAN,);
    // m.insert(uapi::KEY_KATAKANAHIRAGANA,);
    // m.insert(uapi::KEY_MUHENKAN,);
    // m.insert(uapi::KEY_KPJPCOMMA,);
    m.insert(uapi::KEY_KPENTER, Key::KeypadEnter);
    m.insert(uapi::KEY_RIGHTCTRL, Key::RightCtrl);
    m.insert(uapi::KEY_KPSLASH, Key::KeypadSlash);
    // SYSRQ is "PrintScreen" Key located on the right of F12 on 104 keyboard.
    m.insert(uapi::KEY_SYSRQ, Key::PrintScreen);
    m.insert(uapi::KEY_RIGHTALT, Key::RightAlt);
    // m.insert(uapi::KEY_LINEFEED,);
    m.insert(uapi::KEY_HOME, Key::Home);
    m.insert(uapi::KEY_UP, Key::Up);
    m.insert(uapi::KEY_PAGEUP, Key::PageUp);
    m.insert(uapi::KEY_LEFT, Key::Left);
    m.insert(uapi::KEY_RIGHT, Key::Right);
    m.insert(uapi::KEY_END, Key::End);
    m.insert(uapi::KEY_DOWN, Key::Down);
    m.insert(uapi::KEY_PAGEDOWN, Key::PageDown);
    m.insert(uapi::KEY_INSERT, Key::Insert);
    m.insert(uapi::KEY_DELETE, Key::Delete);
    // m.insert(uapi::KEY_MACRO,);
    m.insert(uapi::KEY_MUTE, Key::Mute);
    m.insert(uapi::KEY_VOLUMEDOWN, Key::VolumeDown);
    m.insert(uapi::KEY_VOLUMEUP, Key::VolumeUp);
    m.insert(uapi::KEY_POWER, Key::Power);
    m.insert(uapi::KEY_KPEQUAL, Key::KeypadEquals);
    // m.insert(uapi::KEY_KPPLUSMINUS,);
    m.insert(uapi::KEY_PAUSE, Key::Pause);
    // m.insert(uapi::KEY_SCALE,);
    // m.insert(uapi::KEY_KPCOMMA,);
    //
    // Japan Keyboard layout.
    //
    // m.insert(uapi::KEY_HANGEUL,);
    // m.insert(uapi::KEY_HANGUEL,);
    // m.insert(uapi::KEY_HANJA,);
    // m.insert(uapi::KEY_YEN,);
    m.insert(uapi::KEY_LEFTMETA, Key::LeftMeta);
    m.insert(uapi::KEY_RIGHTMETA, Key::RightMeta);
    // m.insert(uapi::KEY_COMPOSE,);
    // m.insert(uapi::KEY_STOP,);
    // m.insert(uapi::KEY_AGAIN,);
    // m.insert(uapi::KEY_PROPS,);
    // m.insert(uapi::KEY_UNDO,);
    // m.insert(uapi::KEY_FRONT,);
    // m.insert(uapi::KEY_COPY,);
    // m.insert(uapi::KEY_OPEN,);
    // m.insert(uapi::KEY_PASTE,);
    // m.insert(uapi::KEY_FIND,);
    // m.insert(uapi::KEY_CUT,);
    // m.insert(uapi::KEY_HELP,);
    m.insert(uapi::KEY_MENU, Key::Menu);
    // m.insert(uapi::KEY_CALC,);
    // m.insert(uapi::KEY_SETUP,);
    // m.insert(uapi::KEY_SLEEP,);
    // m.insert(uapi::KEY_WAKEUP,);
    // m.insert(uapi::KEY_FILE,);
    // m.insert(uapi::KEY_SENDFILE,);
    // m.insert(uapi::KEY_DELETEFILE,);
    // m.insert(uapi::KEY_XFER,);
    // m.insert(uapi::KEY_PROG1,);
    // m.insert(uapi::KEY_PROG2,);
    // m.insert(uapi::KEY_WWW,);
    // m.insert(uapi::KEY_MSDOS,);
    // m.insert(uapi::KEY_COFFEE,);
    // m.insert(uapi::KEY_SCREENLOCK,);
    // m.insert(uapi::KEY_ROTATE_DISPLAY,);
    // m.insert(uapi::KEY_DIRECTION,);
    // m.insert(uapi::KEY_CYCLEWINDOWS,);
    // m.insert(uapi::KEY_MAIL,);
    // m.insert(uapi::KEY_BOOKMARKS,);
    // m.insert(uapi::KEY_COMPUTER,);
    // m.insert(uapi::KEY_BACK,);
    // m.insert(uapi::KEY_FORWARD,);
    // m.insert(uapi::KEY_CLOSECD,);
    // m.insert(uapi::KEY_EJECTCD,);
    // m.insert(uapi::KEY_EJECTCLOSECD,);
    // m.insert(uapi::KEY_NEXTSONG,);
    m.insert(uapi::KEY_PLAYPAUSE, Key::PlayPause);
    // m.insert(uapi::KEY_PREVIOUSSONG,);
    // m.insert(uapi::KEY_STOPCD,);
    // m.insert(uapi::KEY_RECORD,);
    // m.insert(uapi::KEY_REWIND,);
    // m.insert(uapi::KEY_PHONE,);
    // m.insert(uapi::KEY_ISO,);
    // m.insert(uapi::KEY_CONFIG,);
    // m.insert(uapi::KEY_HOMEPAGE,);
    // m.insert(uapi::KEY_REFRESH,);
    // m.insert(uapi::KEY_EXIT,);
    // m.insert(uapi::KEY_MOVE,);
    // m.insert(uapi::KEY_EDIT,);
    // m.insert(uapi::KEY_SCROLLUP,);
    // m.insert(uapi::KEY_SCROLLDOWN,);
    // m.insert(uapi::KEY_KPLEFTPAREN,);
    // m.insert(uapi::KEY_KPRIGHTPAREN,);
    // m.insert(uapi::KEY_NEW,);
    // m.insert(uapi::KEY_REDO,);
    // m.insert(uapi::KEY_F13,);
    // m.insert(uapi::KEY_F14,);
    // m.insert(uapi::KEY_F15,);
    // m.insert(uapi::KEY_F16,);
    // m.insert(uapi::KEY_F17,);
    // m.insert(uapi::KEY_F18,);
    // m.insert(uapi::KEY_F19,);
    // m.insert(uapi::KEY_F20,);
    // m.insert(uapi::KEY_F21,);
    // m.insert(uapi::KEY_F22,);
    // m.insert(uapi::KEY_F23,);
    // m.insert(uapi::KEY_F24,);
    // m.insert(uapi::KEY_PLAYCD,);
    // m.insert(uapi::KEY_PAUSECD,);
    // m.insert(uapi::KEY_PROG3,);
    // m.insert(uapi::KEY_PROG4,);
    // m.insert(uapi::KEY_ALL_APPLICATIONS,);
    // m.insert(uapi::KEY_DASHBOARD,);
    // m.insert(uapi::KEY_SUSPEND,);
    // m.insert(uapi::KEY_CLOSE,);
    // m.insert(uapi::KEY_PLAY,);
    // m.insert(uapi::KEY_FASTFORWARD,);
    // m.insert(uapi::KEY_BASSBOOST,);
    // m.insert(uapi::KEY_PRINT,);
    // m.insert(uapi::KEY_HP,);
    // m.insert(uapi::KEY_CAMERA,);
    // m.insert(uapi::KEY_SOUND,);
    // m.insert(uapi::KEY_QUESTION,);
    // m.insert(uapi::KEY_EMAIL,);
    // m.insert(uapi::KEY_CHAT,);
    // m.insert(uapi::KEY_SEARCH,);
    // m.insert(uapi::KEY_CONNECT,);
    // m.insert(uapi::KEY_FINANCE,);
    // m.insert(uapi::KEY_SPORT,);
    // m.insert(uapi::KEY_SHOP,);
    // m.insert(uapi::KEY_ALTERASE,);
    // m.insert(uapi::KEY_CANCEL,);
    m.insert(uapi::KEY_BRIGHTNESSDOWN, Key::BrightnessDown);
    m.insert(uapi::KEY_BRIGHTNESSUP, Key::BrightnessUp);
    // m.insert(uapi::KEY_MEDIA,);
    // m.insert(uapi::KEY_SWITCHVIDEOMODE,);
    // m.insert(uapi::KEY_KBDILLUMTOGGLE,);
    // m.insert(uapi::KEY_KBDILLUMDOWN,);
    // m.insert(uapi::KEY_KBDILLUMUP,);
    // m.insert(uapi::KEY_SEND,);
    // m.insert(uapi::KEY_REPLY,);
    // m.insert(uapi::KEY_FORWARDMAIL,);
    // m.insert(uapi::KEY_SAVE,);
    // m.insert(uapi::KEY_DOCUMENTS,);
    // m.insert(uapi::KEY_BATTERY,);
    // m.insert(uapi::KEY_BLUETOOTH,);
    // m.insert(uapi::KEY_WLAN,);
    // m.insert(uapi::KEY_UWB,);
    // m.insert(uapi::KEY_UNKNOWN,);
    // m.insert(uapi::KEY_VIDEO_NEXT,);
    // m.insert(uapi::KEY_VIDEO_PREV,);
    // m.insert(uapi::KEY_BRIGHTNESS_CYCLE,);
    // m.insert(uapi::KEY_BRIGHTNESS_AUTO,);
    // m.insert(uapi::KEY_BRIGHTNESS_ZERO,);
    // m.insert(uapi::KEY_DISPLAY_OFF,);
    // m.insert(uapi::KEY_WWAN,);
    // m.insert(uapi::KEY_WIMAX,);
    // m.insert(uapi::KEY_RFKILL,);
    // m.insert(uapi::KEY_MICMUTE,);
    // m.insert(uapi::KEY_OK,);
    // m.insert(uapi::KEY_SELECT,);
    // m.insert(uapi::KEY_GOTO,);
    // m.insert(uapi::KEY_CLEAR,);
    // m.insert(uapi::KEY_POWER2,);
    // m.insert(uapi::KEY_OPTION,);
    // m.insert(uapi::KEY_INFO,);
    // m.insert(uapi::KEY_TIME,);
    // m.insert(uapi::KEY_VENDOR,);
    // m.insert(uapi::KEY_ARCHIVE,);
    // m.insert(uapi::KEY_PROGRAM,);
    // m.insert(uapi::KEY_CHANNEL,);
    // m.insert(uapi::KEY_FAVORITES,);
    // m.insert(uapi::KEY_EPG,);
    // m.insert(uapi::KEY_PVR,);
    // m.insert(uapi::KEY_MHP,);
    // m.insert(uapi::KEY_LANGUAGE,);
    // m.insert(uapi::KEY_TITLE,);
    // m.insert(uapi::KEY_SUBTITLE,);
    // m.insert(uapi::KEY_ANGLE,);
    // m.insert(uapi::KEY_FULL_SCREEN,);
    // m.insert(uapi::KEY_ZOOM,);
    // m.insert(uapi::KEY_MODE,);
    // m.insert(uapi::KEY_KEYBOARD,);
    // m.insert(uapi::KEY_ASPECT_RATIO,);
    // m.insert(uapi::KEY_SCREEN,);
    // m.insert(uapi::KEY_PC,);
    // m.insert(uapi::KEY_TV,);
    // m.insert(uapi::KEY_TV2,);
    // m.insert(uapi::KEY_VCR,);
    // m.insert(uapi::KEY_VCR2,);
    // m.insert(uapi::KEY_SAT,);
    // m.insert(uapi::KEY_SAT2,);
    // m.insert(uapi::KEY_CD,);
    // m.insert(uapi::KEY_TAPE,);
    // m.insert(uapi::KEY_RADIO,);
    // m.insert(uapi::KEY_TUNER,);
    // m.insert(uapi::KEY_PLAYER,);
    // m.insert(uapi::KEY_TEXT,);
    // m.insert(uapi::KEY_DVD,);
    // m.insert(uapi::KEY_AUX,);
    // m.insert(uapi::KEY_MP3,);
    // m.insert(uapi::KEY_AUDIO,);
    // m.insert(uapi::KEY_VIDEO,);
    // m.insert(uapi::KEY_DIRECTORY,);
    // m.insert(uapi::KEY_LIST,);
    // m.insert(uapi::KEY_MEMO,);
    // m.insert(uapi::KEY_CALENDAR,);
    // m.insert(uapi::KEY_RED,);
    // m.insert(uapi::KEY_GREEN,);
    // m.insert(uapi::KEY_YELLOW,);
    // m.insert(uapi::KEY_BLUE,);
    // m.insert(uapi::KEY_CHANNELUP,);
    // m.insert(uapi::KEY_CHANNELDOWN,);
    // m.insert(uapi::KEY_FIRST,);
    // m.insert(uapi::KEY_LAST,);
    // m.insert(uapi::KEY_AB,);
    // m.insert(uapi::KEY_NEXT,);
    // m.insert(uapi::KEY_RESTART,);
    // m.insert(uapi::KEY_SLOW,);
    // m.insert(uapi::KEY_SHUFFLE,);
    // m.insert(uapi::KEY_BREAK,);
    // m.insert(uapi::KEY_PREVIOUS,);
    // m.insert(uapi::KEY_DIGITS,);
    // m.insert(uapi::KEY_TEEN,);
    // m.insert(uapi::KEY_TWEN,);
    // m.insert(uapi::KEY_VIDEOPHONE,);
    // m.insert(uapi::KEY_GAMES,);
    // m.insert(uapi::KEY_ZOOMIN,);
    // m.insert(uapi::KEY_ZOOMOUT,);
    // m.insert(uapi::KEY_ZOOMRESET,);
    // m.insert(uapi::KEY_WORDPROCESSOR,);
    // m.insert(uapi::KEY_EDITOR,);
    // m.insert(uapi::KEY_SPREADSHEET,);
    // m.insert(uapi::KEY_GRAPHICSEDITOR,);
    // m.insert(uapi::KEY_PRESENTATION,);
    // m.insert(uapi::KEY_DATABASE,);
    // m.insert(uapi::KEY_NEWS,);
    // m.insert(uapi::KEY_VOICEMAIL,);
    // m.insert(uapi::KEY_ADDRESSBOOK,);
    // m.insert(uapi::KEY_MESSENGER,);
    // m.insert(uapi::KEY_DISPLAYTOGGLE,);
    // m.insert(uapi::KEY_BRIGHTNESS_TOGGLE,);
    // m.insert(uapi::KEY_SPELLCHECK,);
    // m.insert(uapi::KEY_LOGOFF,);
    // m.insert(uapi::KEY_DOLLAR,);
    // m.insert(uapi::KEY_EURO,);
    // m.insert(uapi::KEY_FRAMEBACK,);
    // m.insert(uapi::KEY_FRAMEFORWARD,);
    // m.insert(uapi::KEY_CONTEXT_MENU,);
    // m.insert(uapi::KEY_MEDIA_REPEAT,);
    // m.insert(uapi::KEY_10CHANNELSUP,);
    // m.insert(uapi::KEY_10CHANNELSDOWN,);
    // m.insert(uapi::KEY_IMAGES,);
    // m.insert(uapi::KEY_NOTIFICATION_CENTER,);
    // m.insert(uapi::KEY_PICKUP_PHONE,);
    // m.insert(uapi::KEY_HANGUP_PHONE,);
    // m.insert(uapi::KEY_DEL_EOL,);
    // m.insert(uapi::KEY_DEL_EOS,);
    // m.insert(uapi::KEY_INS_LINE,);
    // m.insert(uapi::KEY_DEL_LINE,);
    // m.insert(uapi::KEY_FN,);
    // m.insert(uapi::KEY_FN_ESC,);
    // m.insert(uapi::KEY_FN_F1,);
    // m.insert(uapi::KEY_FN_F2,);
    // m.insert(uapi::KEY_FN_F3,);
    // m.insert(uapi::KEY_FN_F4,);
    // m.insert(uapi::KEY_FN_F5,);
    // m.insert(uapi::KEY_FN_F6,);
    // m.insert(uapi::KEY_FN_F7,);
    // m.insert(uapi::KEY_FN_F8,);
    // m.insert(uapi::KEY_FN_F9,);
    // m.insert(uapi::KEY_FN_F10,);
    // m.insert(uapi::KEY_FN_F11,);
    // m.insert(uapi::KEY_FN_F12,);
    // m.insert(uapi::KEY_FN_1,);
    // m.insert(uapi::KEY_FN_2,);
    // m.insert(uapi::KEY_FN_D,);
    // m.insert(uapi::KEY_FN_E,);
    // m.insert(uapi::KEY_FN_F,);
    // m.insert(uapi::KEY_FN_S,);
    // m.insert(uapi::KEY_FN_B,);
    // m.insert(uapi::KEY_FN_RIGHT_SHIFT,);
    // m.insert(uapi::KEY_BRL_DOT1,);
    // m.insert(uapi::KEY_BRL_DOT2,);
    // m.insert(uapi::KEY_BRL_DOT3,);
    // m.insert(uapi::KEY_BRL_DOT4,);
    // m.insert(uapi::KEY_BRL_DOT5,);
    // m.insert(uapi::KEY_BRL_DOT6,);
    // m.insert(uapi::KEY_BRL_DOT7,);
    // m.insert(uapi::KEY_BRL_DOT8,);
    // m.insert(uapi::KEY_BRL_DOT9,);
    // m.insert(uapi::KEY_BRL_DOT10,);
    // m.insert(uapi::KEY_NUMERIC_0,);
    // m.insert(uapi::KEY_NUMERIC_1,);
    // m.insert(uapi::KEY_NUMERIC_2,);
    // m.insert(uapi::KEY_NUMERIC_3,);
    // m.insert(uapi::KEY_NUMERIC_4,);
    // m.insert(uapi::KEY_NUMERIC_5,);
    // m.insert(uapi::KEY_NUMERIC_6,);
    // m.insert(uapi::KEY_NUMERIC_7,);
    // m.insert(uapi::KEY_NUMERIC_8,);
    // m.insert(uapi::KEY_NUMERIC_9,);
    // m.insert(uapi::KEY_NUMERIC_STAR,);
    // m.insert(uapi::KEY_NUMERIC_POUND,);
    // m.insert(uapi::KEY_NUMERIC_A,);
    // m.insert(uapi::KEY_NUMERIC_B,);
    // m.insert(uapi::KEY_NUMERIC_C,);
    // m.insert(uapi::KEY_NUMERIC_D,);
    // m.insert(uapi::KEY_CAMERA_FOCUS,);
    // m.insert(uapi::KEY_WPS_BUTTON,);
    // m.insert(uapi::KEY_TOUCHPAD_TOGGLE,);
    // m.insert(uapi::KEY_TOUCHPAD_ON,);
    // m.insert(uapi::KEY_TOUCHPAD_OFF,);
    // m.insert(uapi::KEY_CAMERA_ZOOMIN,);
    // m.insert(uapi::KEY_CAMERA_ZOOMOUT,);
    // m.insert(uapi::KEY_CAMERA_UP,);
    // m.insert(uapi::KEY_CAMERA_DOWN,);
    // m.insert(uapi::KEY_CAMERA_LEFT,);
    // m.insert(uapi::KEY_CAMERA_RIGHT,);
    // m.insert(uapi::KEY_ATTENDANT_ON,);
    // m.insert(uapi::KEY_ATTENDANT_OFF,);
    // m.insert(uapi::KEY_ATTENDANT_TOGGLE,);
    // m.insert(uapi::KEY_LIGHTS_TOGGLE,);
    // m.insert(uapi::KEY_ALS_TOGGLE,);
    // m.insert(uapi::KEY_ROTATE_LOCK_TOGGLE,);
    // m.insert(uapi::KEY_BUTTONCONFIG,);
    // m.insert(uapi::KEY_TASKMANAGER,);
    // m.insert(uapi::KEY_JOURNAL,);
    // m.insert(uapi::KEY_CONTROLPANEL,);
    // m.insert(uapi::KEY_APPSELECT,);
    // m.insert(uapi::KEY_SCREENSAVER,);
    // m.insert(uapi::KEY_VOICECOMMAND,);
    // m.insert(uapi::KEY_ASSISTANT,);
    // m.insert(uapi::KEY_KBD_LAYOUT_NEXT,);
    // m.insert(uapi::KEY_EMOJI_PICKER,);
    // m.insert(uapi::KEY_DICTATE,);
    // m.insert(uapi::KEY_CAMERA_ACCESS_ENABLE,);
    // m.insert(uapi::KEY_CAMERA_ACCESS_DISABLE,);
    // m.insert(uapi::KEY_CAMERA_ACCESS_TOGGLE,);
    // m.insert(uapi::KEY_BRIGHTNESS_MIN,);
    // m.insert(uapi::KEY_BRIGHTNESS_MAX,);
    // m.insert(uapi::KEY_KBDINPUTASSIST_PREV,);
    // m.insert(uapi::KEY_KBDINPUTASSIST_NEXT,);
    // m.insert(uapi::KEY_KBDINPUTASSIST_PREVGROUP,);
    // m.insert(uapi::KEY_KBDINPUTASSIST_NEXTGROUP,);
    // m.insert(uapi::KEY_KBDINPUTASSIST_ACCEPT,);
    // m.insert(uapi::KEY_KBDINPUTASSIST_CANCEL,);
    // m.insert(uapi::KEY_RIGHT_UP,);
    // m.insert(uapi::KEY_RIGHT_DOWN,);
    // m.insert(uapi::KEY_LEFT_UP,);
    // m.insert(uapi::KEY_LEFT_DOWN,);
    // m.insert(uapi::KEY_ROOT_MENU,);
    // m.insert(uapi::KEY_MEDIA_TOP_MENU,);
    // m.insert(uapi::KEY_NUMERIC_11,);
    // m.insert(uapi::KEY_NUMERIC_12,);
    // m.insert(uapi::KEY_AUDIO_DESC,);
    // m.insert(uapi::KEY_3D_MODE,);
    // m.insert(uapi::KEY_NEXT_FAVORITE,);
    // m.insert(uapi::KEY_STOP_RECORD,);
    // m.insert(uapi::KEY_PAUSE_RECORD,);
    // m.insert(uapi::KEY_VOD,);
    // m.insert(uapi::KEY_UNMUTE,);
    // m.insert(uapi::KEY_FASTREVERSE,);
    // m.insert(uapi::KEY_SLOWREVERSE,);
    // m.insert(uapi::KEY_DATA,);
    // m.insert(uapi::KEY_ONSCREEN_KEYBOARD,);
    // m.insert(uapi::KEY_PRIVACY_SCREEN_TOGGLE,);
    // m.insert(uapi::KEY_SELECTIVE_SCREENSHOT,);
    // m.insert(uapi::KEY_NEXT_ELEMENT,);
    // m.insert(uapi::KEY_PREVIOUS_ELEMENT,);
    // m.insert(uapi::KEY_AUTOPILOT_ENGAGE_TOGGLE,);
    // m.insert(uapi::KEY_MARK_WAYPOINT,);
    // m.insert(uapi::KEY_SOS,);
    // m.insert(uapi::KEY_NAV_CHART,);
    // m.insert(uapi::KEY_FISHING_CHART,);
    // m.insert(uapi::KEY_SINGLE_RANGE_RADAR,);
    // m.insert(uapi::KEY_DUAL_RANGE_RADAR,);
    // m.insert(uapi::KEY_RADAR_OVERLAY,);
    // m.insert(uapi::KEY_TRADITIONAL_SONAR,);
    // m.insert(uapi::KEY_CLEARVU_SONAR,);
    // m.insert(uapi::KEY_SIDEVU_SONAR,);
    // m.insert(uapi::KEY_NAV_INFO,);
    // m.insert(uapi::KEY_BRIGHTNESS_MENU,);
    // m.insert(uapi::KEY_MACRO1,);
    // m.insert(uapi::KEY_MACRO2,);
    // m.insert(uapi::KEY_MACRO3,);
    // m.insert(uapi::KEY_MACRO4,);
    // m.insert(uapi::KEY_MACRO5,);
    // m.insert(uapi::KEY_MACRO6,);
    // m.insert(uapi::KEY_MACRO7,);
    // m.insert(uapi::KEY_MACRO8,);
    // m.insert(uapi::KEY_MACRO9,);
    // m.insert(uapi::KEY_MACRO10,);
    // m.insert(uapi::KEY_MACRO11,);
    // m.insert(uapi::KEY_MACRO12,);
    // m.insert(uapi::KEY_MACRO13,);
    // m.insert(uapi::KEY_MACRO14,);
    // m.insert(uapi::KEY_MACRO15,);
    // m.insert(uapi::KEY_MACRO16,);
    // m.insert(uapi::KEY_MACRO17,);
    // m.insert(uapi::KEY_MACRO18,);
    // m.insert(uapi::KEY_MACRO19,);
    // m.insert(uapi::KEY_MACRO20,);
    // m.insert(uapi::KEY_MACRO21,);
    // m.insert(uapi::KEY_MACRO22,);
    // m.insert(uapi::KEY_MACRO23,);
    // m.insert(uapi::KEY_MACRO24,);
    // m.insert(uapi::KEY_MACRO25,);
    // m.insert(uapi::KEY_MACRO26,);
    // m.insert(uapi::KEY_MACRO27,);
    // m.insert(uapi::KEY_MACRO28,);
    // m.insert(uapi::KEY_MACRO29,);
    // m.insert(uapi::KEY_MACRO30,);
    // m.insert(uapi::KEY_MACRO_RECORD_START,);
    // m.insert(uapi::KEY_MACRO_RECORD_STOP,);
    // m.insert(uapi::KEY_MACRO_PRESET_CYCLE,);
    // m.insert(uapi::KEY_MACRO_PRESET1,);
    // m.insert(uapi::KEY_MACRO_PRESET2,);
    // m.insert(uapi::KEY_MACRO_PRESET3,);
    // m.insert(uapi::KEY_KBD_LCD_MENU1,);
    // m.insert(uapi::KEY_KBD_LCD_MENU2,);
    // m.insert(uapi::KEY_KBD_LCD_MENU3,);
    // m.insert(uapi::KEY_KBD_LCD_MENU4,);
    // m.insert(uapi::KEY_KBD_LCD_MENU5,);

    // we use following keycodes in starnix tests. See b/311425670 for details.
    m.insert(0x0055, Key::Unknown0055);
    m.insert(0x0056, Key::Unknown0056);
    m.insert(0x0059, Key::Unknown0059);
    m.insert(0x005c, Key::Unknown005C);
    m.insert(0x005d, Key::Unknown005D);
    m.insert(0x005e, Key::Unknown005E);
    m.insert(0x0079, Key::Unknown0079);
    m.insert(0x007a, Key::Unknown007A);
    m.insert(0x007b, Key::Unknown007B);
    m.insert(0x007c, Key::Unknown007C);
    m.insert(0x0085, Key::Unknown0085);
    m.insert(0x0087, Key::Unknown0087);
    m.insert(0x0089, Key::Unknown0089);
    m.insert(0x009c, Key::Unknown009C);
    m.insert(0x009f, Key::Unknown009F);
    m.insert(0x00a0, Key::Unknown00A0);
    m.insert(0x00a2, Key::Unknown00A2);
    m.insert(0x00a3, Key::Unknown00A3);
    m.insert(0x00a5, Key::Unknown00A5);
    m.insert(0x00a6, Key::Unknown00A6);
    m.insert(0x00a7, Key::Unknown00A7);
    m.insert(0x00a8, Key::Unknown00A8);
    m.insert(0x00a9, Key::Unknown00A9);
    m.insert(0x00ad, Key::Unknown00Ad);
    m.insert(0x00b1, Key::Unknown00B1);
    m.insert(0x00b2, Key::Unknown00B2);
    m.insert(0x00b3, Key::Unknown00B3);
    m.insert(0x00b4, Key::Unknown00B4);
    m.insert(0x00c9, Key::Unknown00C9);
    m.insert(0x00cf, Key::Unknown00Cf);
    m.insert(0x00d0, Key::Unknown00D0);
    m.insert(0x00d4, Key::Unknown00D4);
    m.insert(0x00e2, Key::Unknown00E2);
    m.insert(0x0120, Key::Unknown0120);
    m.insert(0x0121, Key::Unknown0121);
    m.insert(0x0122, Key::Unknown0122);
    m.insert(0x0123, Key::Unknown0123);
    m.insert(0x0124, Key::Unknown0124);
    m.insert(0x0125, Key::Unknown0125);
    m.insert(0x0126, Key::Unknown0126);
    m.insert(0x0127, Key::Unknown0127);
    m.insert(0x0128, Key::Unknown0128);
    m.insert(0x0129, Key::Unknown0129);
    m.insert(0x012a, Key::Unknown012A);
    m.insert(0x012b, Key::Unknown012B);
    m.insert(0x012c, Key::Unknown012C);
    m.insert(0x012d, Key::Unknown012D);
    m.insert(0x012e, Key::Unknown012E);
    m.insert(0x012f, Key::Unknown012F);
    m.insert(0x0130, Key::Unknown0130);
    m.insert(0x0131, Key::Unknown0131);
    m.insert(0x0132, Key::Unknown0132);
    m.insert(0x0133, Key::Unknown0133);
    m.insert(0x0134, Key::Unknown0134);
    m.insert(0x0135, Key::Unknown0135);
    m.insert(0x0136, Key::Unknown0136);
    m.insert(0x0137, Key::Unknown0137);
    m.insert(0x0138, Key::Unknown0138);
    m.insert(0x0139, Key::Unknown0139);
    m.insert(0x013a, Key::Unknown013A);
    m.insert(0x013b, Key::Unknown013B);
    m.insert(0x013c, Key::Unknown013C);
    m.insert(0x013d, Key::Unknown013D);
    m.insert(0x013e, Key::Unknown013E);
    m.insert(0x0161, Key::Unknown0161);
    m.insert(0x016a, Key::Unknown016A);
    m.insert(0x016e, Key::Unknown016E);
    m.insert(0x0172, Key::Unknown0172);
    m.insert(0x0179, Key::Unknown0179);
    m.insert(0x018e, Key::Unknown018E);
    m.insert(0x018f, Key::Unknown018F);
    m.insert(0x0190, Key::Unknown0190);
    m.insert(0x0191, Key::Unknown0191);
    m.insert(0x0192, Key::Unknown0192);
    m.insert(0x0193, Key::Unknown0193);
    m.insert(0x0195, Key::Unknown0195);
    m.insert(0x01d0, Key::Unknown01D0);
    m.insert(0x020a, Key::Unknown020A);
    m.insert(0x020b, Key::Unknown020B);

    m
}

pub struct LinuxButtonEventBatch {
    pub events: Vec<uapi::input_event>,

    // Because FIDL button events do not carry a timestamp, we perform a direct
    // clock read during conversion and assign this value as the timestamp for
    // all generated Linux button events in a single batch.
    pub event_time: zx::MonotonicInstant,
    pub power_is_pressed: bool,
    pub function_is_pressed: bool,
}

impl LinuxButtonEventBatch {
    pub fn new() -> Self {
        Self {
            events: vec![],
            event_time: zx::MonotonicInstant::get(),
            power_is_pressed: false,
            function_is_pressed: false,
        }
    }
}

pub fn parse_fidl_button_event(
    fidl_event: &MediaButtonsEvent,
    power_was_pressed: bool,
    function_was_pressed: bool,
) -> LinuxButtonEventBatch {
    let mut batch = LinuxButtonEventBatch::new();
    let time = timeval_from_time(batch.event_time);
    let sync_event = uapi::input_event {
        // See https://www.kernel.org/doc/Documentation/input/event-codes.rst.
        time,
        type_: uapi::EV_SYN as u16,
        code: uapi::SYN_REPORT as u16,
        value: 0,
    };

    batch.power_is_pressed = fidl_event.power.unwrap_or(false);
    batch.function_is_pressed = fidl_event.function.unwrap_or(false);
    for (then, now, key_code) in [
        (power_was_pressed, batch.power_is_pressed, uapi::KEY_POWER),
        (function_was_pressed, batch.function_is_pressed, uapi::KEY_SCREENSAVER),
    ] {
        // Button state changed. Send an event.
        if then != now {
            batch.events.push(uapi::input_event {
                time,
                type_: uapi::EV_KEY as u16,
                code: key_code as u16,
                value: now as i32,
            });
            batch.events.push(sync_event);
        }
    }

    batch
}

#[cfg(test)]
mod touchscreen_linux_fuchsia_tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use test_case::test_case;
    use uapi::timeval;

    fn input_event(ty: u32, code: u32, value: i32) -> uapi::input_event {
        uapi::input_event { time: timeval::default(), type_: ty as u16, code: code as u16, value }
    }

    #[test]
    fn handle_btn_touch_ok_does_not_produce_input_report() {
        let e = input_event(uapi::EV_KEY, uapi::BTN_TOUCH, 1);
        let mut parser = LinuxTouchEventParser::create();
        assert_eq!(parser.handle(e), Ok(None));
        assert_eq!(
            parser,
            LinuxTouchEventParser {
                cached_events: vec![],
                slot_id_to_tracking_id: HashMap::new(),
                ..LinuxTouchEventParser::default()
            }
        );
    }

    #[test_case(input_event(uapi::EV_ABS, uapi::ABS_MT_SLOT, 1); "ABS_MT_SLOT")]
    #[test_case(input_event(uapi::EV_ABS, uapi::ABS_MT_TRACKING_ID, 1); "ABS_MT_TRACKING_ID")]
    #[test_case(input_event(uapi::EV_ABS, uapi::ABS_MT_POSITION_X, 1); "ABS_MT_POSITION_X")]
    #[test_case(input_event(uapi::EV_ABS, uapi::ABS_MT_POSITION_Y, 1); "ABS_MT_POSITION_Y")]
    fn handle_input_event_ok_does_not_produce_input_report(e: uapi::input_event) {
        let mut parser = LinuxTouchEventParser::create();
        pretty_assertions::assert_eq!(parser.handle(e), Ok(None));
        pretty_assertions::assert_eq!(
            parser,
            LinuxTouchEventParser {
                cached_events: vec![e],
                slot_id_to_tracking_id: HashMap::new(),
                ..LinuxTouchEventParser::default()
            }
        );
    }

    #[test_case(input_event(uapi::EV_KEY, uapi::KEY_A, 1); "unsupported keycode")]
    #[test_case(input_event(uapi::EV_ABS, uapi::ABS_PRESSURE, 1); "unsupported ABS event")]
    #[test_case(input_event(uapi::EV_SYN, uapi::SYN_MT_REPORT, 1); "Type A")]
    #[test_case(input_event(uapi::EV_SYN, uapi::SYN_CONFIG, 1); "unsupported SYN event")]
    fn handle_input_event_error(e: uapi::input_event) {
        let mut parser = LinuxTouchEventParser::create();
        pretty_assertions::assert_eq!(parser.handle(e), error!(EINVAL));
        pretty_assertions::assert_eq!(
            parser,
            LinuxTouchEventParser {
                cached_events: vec![],
                slot_id_to_tracking_id: HashMap::new(),
                ..LinuxTouchEventParser::default()
            }
        );
    }

    #[test_case(input_event(uapi::EV_ABS, uapi::ABS_MT_TRACKING_ID, 1); "ABS_MT_TRACKING_ID")]
    #[test_case(input_event(uapi::EV_ABS, uapi::ABS_MT_POSITION_X, 1); "ABS_MT_POSITION_X")]
    #[test_case(input_event(uapi::EV_ABS, uapi::ABS_MT_POSITION_Y, 1); "ABS_MT_POSITION_Y")]
    fn no_slot_leading_event(e: uapi::input_event) {
        let syn = input_event(uapi::EV_SYN, uapi::SYN_REPORT, 0);

        let mut parser = LinuxTouchEventParser::create();
        pretty_assertions::assert_eq!(parser.handle(e), Ok(None));
        pretty_assertions::assert_eq!(parser.handle(syn), error!(EINVAL));
        pretty_assertions::assert_eq!(
            parser,
            LinuxTouchEventParser {
                cached_events: vec![],
                slot_id_to_tracking_id: HashMap::new(),
                ..LinuxTouchEventParser::default()
            }
        );
    }

    #[test]
    fn slot_does_not_have_enough_information() {
        let slot_0 = input_event(uapi::EV_ABS, uapi::ABS_MT_SLOT, 0);
        let syn = input_event(uapi::EV_SYN, uapi::SYN_REPORT, 0);

        let mut parser = LinuxTouchEventParser::create();

        // The last slot does not have enough information.
        assert_eq!(parser.handle(slot_0), Ok(None));
        assert_eq!(parser.handle(syn), error!(EINVAL));
        assert_eq!(
            parser,
            LinuxTouchEventParser {
                cached_events: vec![],
                slot_id_to_tracking_id: HashMap::new(),
                ..LinuxTouchEventParser::default()
            }
        );

        // The first slot does not have enough information.
        let slot_1 = input_event(uapi::EV_ABS, uapi::ABS_MT_SLOT, 1);
        assert_eq!(parser.handle(slot_0), Ok(None));
        assert_eq!(parser.handle(slot_1), Ok(None));
        assert_eq!(parser.handle(syn), error!(EINVAL));
        assert_eq!(
            parser,
            LinuxTouchEventParser {
                cached_events: vec![],
                slot_id_to_tracking_id: HashMap::new(),
                ..LinuxTouchEventParser::default()
            }
        );
    }

    #[test]
    fn same_slot_id_in_one_event() {
        let slot_0 = input_event(uapi::EV_ABS, uapi::ABS_MT_SLOT, 0);
        let traking_id = input_event(uapi::EV_ABS, uapi::ABS_MT_TRACKING_ID, 0);
        let x = input_event(uapi::EV_ABS, uapi::ABS_MT_POSITION_X, 0);
        let y = input_event(uapi::EV_ABS, uapi::ABS_MT_POSITION_Y, 0);
        let syn = input_event(uapi::EV_SYN, uapi::SYN_REPORT, 0);

        let mut parser = LinuxTouchEventParser::create();
        assert_eq!(parser.handle(slot_0), Ok(None));
        assert_eq!(parser.handle(traking_id), Ok(None));
        assert_eq!(parser.handle(x), Ok(None));
        assert_eq!(parser.handle(y), Ok(None));
        assert_eq!(parser.handle(slot_0), Ok(None));
        assert_eq!(parser.handle(syn), error!(EINVAL));
        assert_eq!(
            parser,
            LinuxTouchEventParser {
                cached_events: vec![],
                slot_id_to_tracking_id: HashMap::new(),
                ..LinuxTouchEventParser::default()
            }
        );
    }

    #[test]
    fn tracking_id_changed_in_slot() {
        let slot_0 = input_event(uapi::EV_ABS, uapi::ABS_MT_SLOT, 0);
        let traking_id_0 = input_event(uapi::EV_ABS, uapi::ABS_MT_TRACKING_ID, 0);
        let traking_id_1 = input_event(uapi::EV_ABS, uapi::ABS_MT_TRACKING_ID, 1);
        let syn = input_event(uapi::EV_SYN, uapi::SYN_REPORT, 0);

        let mut parser = LinuxTouchEventParser::create();
        assert_eq!(parser.handle(slot_0), Ok(None));
        assert_eq!(parser.handle(traking_id_0), Ok(None));
        assert_eq!(parser.handle(traking_id_1), Ok(None));
        assert_eq!(parser.handle(syn), error!(EINVAL));
        assert_eq!(
            parser,
            LinuxTouchEventParser {
                cached_events: vec![],
                slot_id_to_tracking_id: HashMap::new(),
                ..LinuxTouchEventParser::default()
            }
        );
    }

    #[test]
    fn tracking_id_different_with_parser_recorded() {
        let slot_0 = input_event(uapi::EV_ABS, uapi::ABS_MT_SLOT, 0);
        let traking_id_1 = input_event(uapi::EV_ABS, uapi::ABS_MT_TRACKING_ID, 1);
        let syn = input_event(uapi::EV_SYN, uapi::SYN_REPORT, 0);

        let mut parser = LinuxTouchEventParser::create();
        parser.slot_id_to_tracking_id.insert(0, 0);
        assert_eq!(parser.handle(slot_0), Ok(None));
        assert_eq!(parser.handle(traking_id_1), Ok(None));
        assert_eq!(parser.handle(syn), error!(EINVAL));
        assert_eq!(
            parser,
            LinuxTouchEventParser {
                cached_events: vec![],
                slot_id_to_tracking_id: HashMap::new(),
                ..LinuxTouchEventParser::default()
            }
        );
    }

    #[test]
    fn produce_input_report() {
        // 1 contact.
        let slot_0 = input_event(uapi::EV_ABS, uapi::ABS_MT_SLOT, 0);
        let traking_id_0 = input_event(uapi::EV_ABS, uapi::ABS_MT_TRACKING_ID, 1);
        let x_0 = input_event(uapi::EV_ABS, uapi::ABS_MT_POSITION_X, 2);
        let y_0 = input_event(uapi::EV_ABS, uapi::ABS_MT_POSITION_Y, 3);
        let syn = input_event(uapi::EV_SYN, uapi::SYN_REPORT, 0);

        let mut parser = LinuxTouchEventParser::create();
        assert_eq!(parser.handle(slot_0), Ok(None));
        assert_eq!(parser.handle(traking_id_0), Ok(None));
        assert_eq!(parser.handle(x_0), Ok(None));
        assert_eq!(parser.handle(y_0), Ok(None));
        assert_eq!(
            parser.handle(syn),
            Ok(Some(fir::InputReport {
                event_time: Some(0),
                touch: Some(fir::TouchInputReport {
                    contacts: Some(vec![fir::ContactInputReport {
                        contact_id: Some(1),
                        position_x: Some(2),
                        position_y: Some(3),
                        ..fir::ContactInputReport::default()
                    },]),
                    ..fir::TouchInputReport::default()
                }),
                ..fir::InputReport::default()
            }))
        );
        assert_eq!(
            parser,
            LinuxTouchEventParser {
                cached_events: vec![],
                slot_id_to_tracking_id: HashMap::from([(0, 1)]),
                ..LinuxTouchEventParser::default()
            }
        );

        // 2 contact.
        let x_0 = input_event(uapi::EV_ABS, uapi::ABS_MT_POSITION_X, 4);
        let y_0 = input_event(uapi::EV_ABS, uapi::ABS_MT_POSITION_Y, 5);

        let slot_1 = input_event(uapi::EV_ABS, uapi::ABS_MT_SLOT, 1);
        let traking_id_1 = input_event(uapi::EV_ABS, uapi::ABS_MT_TRACKING_ID, 2);
        let x_1 = input_event(uapi::EV_ABS, uapi::ABS_MT_POSITION_X, 10);
        let y_1 = input_event(uapi::EV_ABS, uapi::ABS_MT_POSITION_Y, 11);

        assert_eq!(parser.handle(slot_0), Ok(None));
        assert_eq!(parser.handle(x_0), Ok(None));
        assert_eq!(parser.handle(y_0), Ok(None));
        assert_eq!(parser.handle(slot_1), Ok(None));
        assert_eq!(parser.handle(traking_id_1), Ok(None));
        assert_eq!(parser.handle(x_1), Ok(None));
        assert_eq!(parser.handle(y_1), Ok(None));
        assert_eq!(
            parser.handle(syn),
            Ok(Some(fir::InputReport {
                event_time: Some(0),
                touch: Some(fir::TouchInputReport {
                    contacts: Some(vec![
                        fir::ContactInputReport {
                            contact_id: Some(1),
                            position_x: Some(4),
                            position_y: Some(5),
                            ..fir::ContactInputReport::default()
                        },
                        fir::ContactInputReport {
                            contact_id: Some(2),
                            position_x: Some(10),
                            position_y: Some(11),
                            ..fir::ContactInputReport::default()
                        },
                    ]),
                    ..fir::TouchInputReport::default()
                }),
                ..fir::InputReport::default()
            }))
        );
        assert_eq!(
            parser,
            LinuxTouchEventParser {
                cached_events: vec![],
                slot_id_to_tracking_id: HashMap::from([(0, 1), (1, 2)]),
                ..LinuxTouchEventParser::default()
            }
        );

        // lift the first contact.
        let tracking_id_lifted = input_event(uapi::EV_ABS, uapi::ABS_MT_TRACKING_ID, -1);

        assert_eq!(parser.handle(slot_0), Ok(None));
        assert_eq!(parser.handle(tracking_id_lifted), Ok(None));
        assert_eq!(parser.handle(slot_1), Ok(None));
        assert_eq!(parser.handle(x_1), Ok(None));
        assert_eq!(parser.handle(y_1), Ok(None));
        assert_eq!(
            parser.handle(syn),
            Ok(Some(fir::InputReport {
                event_time: Some(0),
                touch: Some(fir::TouchInputReport {
                    contacts: Some(vec![fir::ContactInputReport {
                        contact_id: Some(2),
                        position_x: Some(10),
                        position_y: Some(11),
                        ..fir::ContactInputReport::default()
                    },]),
                    ..fir::TouchInputReport::default()
                }),
                ..fir::InputReport::default()
            }))
        );
        // should remove the mapping.
        assert_eq!(
            parser,
            LinuxTouchEventParser {
                cached_events: vec![],
                slot_id_to_tracking_id: HashMap::from([(1, 2)]),
                ..LinuxTouchEventParser::default()
            }
        );

        // lift all contact.
        assert_eq!(parser.handle(slot_1), Ok(None));
        assert_eq!(parser.handle(tracking_id_lifted), Ok(None));
        assert_eq!(
            parser.handle(syn),
            Ok(Some(fir::InputReport {
                event_time: Some(0),
                touch: Some(fir::TouchInputReport {
                    contacts: Some(vec![]),
                    ..fir::TouchInputReport::default()
                }),
                ..fir::InputReport::default()
            }))
        );
        // should remove the mapping.
        assert_eq!(
            parser,
            LinuxTouchEventParser {
                cached_events: vec![],
                slot_id_to_tracking_id: HashMap::new(),
                ..LinuxTouchEventParser::default()
            }
        );
    }
}

#[cfg(test)]
mod touchscreen_fuchsia_linux_tests {
    use super::*;
    use fidl_fuchsia_ui_pointer::TouchInteractionId;
    use pretty_assertions::assert_eq;
    use test_case::test_case;

    fn make_touch_event_with_coords_phase_id_time(
        x: f32,
        y: f32,
        phase: FidlEventPhase,
        pointer_id: u32,
        time_nanos: i64,
    ) -> FidlTouchEvent {
        FidlTouchEvent {
            timestamp: Some(time_nanos),
            pointer_sample: Some(TouchPointerSample {
                position_in_viewport: Some([x, y]),
                phase: Some(phase),
                interaction: Some(TouchInteractionId {
                    pointer_id,
                    device_id: 0,
                    interaction_id: 0,
                }),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    fn make_touch_event_with_coords_phase_id(
        x: f32,
        y: f32,
        phase: FidlEventPhase,
        pointer_id: u32,
    ) -> FidlTouchEvent {
        make_touch_event_with_coords_phase_id_time(x, y, phase, pointer_id, 0)
    }

    fn make_uapi_input_event_with_time(
        ty: u32,
        code: u32,
        value: i32,
        time_nanos: i64,
    ) -> uapi::input_event {
        uapi::input_event {
            time: timeval_from_time(zx::MonotonicInstant::from_nanos(time_nanos)),
            type_: ty as u16,
            code: code as u16,
            value,
        }
    }

    fn make_uapi_input_event(ty: u32, code: u32, value: i32) -> uapi::input_event {
        make_uapi_input_event_with_time(ty, code, value, 0)
    }

    #[test_case(FidlTouchEvent::default(); "not enough fields")]
    fn ignored_events(e: FidlTouchEvent) {
        let mut converter = FuchsiaTouchEventToLinuxTouchEventConverter::create();
        let _ = converter.handle(vec![make_touch_event_with_coords_phase_id(
            10.0,
            20.0,
            FidlEventPhase::Add,
            1,
        )]);
        let batch = converter.handle(vec![e]);
        assert_eq!(batch.events, vec![]);
        assert_eq!(batch.last_event_time_ns, 0);
        assert_eq!(batch.count_converted_fidl_events, 0);
        assert_eq!(batch.count_ignored_fidl_events, 1);
        assert_eq!(batch.count_unexpected_fidl_events, 0);
    }

    #[test_case(make_touch_event_with_coords_phase_id(
        1.0,
        2.0,
        FidlEventPhase::Add,
        1,
    ); "touch add pointer already added")]
    #[test_case(make_touch_event_with_coords_phase_id(
        1.0,
        2.0,
        FidlEventPhase::Change,
        2,
    ); "touch change pointer not added")]
    #[test_case(make_touch_event_with_coords_phase_id(
        0.0,
        0.0,
        FidlEventPhase::Remove,
        2,
    ); "touch remove pointer not added")]
    #[test_case(make_touch_event_with_coords_phase_id(
        0.0,
        0.0,
        FidlEventPhase::Cancel,
        1,
    ); "touch cancel")]
    fn unexpected_events(e: FidlTouchEvent) {
        let mut converter = FuchsiaTouchEventToLinuxTouchEventConverter::create();
        let _ = converter.handle(vec![make_touch_event_with_coords_phase_id(
            10.0,
            20.0,
            FidlEventPhase::Add,
            1,
        )]);
        let batch = converter.handle(vec![e]);
        assert_eq!(batch.events, vec![]);
        assert_eq!(batch.last_event_time_ns, 0);
        assert_eq!(batch.count_converted_fidl_events, 0);
        assert_eq!(batch.count_ignored_fidl_events, 0);
        assert_eq!(batch.count_unexpected_fidl_events, 1);
    }

    #[test]
    fn touch_add() {
        let mut converter = FuchsiaTouchEventToLinuxTouchEventConverter::create();
        let batch = converter.handle(vec![make_touch_event_with_coords_phase_id(
            10.0,
            20.0,
            FidlEventPhase::Add,
            1,
        )]);

        assert_eq!(
            batch.events,
            vec![
                make_uapi_input_event(uapi::EV_KEY, uapi::BTN_TOUCH, 1),
                make_uapi_input_event(uapi::EV_ABS, uapi::ABS_MT_SLOT, 0),
                make_uapi_input_event(uapi::EV_ABS, uapi::ABS_MT_TRACKING_ID, 1),
                make_uapi_input_event(uapi::EV_ABS, uapi::ABS_MT_POSITION_X, 10),
                make_uapi_input_event(uapi::EV_ABS, uapi::ABS_MT_POSITION_Y, 20),
                make_uapi_input_event(uapi::EV_SYN, uapi::SYN_REPORT, 0),
            ]
        );
        assert_eq!(batch.last_event_time_ns, 0);
        assert_eq!(batch.count_converted_fidl_events, 1);
        assert_eq!(batch.count_ignored_fidl_events, 0);
        assert_eq!(batch.count_unexpected_fidl_events, 0);

        let mut want_converter =
            FuchsiaTouchEventToLinuxTouchEventConverter { pointer_id_to_slot_id: HashMap::new() };

        want_converter.pointer_id_to_slot_id.insert(1, 0);

        assert_eq!(converter, want_converter);
    }

    #[test]
    fn touch_change() {
        let mut converter = FuchsiaTouchEventToLinuxTouchEventConverter::create();
        let _ = converter.handle(vec![make_touch_event_with_coords_phase_id(
            10.0,
            20.0,
            FidlEventPhase::Add,
            1,
        )]);

        let batch = converter.handle(vec![make_touch_event_with_coords_phase_id(
            11.0,
            21.0,
            FidlEventPhase::Change,
            1,
        )]);
        assert_eq!(
            batch.events,
            vec![
                make_uapi_input_event(uapi::EV_ABS, uapi::ABS_MT_SLOT, 0),
                make_uapi_input_event(uapi::EV_ABS, uapi::ABS_MT_POSITION_X, 11),
                make_uapi_input_event(uapi::EV_ABS, uapi::ABS_MT_POSITION_Y, 21),
                make_uapi_input_event(uapi::EV_SYN, uapi::SYN_REPORT, 0),
            ]
        );
        assert_eq!(batch.last_event_time_ns, 0);
        assert_eq!(batch.count_converted_fidl_events, 1);
        assert_eq!(batch.count_ignored_fidl_events, 0);
        assert_eq!(batch.count_unexpected_fidl_events, 0);

        let mut want_converter =
            FuchsiaTouchEventToLinuxTouchEventConverter { pointer_id_to_slot_id: HashMap::new() };

        want_converter.pointer_id_to_slot_id.insert(1, 0);

        assert_eq!(converter, want_converter);
    }

    #[test]
    fn touch_remove() {
        let mut converter = FuchsiaTouchEventToLinuxTouchEventConverter::create();
        let _ = converter.handle(vec![make_touch_event_with_coords_phase_id(
            10.0,
            20.0,
            FidlEventPhase::Add,
            1,
        )]);
        let batch = converter.handle(vec![make_touch_event_with_coords_phase_id(
            0.0,
            0.0,
            FidlEventPhase::Remove,
            1,
        )]);
        assert_eq!(batch.last_event_time_ns, 0);
        assert_eq!(batch.count_converted_fidl_events, 1);
        assert_eq!(batch.count_ignored_fidl_events, 0);
        assert_eq!(batch.count_unexpected_fidl_events, 0);

        assert_eq!(
            batch.events,
            vec![
                make_uapi_input_event(uapi::EV_KEY, uapi::BTN_TOUCH, 0),
                make_uapi_input_event(uapi::EV_ABS, uapi::ABS_MT_SLOT, 0),
                make_uapi_input_event(uapi::EV_ABS, uapi::ABS_MT_TRACKING_ID, -1),
                make_uapi_input_event(uapi::EV_SYN, uapi::SYN_REPORT, 0),
            ]
        );

        assert_eq!(
            converter,
            FuchsiaTouchEventToLinuxTouchEventConverter { pointer_id_to_slot_id: HashMap::new() }
        );
    }

    #[test]
    fn multi_touch_sequence() {
        let mut converter = FuchsiaTouchEventToLinuxTouchEventConverter::create();

        // The first pointer down.
        let _ = converter.handle(vec![make_touch_event_with_coords_phase_id(
            10.0,
            20.0,
            FidlEventPhase::Add,
            1,
        )]);

        // The second pointer down, and the first pointer move.
        let batch = converter.handle(vec![
            make_touch_event_with_coords_phase_id(11.0, 21.0, FidlEventPhase::Change, 1),
            make_touch_event_with_coords_phase_id(100.0, 200.0, FidlEventPhase::Add, 2),
        ]);

        assert_eq!(
            batch.events,
            vec![
                make_uapi_input_event(uapi::EV_ABS, uapi::ABS_MT_SLOT, 0),
                make_uapi_input_event(uapi::EV_ABS, uapi::ABS_MT_POSITION_X, 11),
                make_uapi_input_event(uapi::EV_ABS, uapi::ABS_MT_POSITION_Y, 21),
                make_uapi_input_event(uapi::EV_ABS, uapi::ABS_MT_SLOT, 1),
                make_uapi_input_event(uapi::EV_ABS, uapi::ABS_MT_TRACKING_ID, 2),
                make_uapi_input_event(uapi::EV_ABS, uapi::ABS_MT_POSITION_X, 100),
                make_uapi_input_event(uapi::EV_ABS, uapi::ABS_MT_POSITION_Y, 200),
                make_uapi_input_event(uapi::EV_SYN, uapi::SYN_REPORT, 0),
            ]
        );
        assert_eq!(batch.last_event_time_ns, 0);
        assert_eq!(batch.count_converted_fidl_events, 2);
        assert_eq!(batch.count_ignored_fidl_events, 0);
        assert_eq!(batch.count_unexpected_fidl_events, 0);

        let mut want_converter =
            FuchsiaTouchEventToLinuxTouchEventConverter { pointer_id_to_slot_id: HashMap::new() };

        want_converter.pointer_id_to_slot_id.insert(1, 0);
        want_converter.pointer_id_to_slot_id.insert(2, 1);

        assert_eq!(converter, want_converter);

        // Both pointer move.
        let batch = converter.handle(vec![
            make_touch_event_with_coords_phase_id(12.0, 22.0, FidlEventPhase::Change, 1),
            make_touch_event_with_coords_phase_id(101.0, 201.0, FidlEventPhase::Change, 2),
        ]);

        assert_eq!(
            batch.events,
            vec![
                make_uapi_input_event(uapi::EV_ABS, uapi::ABS_MT_SLOT, 0),
                make_uapi_input_event(uapi::EV_ABS, uapi::ABS_MT_POSITION_X, 12),
                make_uapi_input_event(uapi::EV_ABS, uapi::ABS_MT_POSITION_Y, 22),
                make_uapi_input_event(uapi::EV_ABS, uapi::ABS_MT_SLOT, 1),
                make_uapi_input_event(uapi::EV_ABS, uapi::ABS_MT_POSITION_X, 101),
                make_uapi_input_event(uapi::EV_ABS, uapi::ABS_MT_POSITION_Y, 201),
                make_uapi_input_event(uapi::EV_SYN, uapi::SYN_REPORT, 0),
            ]
        );
        assert_eq!(batch.last_event_time_ns, 0);
        assert_eq!(batch.count_converted_fidl_events, 2);
        assert_eq!(batch.count_ignored_fidl_events, 0);
        assert_eq!(batch.count_unexpected_fidl_events, 0);
        assert_eq!(converter, want_converter);

        // The second pointer up, and the first pointer move.
        let batch = converter.handle(vec![
            make_touch_event_with_coords_phase_id(12.0, 22.0, FidlEventPhase::Change, 1),
            make_touch_event_with_coords_phase_id(0.0, 0.0, FidlEventPhase::Remove, 2),
        ]);

        assert_eq!(
            batch.events,
            vec![
                make_uapi_input_event(uapi::EV_ABS, uapi::ABS_MT_SLOT, 0),
                make_uapi_input_event(uapi::EV_ABS, uapi::ABS_MT_POSITION_X, 12),
                make_uapi_input_event(uapi::EV_ABS, uapi::ABS_MT_POSITION_Y, 22),
                make_uapi_input_event(uapi::EV_ABS, uapi::ABS_MT_SLOT, 1),
                make_uapi_input_event(uapi::EV_ABS, uapi::ABS_MT_TRACKING_ID, -1),
                make_uapi_input_event(uapi::EV_SYN, uapi::SYN_REPORT, 0),
            ]
        );
        assert_eq!(batch.last_event_time_ns, 0);
        assert_eq!(batch.count_converted_fidl_events, 2);
        assert_eq!(batch.count_ignored_fidl_events, 0);
        assert_eq!(batch.count_unexpected_fidl_events, 0);

        want_converter.pointer_id_to_slot_id.remove(&2);

        assert_eq!(converter, want_converter);

        // The third pointer down, and the first pointer move.
        let batch = converter.handle(vec![
            make_touch_event_with_coords_phase_id(12.0, 22.0, FidlEventPhase::Change, 1),
            make_touch_event_with_coords_phase_id(50.0, 60.0, FidlEventPhase::Add, 3),
        ]);

        assert_eq!(
            batch.events,
            vec![
                make_uapi_input_event(uapi::EV_ABS, uapi::ABS_MT_SLOT, 0),
                make_uapi_input_event(uapi::EV_ABS, uapi::ABS_MT_POSITION_X, 12),
                make_uapi_input_event(uapi::EV_ABS, uapi::ABS_MT_POSITION_Y, 22),
                // should reuse slot id 1.
                make_uapi_input_event(uapi::EV_ABS, uapi::ABS_MT_SLOT, 1),
                make_uapi_input_event(uapi::EV_ABS, uapi::ABS_MT_TRACKING_ID, 3),
                make_uapi_input_event(uapi::EV_ABS, uapi::ABS_MT_POSITION_X, 50),
                make_uapi_input_event(uapi::EV_ABS, uapi::ABS_MT_POSITION_Y, 60),
                make_uapi_input_event(uapi::EV_SYN, uapi::SYN_REPORT, 0),
            ]
        );
        assert_eq!(batch.last_event_time_ns, 0);
        assert_eq!(batch.count_converted_fidl_events, 2);
        assert_eq!(batch.count_ignored_fidl_events, 0);
        assert_eq!(batch.count_unexpected_fidl_events, 0);

        want_converter.pointer_id_to_slot_id.insert(3, 1);

        assert_eq!(converter, want_converter);

        // The third pointer up, and the first pointer move.
        let batch = converter.handle(vec![
            make_touch_event_with_coords_phase_id(12.0, 22.0, FidlEventPhase::Change, 1),
            make_touch_event_with_coords_phase_id(0.0, 0.0, FidlEventPhase::Remove, 3),
        ]);

        assert_eq!(
            batch.events,
            vec![
                make_uapi_input_event(uapi::EV_ABS, uapi::ABS_MT_SLOT, 0),
                make_uapi_input_event(uapi::EV_ABS, uapi::ABS_MT_POSITION_X, 12),
                make_uapi_input_event(uapi::EV_ABS, uapi::ABS_MT_POSITION_Y, 22),
                make_uapi_input_event(uapi::EV_ABS, uapi::ABS_MT_SLOT, 1),
                make_uapi_input_event(uapi::EV_ABS, uapi::ABS_MT_TRACKING_ID, -1),
                make_uapi_input_event(uapi::EV_SYN, uapi::SYN_REPORT, 0),
            ]
        );
        assert_eq!(batch.last_event_time_ns, 0);
        assert_eq!(batch.count_converted_fidl_events, 2);
        assert_eq!(batch.count_ignored_fidl_events, 0);
        assert_eq!(batch.count_unexpected_fidl_events, 0);

        want_converter.pointer_id_to_slot_id.remove(&3);

        assert_eq!(converter, want_converter);

        // The first pointer up.
        let batch = converter.handle(vec![make_touch_event_with_coords_phase_id(
            0.0,
            0.0,
            FidlEventPhase::Remove,
            1,
        )]);

        assert_eq!(
            batch.events,
            vec![
                make_uapi_input_event(uapi::EV_KEY, uapi::BTN_TOUCH, 0),
                make_uapi_input_event(uapi::EV_ABS, uapi::ABS_MT_SLOT, 0),
                make_uapi_input_event(uapi::EV_ABS, uapi::ABS_MT_TRACKING_ID, -1),
                make_uapi_input_event(uapi::EV_SYN, uapi::SYN_REPORT, 0),
            ]
        );
        assert_eq!(batch.last_event_time_ns, 0);
        assert_eq!(batch.count_converted_fidl_events, 1);
        assert_eq!(batch.count_ignored_fidl_events, 0);
        assert_eq!(batch.count_unexpected_fidl_events, 0);

        want_converter.pointer_id_to_slot_id = HashMap::new();

        assert_eq!(converter, want_converter);
    }

    #[test]
    fn multi_touch_sequence_receive_only_one_pointer_change_when_two_pointer_contacting() {
        let mut converter = FuchsiaTouchEventToLinuxTouchEventConverter::create();

        // 2 pointer down.
        let batch = converter.handle(vec![
            make_touch_event_with_coords_phase_id(10.0, 20.0, FidlEventPhase::Add, 1),
            make_touch_event_with_coords_phase_id(100.0, 200.0, FidlEventPhase::Add, 2),
        ]);

        assert_eq!(
            batch.events,
            vec![
                make_uapi_input_event(uapi::EV_KEY, uapi::BTN_TOUCH, 1),
                make_uapi_input_event(uapi::EV_ABS, uapi::ABS_MT_SLOT, 0),
                make_uapi_input_event(uapi::EV_ABS, uapi::ABS_MT_TRACKING_ID, 1),
                make_uapi_input_event(uapi::EV_ABS, uapi::ABS_MT_POSITION_X, 10),
                make_uapi_input_event(uapi::EV_ABS, uapi::ABS_MT_POSITION_Y, 20),
                make_uapi_input_event(uapi::EV_ABS, uapi::ABS_MT_SLOT, 1),
                make_uapi_input_event(uapi::EV_ABS, uapi::ABS_MT_TRACKING_ID, 2),
                make_uapi_input_event(uapi::EV_ABS, uapi::ABS_MT_POSITION_X, 100),
                make_uapi_input_event(uapi::EV_ABS, uapi::ABS_MT_POSITION_Y, 200),
                make_uapi_input_event(uapi::EV_SYN, uapi::SYN_REPORT, 0),
            ]
        );
        assert_eq!(batch.last_event_time_ns, 0);
        assert_eq!(batch.count_converted_fidl_events, 2);
        assert_eq!(batch.count_ignored_fidl_events, 0);
        assert_eq!(batch.count_unexpected_fidl_events, 0);

        let mut want_converter =
            FuchsiaTouchEventToLinuxTouchEventConverter { pointer_id_to_slot_id: HashMap::new() };

        want_converter.pointer_id_to_slot_id.insert(1, 0);
        want_converter.pointer_id_to_slot_id.insert(2, 1);

        assert_eq!(converter, want_converter);

        // 1st pointer move, no event for 2nd pointer.
        let batch = converter.handle(vec![make_touch_event_with_coords_phase_id(
            12.0,
            22.0,
            FidlEventPhase::Change,
            1,
        )]);

        assert_eq!(
            batch.events,
            vec![
                make_uapi_input_event(uapi::EV_ABS, uapi::ABS_MT_SLOT, 0),
                make_uapi_input_event(uapi::EV_ABS, uapi::ABS_MT_POSITION_X, 12),
                make_uapi_input_event(uapi::EV_ABS, uapi::ABS_MT_POSITION_Y, 22),
                make_uapi_input_event(uapi::EV_SYN, uapi::SYN_REPORT, 0),
            ]
        );
        assert_eq!(batch.last_event_time_ns, 0);
        assert_eq!(batch.count_converted_fidl_events, 1);
        assert_eq!(batch.count_ignored_fidl_events, 0);
        assert_eq!(batch.count_unexpected_fidl_events, 0);
        assert_eq!(converter, want_converter);
    }

    #[test]
    fn handle_return_multi_protocl_b_seq() {
        let mut converter = FuchsiaTouchEventToLinuxTouchEventConverter::create();

        let batch = converter.handle(vec![
            // ignore
            FidlTouchEvent::default(),
            make_touch_event_with_coords_phase_id_time(10.0, 20.0, FidlEventPhase::Add, 1, 1),
            make_touch_event_with_coords_phase_id_time(11.0, 21.0, FidlEventPhase::Change, 1, 1000),
        ]);

        assert_eq!(
            batch.events,
            vec![
                make_uapi_input_event_with_time(uapi::EV_KEY, uapi::BTN_TOUCH, 1, 1),
                make_uapi_input_event_with_time(uapi::EV_ABS, uapi::ABS_MT_SLOT, 0, 1),
                make_uapi_input_event_with_time(uapi::EV_ABS, uapi::ABS_MT_TRACKING_ID, 1, 1),
                make_uapi_input_event_with_time(uapi::EV_ABS, uapi::ABS_MT_POSITION_X, 10, 1),
                make_uapi_input_event_with_time(uapi::EV_ABS, uapi::ABS_MT_POSITION_Y, 20, 1),
                make_uapi_input_event_with_time(uapi::EV_SYN, uapi::SYN_REPORT, 0, 1),
                make_uapi_input_event_with_time(uapi::EV_ABS, uapi::ABS_MT_SLOT, 0, 1000),
                make_uapi_input_event_with_time(uapi::EV_ABS, uapi::ABS_MT_POSITION_X, 11, 1000),
                make_uapi_input_event_with_time(uapi::EV_ABS, uapi::ABS_MT_POSITION_Y, 21, 1000),
                make_uapi_input_event_with_time(uapi::EV_SYN, uapi::SYN_REPORT, 0, 1000),
            ]
        );
        assert_eq!(batch.last_event_time_ns, 1000);
        assert_eq!(batch.count_converted_fidl_events, 2);
        assert_eq!(batch.count_ignored_fidl_events, 1);
        assert_eq!(batch.count_unexpected_fidl_events, 0);

        let mut want_converter =
            FuchsiaTouchEventToLinuxTouchEventConverter { pointer_id_to_slot_id: HashMap::new() };

        want_converter.pointer_id_to_slot_id.insert(1, 0);
        assert_eq!(converter, want_converter);
    }

    #[test]
    fn handle_unsorted_events() {
        let mut converter = FuchsiaTouchEventToLinuxTouchEventConverter::create();

        let batch = converter.handle(vec![
            // ignore
            FidlTouchEvent::default(),
            make_touch_event_with_coords_phase_id_time(11.0, 21.0, FidlEventPhase::Change, 1, 1000),
            make_touch_event_with_coords_phase_id_time(10.0, 20.0, FidlEventPhase::Add, 1, 1),
        ]);

        assert_eq!(
            batch.events,
            vec![
                make_uapi_input_event_with_time(uapi::EV_KEY, uapi::BTN_TOUCH, 1, 1),
                make_uapi_input_event_with_time(uapi::EV_ABS, uapi::ABS_MT_SLOT, 0, 1),
                make_uapi_input_event_with_time(uapi::EV_ABS, uapi::ABS_MT_TRACKING_ID, 1, 1),
                make_uapi_input_event_with_time(uapi::EV_ABS, uapi::ABS_MT_POSITION_X, 10, 1),
                make_uapi_input_event_with_time(uapi::EV_ABS, uapi::ABS_MT_POSITION_Y, 20, 1),
                make_uapi_input_event_with_time(uapi::EV_SYN, uapi::SYN_REPORT, 0, 1),
                make_uapi_input_event_with_time(uapi::EV_ABS, uapi::ABS_MT_SLOT, 0, 1000),
                make_uapi_input_event_with_time(uapi::EV_ABS, uapi::ABS_MT_POSITION_X, 11, 1000),
                make_uapi_input_event_with_time(uapi::EV_ABS, uapi::ABS_MT_POSITION_Y, 21, 1000),
                make_uapi_input_event_with_time(uapi::EV_SYN, uapi::SYN_REPORT, 0, 1000),
            ]
        );
        assert_eq!(batch.last_event_time_ns, 1000);
        assert_eq!(batch.count_converted_fidl_events, 2);
        assert_eq!(batch.count_ignored_fidl_events, 1);
        assert_eq!(batch.count_unexpected_fidl_events, 0);

        let mut want_converter =
            FuchsiaTouchEventToLinuxTouchEventConverter { pointer_id_to_slot_id: HashMap::new() };

        want_converter.pointer_id_to_slot_id.insert(1, 0);
        assert_eq!(converter, want_converter);
    }
}

#[cfg(test)]
mod keyboard_tests {
    use super::*;
    use assert_matches::assert_matches;
    use pretty_assertions::assert_eq;
    use test_case::test_case;
    use uapi::timeval;

    #[test]
    fn init_key_map_no_assert_failed() {
        let _ = init_key_map();
    }

    #[test]
    fn linux_keycode_to_fuchsia_input_key() {
        let km = init_key_map();
        for (&linux_key, &want) in km.linux_to_fuchsia.iter() {
            let got = km.linux_keycode_to_fuchsia_input_key(linux_key);
            assert_eq!(want, got);
        }
    }

    #[test]
    fn unknown_linux_keycode_to_fuchsia_input_key() {
        let km = init_key_map();
        let got = km.linux_keycode_to_fuchsia_input_key(701);
        assert_eq!(Key::Unknown, got);
    }

    #[test]
    fn fuchsia_input_key_to_linux_keycode() {
        let km = init_key_map();
        for (&fuchsia_key, &want) in km.fuchsia_to_linux.iter() {
            let got = km.fuchsia_input_key_to_linux_keycode(fuchsia_key);
            assert_eq!(want, got);
        }
    }

    #[test]
    fn linux_keycode_testset() {
        // Want to ensure all linux keycode in this can map to fuchsia key. See b/311425670 for
        // details.
        let linux_keycodes: Vec<u32> = vec![
            1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24,
            25, 26, 27, 28, 29, 30, 31, 32, 33, 34, 35, 36, 37, 38, 39, 40, 41, 42, 44, 45, 46, 47,
            48, 49, 50, 51, 52, 53, 54, 55, 56, 57, 58, 59, 60, 61, 62, 63, 64, 65, 66, 67, 68, 69,
            70, 71, 72, 73, 74, 75, 76, 77, 78, 79, 80, 81, 82, 83, 85, 86, 87, 88, 89, 92, 93, 94,
            96, 97, 98, 100, 102, 103, 105, 106, 107, 108, 110, 111, 113, 114, 115, 117, 119, 121,
            122, 123, 124, 133, 135, 137, 139, 156, 159, 160, 162, 163, 164, 165, 166, 167, 168,
            169, 173, 177, 178, 179, 180, 201, 207, 208, 212, 226, 288, 289, 290, 291, 292, 293,
            294, 295, 296, 297, 298, 299, 300, 301, 302, 303, 304, 305, 306, 307, 308, 309, 310,
            311, 312, 313, 314, 315, 316, 317, 318, 353, 362, 366, 370, 377, 398, 399, 400, 401,
            402, 403, 405, 464, 522, 523,
        ];

        let km = init_key_map();

        let mut kcs = vec![];
        for kc in linux_keycodes {
            if km.linux_keycode_to_fuchsia_input_key(kc) == Key::Unknown {
                kcs.push(kc);
            }
        }

        assert_eq!(kcs.len(), 0, "{:?}", kcs);
    }

    fn uapi_input_event(ty: u32, code: u32, value: i32) -> uapi::input_event {
        uapi::input_event { time: timeval::default(), type_: ty as u16, code: code as u16, value }
    }

    #[test]
    fn parse_linux_events_to_fidl_keyboard_event_send_syn_when_no_cached_event() {
        let mut linux_keyboard_event_parser = LinuxKeyboardEventParser::create();
        let e = uapi_input_event(uapi::EV_SYN, uapi::SYN_REPORT, 0);
        let res = linux_keyboard_event_parser.handle(e);
        assert_eq!(res, error!(EINVAL));
    }

    #[test_case(
        uapi_input_event(uapi::EV_KEY, uapi::KEY_A, 1);
        "press")]
    #[test_case(
        uapi_input_event(uapi::EV_KEY, uapi::KEY_A, 0);
        "release, not fail on this step")]
    #[test_case(
        uapi_input_event(uapi::EV_KEY, uapi::KEY_RESERVED, 1);
        "unknown keycode, not fail on this step")]
    fn parse_linux_events_to_fidl_keyboard_event_send_key_when_no_cached_event_and_no_pressing_keys(
        e: uapi::input_event,
    ) {
        let mut linux_keyboard_event_parser = LinuxKeyboardEventParser::create();
        let res = linux_keyboard_event_parser.handle(e);
        pretty_assertions::assert_eq!(res, Ok(None));
        pretty_assertions::assert_eq!(
            linux_keyboard_event_parser,
            LinuxKeyboardEventParser { cached_event: Some(e), pressing_keys: vec![] },
        );
    }

    #[test]
    fn parse_linux_events_to_fidl_keyboard_event_send_syn_when_have_cached_event_and_no_pressing_keys(
    ) {
        let mut linux_keyboard_event_parser = LinuxKeyboardEventParser::create();
        let e = uapi_input_event(uapi::EV_KEY, uapi::KEY_A, 1);
        let res = linux_keyboard_event_parser.handle(e);
        assert_eq!(res, Ok(None));

        let e = uapi_input_event(uapi::EV_SYN, uapi::SYN_REPORT, 0);
        let res = linux_keyboard_event_parser.handle(e);
        assert_eq!(
            res,
            Ok(Some(fir::InputReport {
                event_time: Some(0),
                keyboard: Some(fir::KeyboardInputReport {
                    pressed_keys3: Some(vec![Key::A]),
                    ..Default::default()
                }),
                ..Default::default()
            }))
        );
        assert_eq!(
            linux_keyboard_event_parser,
            LinuxKeyboardEventParser { cached_event: None, pressing_keys: vec![Key::A] },
        );
    }

    #[test_case(
        uapi_input_event(uapi::EV_KEY, uapi::KEY_A, 0);
        "release not pressing")]
    #[test_case(
        uapi_input_event(uapi::EV_KEY, uapi::KEY_RESERVED, 1);
        "unknown keycode")]
    fn parse_linux_events_to_fidl_keyboard_event_send_syn_when_have_cached_event_and_no_pressing_keys_failed(
        cached: uapi::input_event,
    ) {
        let mut linux_keyboard_event_parser = LinuxKeyboardEventParser::create();
        let res = linux_keyboard_event_parser.handle(cached);
        pretty_assertions::assert_eq!(res, Ok(None));

        let e = uapi_input_event(uapi::EV_SYN, uapi::SYN_REPORT, 0);
        let res = linux_keyboard_event_parser.handle(e);
        pretty_assertions::assert_eq!(res, error!(EINVAL));
        pretty_assertions::assert_eq!(
            linux_keyboard_event_parser,
            LinuxKeyboardEventParser { cached_event: None, pressing_keys: vec![] },
        );
    }

    #[test]
    fn parse_linux_events_to_fidl_keyboard_event_send_key_when_have_cached_event() {
        let mut linux_keyboard_event_parser = LinuxKeyboardEventParser::create();
        let e = uapi_input_event(uapi::EV_KEY, uapi::KEY_A, 1);
        let res = linux_keyboard_event_parser.handle(e);
        pretty_assertions::assert_eq!(res, Ok(None));

        let e = uapi_input_event(uapi::EV_KEY, uapi::KEY_B, 1);
        let res = linux_keyboard_event_parser.handle(e);
        pretty_assertions::assert_eq!(res, error!(EINVAL));
        pretty_assertions::assert_eq!(
            linux_keyboard_event_parser,
            LinuxKeyboardEventParser { cached_event: None, pressing_keys: vec![] },
        );
    }

    #[test]
    fn parse_linux_events_to_fidl_keyboard_event_press_pressing_key() {
        let mut linux_keyboard_event_parser = LinuxKeyboardEventParser::create();
        let press_a = uapi_input_event(uapi::EV_KEY, uapi::KEY_A, 1);
        let res = linux_keyboard_event_parser.handle(press_a);
        assert_eq!(res, Ok(None));

        let syn = uapi_input_event(uapi::EV_SYN, uapi::SYN_REPORT, 0);
        let res = linux_keyboard_event_parser.handle(syn);
        assert_matches!(res, Ok(Some(_)));

        let res = linux_keyboard_event_parser.handle(press_a);
        assert_eq!(res, Ok(None));
        let res = linux_keyboard_event_parser.handle(syn);
        assert_eq!(res, error!(EINVAL));
        assert_eq!(
            linux_keyboard_event_parser,
            LinuxKeyboardEventParser { cached_event: None, pressing_keys: vec![] },
        );
    }

    #[test]
    fn parse_linux_events_to_fidl_keyboard_event_release_key() {
        let mut linux_keyboard_event_parser = LinuxKeyboardEventParser::create();
        let press_a = uapi_input_event(uapi::EV_KEY, uapi::KEY_A, 1);
        let res = linux_keyboard_event_parser.handle(press_a);
        assert_eq!(res, Ok(None));

        let syn = uapi_input_event(uapi::EV_SYN, uapi::SYN_REPORT, 0);
        let res = linux_keyboard_event_parser.handle(syn);
        assert_matches!(res, Ok(Some(_)));

        let release_a = uapi_input_event(uapi::EV_KEY, uapi::KEY_A, 0);
        let res = linux_keyboard_event_parser.handle(release_a);
        assert_eq!(res, Ok(None));
        let res = linux_keyboard_event_parser.handle(syn);
        assert_eq!(
            res,
            Ok(Some(fir::InputReport {
                event_time: Some(0),
                keyboard: Some(fir::KeyboardInputReport {
                    pressed_keys3: Some(vec![]),
                    ..Default::default()
                }),
                ..Default::default()
            }))
        );
        assert_eq!(
            linux_keyboard_event_parser,
            LinuxKeyboardEventParser { cached_event: None, pressing_keys: vec![] },
        );
    }
}
