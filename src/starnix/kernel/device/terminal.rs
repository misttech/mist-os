// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use derivative::Derivative;
use macro_rules_attribute::apply;
use starnix_sync::{Mutex, RwLock};
use std::collections::{BTreeSet, HashMap, VecDeque};
use std::sync::{Arc, Weak};

use crate::fs::devpts::{get_device_type_for_pts, DEVPTS_COUNT};
use crate::mutable_state::{state_accessor, state_implementation};
use crate::task::{
    CurrentTask, EventHandler, ProcessGroup, Session, WaitCanceler, WaitQueue, Waiter,
};
use crate::vfs::buffers::{InputBuffer, InputBufferExt as _, OutputBuffer};
use starnix_logging::track_stub;
use starnix_sync::{LockBefore, Locked, ProcessGroupState};
use starnix_uapi::auth::FsCred;
use starnix_uapi::device_type::DeviceType;
use starnix_uapi::errors::Errno;
use starnix_uapi::signals::{Signal, SIGINT, SIGQUIT, SIGSTOP};
use starnix_uapi::vfs::FdEvents;
use starnix_uapi::{
    cc_t, error, tcflag_t, uapi, ECHO, ECHOCTL, ECHOE, ECHOK, ECHOKE, ECHONL, ECHOPRT, ICANON,
    ICRNL, IEXTEN, IGNCR, INLCR, ISIG, IUTF8, OCRNL, ONLCR, ONLRET, ONOCR, OPOST, TABDLY, VEOF,
    VEOL, VEOL2, VERASE, VINTR, VQUIT, VSUSP, VWERASE, XTABS,
};

// CANON_MAX_BYTES is the number of bytes that fit into a single line of
// terminal input in canonical mode. See https://github.com/google/gvisor/blob/master/pkg/sentry/fs/tty/line_discipline.go
const CANON_MAX_BYTES: usize = 4096;

// NON_CANON_MAX_BYTES is the maximum number of bytes that can be read at
// a time in non canonical mode.
const NON_CANON_MAX_BYTES: usize = CANON_MAX_BYTES - 1;

// WAIT_BUFFER_MAX_BYTES is the maximum size of a wait buffer. It is based on
// https://github.com/google/gvisor/blob/master/pkg/sentry/fsimpl/devpts/queue.go
const WAIT_BUFFER_MAX_BYTES: usize = 131072;

const SPACES_PER_TAB: usize = 8;

// DISABLED_CHAR is used to indicate that a control character is disabled.
const DISABLED_CHAR: u8 = 0;

const BACKSPACE_CHAR: u8 = 8; // \b

/// The offset in ASCII between a control character and it's character name.
/// For example, typing CTRL-C on a keyboard generates the value
/// b'C' - CONTROL_OFFSET
const CONTROL_OFFSET: u8 = 0x40;

/// Global state of the devpts filesystem.
pub struct TTYState {
    /// The terminal objects indexed by their identifier.
    pub terminals: RwLock<HashMap<u32, Weak<Terminal>>>,

    /// The set of available terminal identifier.
    pts_ids_set: Mutex<PtsIdsSet>,
}

impl TTYState {
    /// Returns the next available terminal.
    pub fn get_next_terminal(
        self: &Arc<Self>,
        current_task: &CurrentTask,
    ) -> Result<Arc<Terminal>, Errno> {
        let id = self.pts_ids_set.lock().acquire()?;
        let terminal = Arc::new(Terminal::new(self.clone(), current_task.as_fscred(), id));
        assert!(self.terminals.write().insert(id, Arc::downgrade(&terminal)).is_none());
        Ok(terminal)
    }

    /// Release the terminal identifier into the set of available identifier.
    pub fn release_terminal(&self, id: u32) -> Result<(), Errno> {
        // We need to remove this terminal id from the set of terminals before we release the
        // identifier. Otherwise, the id might be reused for a new terminal and we'll remove
        // the *new* terminal with that identifier instead of the old one.
        assert!(self.terminals.write().remove(&id).is_some());
        self.pts_ids_set.lock().release(id);
        Ok(())
    }
}

impl Default for TTYState {
    fn default() -> Self {
        Self {
            terminals: RwLock::new(HashMap::new()),
            pts_ids_set: Mutex::new(PtsIdsSet::new(DEVPTS_COUNT)),
        }
    }
}

#[derive(Derivative)]
#[derivative(Default)]
#[derivative(Debug)]
pub struct TerminalMutableState {
    /// |true| is the terminal is locked.
    #[derivative(Default(value = "true"))]
    pub locked: bool,

    /// Terminal size.
    pub window_size: uapi::winsize,

    /// Terminal configuration.
    #[derivative(Default(value = "get_default_termios()"))]
    termios: uapi::termios,

    /// Location in a row of the cursor. Needed to handle certain special characters like
    /// backspace.
    column: usize,

    /// The number of active references to the main part of the terminal. Starts as `None`. The
    /// main part of the terminal is considered closed when this is `Some(0)`.
    main_references: Option<u32>,

    /// The number of active references to the replica part of the terminal. Starts as `None`. The
    /// replica part of the terminal is considered closed when this is `Some(0)`.
    replica_references: Option<u32>,

    /// Input queue of the terminal. Data flow from the main side to the replica side.
    ///
    /// This option is never empty in the steady state of the terminal. Mutating methods on Queue
    /// need a mutable borrow of this object. As rust borrow checker prevents multiple mutable
    /// borrows, the queue is instead moved to the stack, the mutating method is called and the
    /// queue is moved back to this object. This is safe because:
    /// - Moving the queue to the stack requires a write lock on the terminal, which ensure
    /// exclusive access to this object, so no other thread will try to access the queue.
    /// - The methods on the queue that calls back to this object won't try to access the same
    /// queue.
    #[derivative(Default(value = "Queue::input_queue()"))]
    input_queue: Option<Queue>,
    /// Output queue of the terminal. Data flow from the replica side to the main side.
    ///
    /// This option is never empty in the steady state of the terminal. Mutating methods on Queue
    /// need a mutable borrow of this object. As rust borrow checker prevents multiple mutable
    /// borrows, the queue is instead moved to the stack, the mutating method is called and the
    /// queue is moved back to this object. This is safe because:
    /// - Moving the queue to the stack requires a write lock on the terminal, which ensure
    /// exclusive access to this object, so no other thread will try to access the queue.
    /// - The methods on the queue that calls back to this object won't try to access the same
    /// queue.
    #[derivative(Default(value = "Queue::output_queue()"))]
    output_queue: Option<Queue>,

    /// Wait queue for the main side of the terminal.
    main_wait_queue: WaitQueue,

    /// Wait queue for the replica side of the terminal.
    replica_wait_queue: WaitQueue,

    /// The controller for the terminal.
    pub controller: Option<TerminalController>,
}

/// State of a given terminal. This object handles both the main and the replica terminal.
#[derive(Derivative)]
#[derivative(Debug)]
pub struct Terminal {
    /// The global devpts state.
    #[derivative(Debug = "ignore")]
    state: Arc<TTYState>,

    /// The owner of the terminal.
    pub fscred: FsCred,

    /// The identifier of the terminal.
    pub id: u32,

    /// The mutable state of the Terminal.
    mutable_state: RwLock<TerminalMutableState>,
}

impl Terminal {
    pub fn new(state: Arc<TTYState>, fscred: FsCred, id: u32) -> Self {
        Self { state, fscred, id, mutable_state: RwLock::new(Default::default()) }
    }

    /// Sets the terminal configuration.
    pub fn set_termios<L>(&self, locked: &mut Locked<'_, L>, termios: uapi::termios)
    where
        L: LockBefore<ProcessGroupState>,
    {
        let signals = self.write().set_termios(termios);
        self.send_signals(locked, signals);
    }

    /// `close` implementation of the main side of the terminal.
    pub fn main_close(&self) {
        self.write().main_close();
    }

    /// Called when a new reference to the replica side of this terminal is made.
    pub fn main_open(&self) {
        self.write().main_open();
    }

    /// `wait_async` implementation of the main side of the terminal.
    pub fn main_wait_async(
        &self,
        waiter: &Waiter,
        events: FdEvents,
        handler: EventHandler,
    ) -> WaitCanceler {
        self.read().main_wait_async(waiter, events, handler)
    }

    /// `query_events` implementation of the main side of the terminal.
    pub fn main_query_events(&self) -> FdEvents {
        self.read().main_query_events()
    }

    /// `read` implementation of the main side of the terminal.
    pub fn main_read<L>(
        &self,
        _locked: &mut Locked<'_, L>,
        current_task: &CurrentTask,
        data: &mut dyn OutputBuffer,
    ) -> Result<usize, Errno>
    where
        L: LockBefore<ProcessGroupState>,
    {
        self.write().main_read(current_task, data)
    }

    /// `write` implementation of the main side of the terminal.
    pub fn main_write<L>(
        &self,
        locked: &mut Locked<'_, L>,
        current_task: &CurrentTask,
        data: &mut dyn InputBuffer,
    ) -> Result<usize, Errno>
    where
        L: LockBefore<ProcessGroupState>,
    {
        let (bytes, signals) = self.write().main_write(current_task, data)?;
        self.send_signals(locked, signals);
        Ok(bytes)
    }

    /// `close` implementation of the replica side of the terminal.
    pub fn replica_close(&self) {
        self.write().replica_close();
    }

    /// Called when a new reference to the replica side of this terminal is made.
    pub fn replica_open(&self) {
        self.write().replica_open();
    }

    /// `wait_async` implementation of the replica side of the terminal.
    pub fn replica_wait_async(
        &self,
        waiter: &Waiter,
        events: FdEvents,
        handler: EventHandler,
    ) -> WaitCanceler {
        self.read().replica_wait_async(waiter, events, handler)
    }

    /// `query_events` implementation of the replica side of the terminal.
    pub fn replica_query_events(&self) -> FdEvents {
        self.read().replica_query_events()
    }

    /// `read` implementation of the replica side of the terminal.
    pub fn replica_read<L>(
        &self,
        _locked: &mut Locked<'_, L>,
        current_task: &CurrentTask,
        data: &mut dyn OutputBuffer,
    ) -> Result<usize, Errno>
    where
        L: LockBefore<ProcessGroupState>,
    {
        self.write().replica_read(current_task, data)
    }

    /// `write` implementation of the replica side of the terminal.
    pub fn replica_write<L>(
        &self,
        _locked: &mut Locked<'_, L>,
        current_task: &CurrentTask,
        data: &mut dyn InputBuffer,
    ) -> Result<usize, Errno>
    where
        L: LockBefore<ProcessGroupState>,
    {
        self.write().replica_write(current_task, data)
    }

    /// Send the pending signals to the associated foreground process groups if they exist.
    fn send_signals<L>(&self, locked: &mut Locked<'_, L>, signals: PendingSignals)
    where
        L: LockBefore<ProcessGroupState>,
    {
        let signals = signals.signals();
        if !signals.is_empty() {
            let process_group = {
                let terminal_state = self.read();
                let Some(controller) = terminal_state.controller.as_ref() else {
                    return;
                };
                let Some(session) = controller.session.upgrade() else {
                    return;
                };
                let Some(process_group) = session.read().get_foreground_process_group() else {
                    return;
                };
                process_group
            };
            process_group.send_signals(locked, signals);
        }
    }

    pub fn device(&self) -> DeviceType {
        get_device_type_for_pts(self.id)
    }

    state_accessor!(Terminal, mutable_state);
}

/// Macro to help working with the terminal queues. This macro will handle moving the queue to the
/// stack, calling the method on it, moving it back to the terminal and returning the result.
///
/// See the comments on `input_queue` and `output_queue` for the reason.
///
/// This expect to be called with a single method call to either the input or output queue, on
/// self. Example:
/// ```
/// let bytes = with_queue!(self.output_queue.read(self, current_task, data))?;
/// ```
macro_rules! with_queue {
    ($self_:tt . $name:ident . $fn:ident ( $($param:expr),*$(,)?)) => {
        {
        let mut queue = $self_.$name . take().unwrap();
        let result = queue.$fn( $($param),* );
        $self_.$name = Some(queue);
        result
        }
    };
}

/// Keep track of the signals to send when handling terminal content.
#[must_use]
pub struct PendingSignals {
    signals: Vec<Signal>,
}

impl PendingSignals {
    pub fn new() -> Self {
        Self { signals: vec![] }
    }

    /// Add the given signal to the list of signal to send to the associate process group.
    fn add(&mut self, signal: Signal) {
        self.signals.push(signal);
    }

    /// Append all pending signals in `other` to `self`.
    fn append(&mut self, mut other: Self) {
        self.signals.append(&mut other.signals);
    }

    pub fn signals(&self) -> &[Signal] {
        &self.signals[..]
    }
}

#[apply(state_implementation!)]
impl TerminalMutableState<Base = Terminal> {
    /// Returns the terminal configuration.
    pub fn termios(&self) -> &uapi::termios {
        &self.termios
    }

    /// Returns the number of available bytes to read from the side of the terminal described by
    /// `is_main`.
    pub fn get_available_read_size(&self, is_main: bool) -> usize {
        let queue = if is_main { self.output_queue() } else { self.input_queue() };
        queue.readable_size()
    }

    /// Sets the terminal configuration.
    fn set_termios(&mut self, termios: uapi::termios) -> PendingSignals {
        let old_canon_enabled = self.termios.has_local_flags(ICANON);
        self.termios = termios;
        if old_canon_enabled && !self.termios.has_local_flags(ICANON) {
            let signals = with_queue!(self.input_queue.on_canon_disabled(self.as_mut()));
            self.notify_waiters();
            signals
        } else {
            PendingSignals::new()
        }
    }

    /// `close` implementation of the main side of the terminal.
    pub fn main_close(&mut self) {
        self.main_references = self.main_references.map(|v| v - 1);
        self.notify_waiters();
    }

    /// Called when a new reference to the replica side of this terminal is made.
    pub fn main_open(&mut self) {
        self.main_references = Some(self.main_references.unwrap_or(0) + 1);
    }

    pub fn is_main_closed(&self) -> bool {
        matches!(self.main_references, Some(0))
    }

    /// `wait_async` implementation of the main side of the terminal.
    fn main_wait_async(
        &self,
        waiter: &Waiter,
        events: FdEvents,
        handler: EventHandler,
    ) -> WaitCanceler {
        self.main_wait_queue.wait_async_fd_events(waiter, events, handler)
    }

    /// `query_events` implementation of the main side of the terminal.
    fn main_query_events(&self) -> FdEvents {
        if self.is_replica_closed() && self.output_queue().readable_size() == 0 {
            return FdEvents::POLLOUT | FdEvents::POLLHUP;
        }
        self.output_queue().read_readyness() | self.input_queue().write_readyness()
    }

    /// `read` implementation of the main side of the terminal.
    fn main_read(
        &mut self,
        current_task: &CurrentTask,
        data: &mut dyn OutputBuffer,
    ) -> Result<usize, Errno> {
        if self.is_replica_closed() && self.output_queue().readable_size() == 0 {
            return error!(EIO);
        }
        let result = with_queue!(self.output_queue.read(self.as_mut(), current_task, data))?;
        self.notify_waiters();
        Ok(result)
    }

    /// `write` implementation of the main side of the terminal.
    fn main_write(
        &mut self,
        current_task: &CurrentTask,
        data: &mut dyn InputBuffer,
    ) -> Result<(usize, PendingSignals), Errno> {
        let result = with_queue!(self.input_queue.write(self.as_mut(), current_task, data))?;
        self.notify_waiters();
        Ok(result)
    }

    /// `close` implementation of the replica side of the terminal.
    pub fn replica_close(&mut self) {
        self.replica_references = self.replica_references.map(|v| v - 1);
        self.notify_waiters();
    }

    /// Called when a new reference to the replica side of this terminal is made.
    pub fn replica_open(&mut self) {
        self.replica_references = Some(self.replica_references.unwrap_or(0) + 1);
    }

    fn is_replica_closed(&self) -> bool {
        matches!(self.replica_references, Some(0))
    }

    /// `wait_async` implementation of the replica side of the terminal.
    fn replica_wait_async(
        &self,
        waiter: &Waiter,
        events: FdEvents,
        handler: EventHandler,
    ) -> WaitCanceler {
        self.replica_wait_queue.wait_async_fd_events(waiter, events, handler)
    }

    /// `query_events` implementation of the replica side of the terminal.
    fn replica_query_events(&self) -> FdEvents {
        if self.is_main_closed() {
            return FdEvents::POLLIN | FdEvents::POLLOUT | FdEvents::POLLERR | FdEvents::POLLHUP;
        }
        self.input_queue().read_readyness() | self.output_queue().write_readyness()
    }

    /// `read` implementation of the replica side of the terminal.
    fn replica_read(
        &mut self,
        current_task: &CurrentTask,
        data: &mut dyn OutputBuffer,
    ) -> Result<usize, Errno> {
        if self.is_main_closed() {
            return Ok(0);
        }
        let result = with_queue!(self.input_queue.read(self.as_mut(), current_task, data))?;
        self.notify_waiters();
        Ok(result)
    }

    /// `write` implementation of the replica side of the terminal.
    fn replica_write(
        &mut self,
        current_task: &CurrentTask,
        data: &mut dyn InputBuffer,
    ) -> Result<usize, Errno> {
        if self.is_main_closed() {
            return error!(EIO);
        }
        let (read_from_userspace, signals) =
            with_queue!(self.output_queue.write(self.as_mut(), current_task, data))?;
        assert!(signals.signals().is_empty());
        self.notify_waiters();
        Ok(read_from_userspace)
    }

    /// Returns the input_queue. The Option is always filled, see `input_queue` description.
    fn input_queue(&self) -> &Queue {
        self.input_queue.as_ref().unwrap()
    }

    /// Returns the output_queue. The Option is always filled, see `output_queue` description.
    fn output_queue(&self) -> &Queue {
        self.output_queue.as_ref().unwrap()
    }

    /// Notify any waiters if the state of the terminal changes.
    fn notify_waiters(&mut self) {
        let main_events = self.main_query_events();
        if main_events.bits() != 0 {
            self.main_wait_queue.notify_fd_events(main_events);
        }
        let replica_events = self.replica_query_events();
        if replica_events.bits() != 0 {
            self.replica_wait_queue.notify_fd_events(replica_events);
        }
    }

    /// Return whether a signal must be send when receiving `byte`, and if yes, which.
    fn handle_signals(&mut self, byte: RawByte) -> Option<Signal> {
        if !self.termios.has_local_flags(ISIG) {
            return None;
        }
        self.termios.signal(byte)
    }

    /// Transform the given `buffer` according to the terminal configuration and append it to the
    /// read buffer of the `queue`. The given queue is the input or output queue depending on
    /// `is_input`. The transformation method might update the other queue, but in the case, it is
    /// guaranteed that it won't have to update the initial one recursively. The transformation
    /// might also update the state of the terminal.
    ///
    /// Returns the number of bytes extracted from the queue, as well as the pending signals
    /// following the handling of the buffer.
    fn transform(
        &mut self,
        is_input: bool,
        queue: &mut Queue,
        buffer: &[RawByte],
    ) -> (usize, PendingSignals) {
        if is_input {
            self.transform_input(queue, buffer)
        } else {
            (self.transform_output(queue, buffer), PendingSignals::new())
        }
    }

    /// Transformation method for the output queue. See `transform`.
    fn transform_output(&mut self, queue: &mut Queue, original_buffer: &[RawByte]) -> usize {
        let mut buffer = original_buffer;

        // transform_output is effectively always in noncanonical mode, as the
        // main termios never has ICANON set.

        if !self.termios.has_output_flags(OPOST) {
            queue.read_buffer.extend_from_slice(buffer);
            if !queue.read_buffer.is_empty() {
                queue.readable = true;
            }
            return buffer.len();
        }

        let mut return_value = 0;
        while !buffer.is_empty() {
            let size = compute_next_character_size(buffer, &self.termios);
            let mut character_bytes = buffer[..size].to_vec();
            return_value += size;
            buffer = &buffer[size..];
            match character_bytes[0] {
                b'\n' => {
                    if self.termios.has_output_flags(ONLRET) {
                        self.column = 0;
                    }
                    if self.termios.has_output_flags(ONLCR) {
                        queue.read_buffer.extend_from_slice(&[b'\r', b'\n']);
                        continue;
                    }
                }
                b'\r' => {
                    if self.termios.has_output_flags(ONOCR) && self.column == 0 {
                        continue;
                    }
                    if self.termios.has_output_flags(OCRNL) {
                        character_bytes[0] = b'\n';
                        if self.termios.has_output_flags(ONLRET) {
                            self.column = 0;
                        }
                    } else {
                        self.column = 0;
                    }
                }
                b'\t' => {
                    let spaces = SPACES_PER_TAB - self.column % SPACES_PER_TAB;
                    if self.termios.c_oflag & TABDLY == XTABS {
                        self.column += spaces;
                        queue.read_buffer.extend(std::iter::repeat(b' ').take(SPACES_PER_TAB));
                        continue;
                    }
                    self.column += spaces;
                }
                BACKSPACE_CHAR => {
                    if self.column > 0 {
                        self.column -= 1;
                    }
                }
                _ => {
                    self.column += 1;
                }
            }
            queue.read_buffer.append(&mut character_bytes);
        }
        if !queue.read_buffer.is_empty() {
            queue.readable = true;
        }
        return_value
    }

    /// Transformation method for the input queue. See `transform`.
    fn transform_input(
        &mut self,
        queue: &mut Queue,
        original_buffer: &[RawByte],
    ) -> (usize, PendingSignals) {
        let mut buffer = original_buffer;

        // If there's a line waiting to be read in canonical mode, don't write
        // anything else to the read buffer.
        if self.termios.has_local_flags(ICANON) && queue.readable {
            return (0, PendingSignals::new());
        }

        let max_bytes = if self.termios.has_local_flags(ICANON) {
            CANON_MAX_BYTES
        } else {
            NON_CANON_MAX_BYTES
        };

        let mut return_value = 0;
        let mut signals = PendingSignals::new();
        while !buffer.is_empty() && queue.read_buffer.len() < CANON_MAX_BYTES {
            let size = compute_next_character_size(buffer, &self.termios);
            let mut character_bytes = buffer[..size].to_vec();
            // It is guaranteed that character_bytes has at least one element.
            if let Some(signal) = self.handle_signals(character_bytes[0]) {
                signals.add(signal);
            }
            match character_bytes[0] {
                b'\r' => {
                    if self.termios.has_input_flags(IGNCR) {
                        buffer = &buffer[size..];
                        return_value += size;
                        continue;
                    }
                    if self.termios.has_input_flags(ICRNL) {
                        character_bytes[0] = b'\n';
                    }
                }
                b'\n' => {
                    if self.termios.has_input_flags(INLCR) {
                        character_bytes[0] = b'\r'
                    }
                }
                _ => {}
            }
            // In canonical mode, we discard non-terminating characters
            // after the first 4095.
            if self.termios.has_local_flags(ICANON)
                && queue.read_buffer.len() + size >= max_bytes
                && !self.termios.is_terminating(&character_bytes)
            {
                buffer = &buffer[size..];
                return_value += size;
                continue;
            }

            if queue.read_buffer.len() + size > max_bytes {
                break;
            }

            buffer = &buffer[size..];
            return_value += size;

            let first_byte = character_bytes[0];

            // If we get EOF, make the buffer available for reading.
            if self.termios.has_local_flags(ICANON) && self.termios.is_eof(first_byte) {
                queue.readable = true;
                break;
            }

            let mut maybe_erase_span = None;
            if self.termios.has_local_flags(ICANON) {
                if self.termios.is_erase(first_byte) {
                    maybe_erase_span =
                        Some(compute_last_character_span(&queue.read_buffer[..], &self.termios));
                } else if self.termios.is_werase(first_byte) {
                    maybe_erase_span =
                        Some(compute_last_word_span(&queue.read_buffer[..], &self.termios));
                }
            }

            if let Some(erase_span) = maybe_erase_span {
                if erase_span.bytes == 0 {
                    continue;
                }
                queue.read_buffer.truncate(queue.read_buffer.len() - erase_span.bytes);
            } else {
                queue.read_buffer.extend_from_slice(&character_bytes);
            }

            if self.termios.has_local_flags(ECHOE) {
                track_stub!(TODO("https://fxbug.dev/322874345"), "terminal ECHOE");
            }
            if self.termios.has_local_flags(ECHOPRT) {
                track_stub!(TODO("https://fxbug.dev/322874329"), "terminal ECHOPRT");
            }
            if self.termios.has_local_flags(ECHOK) {
                track_stub!(TODO("https://fxbug.dev/322874293"), "terminal ECHOK");
            }
            if self.termios.has_local_flags(ECHOKE) {
                track_stub!(TODO("https://fxbug.dev/322874191"), "terminal ECHOKE");
            }

            // Anything written to the read buffer will have to be echoed.
            let mut echo_bytes = vec![];
            if self.termios.has_local_flags(ECHO) {
                if let Some(erase_span) = maybe_erase_span {
                    let erase_echo = [BACKSPACE_CHAR, b' ', BACKSPACE_CHAR];
                    echo_bytes = erase_echo
                        .iter()
                        .cycle()
                        .take(erase_echo.len() * erase_span.characters)
                        .map(|c| *c)
                        .collect();
                } else if self.termios.has_local_flags(ECHOCTL)
                    && matches!(first_byte, 0..=0x8 | 0xB..=0xC | 0xE..=0x1F)
                {
                    // If this bit is set and the ECHO bit is also set, echo
                    // control characters with ‘^’ followed by the corresponding
                    // text character. Thus, control-A echoes as ‘^A’. This is
                    // usually the preferred mode for interactive input, because
                    // echoing a control character back to the terminal could have
                    // some undesired effect on the terminal.
                    echo_bytes = vec![b'^', first_byte + CONTROL_OFFSET];
                } else {
                    echo_bytes = character_bytes.clone();
                }
            } else if self.termios.has_local_flags(ECHONL) && first_byte == b'\n' {
                // If this bit is set and the ICANON bit is also set, then the
                // newline ('\n') character is echoed even if the ECHO bit is not set.
                echo_bytes = character_bytes.clone();
            }

            if !echo_bytes.is_empty() {
                signals
                    .append(with_queue!(self.output_queue.write_bytes(self.as_mut(), &echo_bytes)));
            }

            // If we finish a line, make it available for reading.
            if self.termios.has_local_flags(ICANON) && self.termios.is_terminating(&character_bytes)
            {
                queue.readable = true;
                break;
            }
        }
        // In noncanonical mode, everything is readable.
        if !self.termios.has_local_flags(ICANON) && !queue.read_buffer.is_empty() {
            queue.readable = true;
        }

        (return_value, signals)
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        self.state.release_terminal(self.id).unwrap()
    }
}

/// The controlling session of a terminal. Is is associated to a single side of the terminal,
/// either main or replica.
#[derive(Debug)]
pub struct TerminalController {
    pub session: Weak<Session>,
}

impl TerminalController {
    pub fn new(session: &Arc<Session>) -> Option<Self> {
        Some(Self { session: Arc::downgrade(&session) })
    }

    pub fn get_foreground_process_group(&self) -> Option<Arc<ProcessGroup>> {
        self.session.upgrade().and_then(|session| session.read().get_foreground_process_group())
    }
}

/// Helper trait for termios to help parse the configuration.
trait TermIOS {
    fn has_input_flags(&self, flags: tcflag_t) -> bool;
    fn has_output_flags(&self, flags: tcflag_t) -> bool;
    fn has_local_flags(&self, flags: tcflag_t) -> bool;
    fn is_eof(&self, c: RawByte) -> bool;
    fn is_erase(&self, c: RawByte) -> bool;
    fn is_werase(&self, c: RawByte) -> bool;
    fn is_terminating(&self, character_bytes: &[RawByte]) -> bool;
    fn signal(&self, c: RawByte) -> Option<Signal>;
}

impl TermIOS for uapi::termios {
    fn has_input_flags(&self, flags: tcflag_t) -> bool {
        self.c_iflag & flags == flags
    }
    fn has_output_flags(&self, flags: tcflag_t) -> bool {
        self.c_oflag & flags == flags
    }
    fn has_local_flags(&self, flags: tcflag_t) -> bool {
        self.c_lflag & flags == flags
    }
    fn is_eof(&self, c: RawByte) -> bool {
        c == self.c_cc[VEOF as usize] && self.c_cc[VEOF as usize] != DISABLED_CHAR
    }
    fn is_erase(&self, c: RawByte) -> bool {
        c == self.c_cc[VERASE as usize] && self.c_cc[VERASE as usize] != DISABLED_CHAR
    }
    fn is_werase(&self, c: RawByte) -> bool {
        c == self.c_cc[VWERASE as usize]
            && self.c_cc[VWERASE as usize] != DISABLED_CHAR
            && self.has_local_flags(IEXTEN)
    }
    fn is_terminating(&self, character_bytes: &[RawByte]) -> bool {
        // All terminating characters are 1 byte.
        if character_bytes.len() != 1 {
            return false;
        }
        let c = character_bytes[0];

        // Is this the user-set EOF character?
        if self.is_eof(c) {
            return true;
        }

        if c == DISABLED_CHAR {
            return false;
        }
        if c == b'\n' || c == self.c_cc[VEOL as usize] {
            return true;
        }
        if c == self.c_cc[VEOL2 as usize] {
            return self.has_local_flags(IEXTEN);
        }
        false
    }
    fn signal(&self, c: RawByte) -> Option<Signal> {
        if c == self.c_cc[VINTR as usize] {
            return Some(SIGINT);
        }
        if c == self.c_cc[VQUIT as usize] {
            return Some(SIGQUIT);
        }
        if c == self.c_cc[VSUSP as usize] {
            return Some(SIGSTOP);
        }
        None
    }
}

/// Returns the number of bytes of the next character in `buffer`.
///
/// Depending on `termios`, this might consider ASCII or UTF8 encoding.
///
/// This will return 1 if the encoding is UTF8 and the first bytes of buffer are not a valid utf8
/// sequence.
fn compute_next_character_size(buffer: &[RawByte], termios: &uapi::termios) -> usize {
    if !termios.has_input_flags(IUTF8) {
        return 1;
    }

    #[derive(Default)]
    struct Receiver {
        /// Whether the first codepoint has been decoded. Contains `None` until either the first
        /// character has been decoded, or until the sequence is considered invalid. When not None,
        /// it contains `true` if a character has been correctly decoded.
        done: Option<bool>,
    }

    impl utf8parse::Receiver for Receiver {
        fn codepoint(&mut self, _c: char) {
            self.done = Some(true);
        }
        fn invalid_sequence(&mut self) {
            self.done = Some(false);
        }
    }

    let mut byte_count = 0;
    let mut receiver = Receiver::default();
    let mut parser = utf8parse::Parser::new();
    while receiver.done.is_none() && byte_count < buffer.len() {
        parser.advance(&mut receiver, buffer[byte_count]);
        byte_count += 1;
    }
    if receiver.done == Some(true) {
        byte_count
    } else {
        1
    }
}

fn is_ascii(c: RawByte) -> bool {
    c & 0x80 == 0
}

fn is_utf8_start(c: RawByte) -> bool {
    c & 0xC0 == 0xC0
}

#[derive(Default, Debug, Clone, Copy)]
struct BufferSpan {
    bytes: usize,
    characters: usize,
}

impl std::ops::AddAssign<Self> for BufferSpan {
    // Required method
    fn add_assign(&mut self, rhs: Self) {
        self.bytes += rhs.bytes;
        self.characters += rhs.characters;
    }
}

/// Returns size of the last character in `buffer`.
///
/// Depending on `termios`, this might consider ASCII or UTF8 encoding.
fn compute_last_character_span(buffer: &[RawByte], termios: &uapi::termios) -> BufferSpan {
    if buffer.is_empty() {
        return BufferSpan::default();
    }
    if termios.has_input_flags(IUTF8) {
        let mut bytes = 0;
        for c in buffer.iter().rev() {
            bytes += 1;
            if is_ascii(*c) || is_utf8_start(*c) {
                return BufferSpan { bytes, characters: 1 };
            }
        }
        BufferSpan::default()
    } else {
        BufferSpan { bytes: 1, characters: 1 }
    }
}

/// Returns size of the last word in `buffer`.
///
/// Depending on `termios`, this might consider ASCII or UTF8 encoding.
fn compute_last_word_span(buffer: &[RawByte], termios: &uapi::termios) -> BufferSpan {
    fn is_whitespace(c: RawByte) -> bool {
        c == b' ' || c == b'\t'
    }

    let mut in_word = false;
    let mut word_span = BufferSpan::default();
    let mut remaining = buffer.len();
    loop {
        let span = compute_last_character_span(&buffer[..remaining], termios);
        if span.bytes == 0 {
            break;
        }
        if span.bytes == 1 {
            let c = buffer[remaining - 1];
            if in_word {
                if is_whitespace(c) {
                    break;
                }
            } else {
                if !is_whitespace(c) {
                    in_word = true;
                }
            }
        }
        remaining -= span.bytes;
        word_span += span;
    }

    word_span
}

/// Alias used to mark bytes in the queues that have not yet been processed and pushed into the
/// read buffer. See `Queue`.
type RawByte = u8;

/// Queue represents one of the input or output queues between a pty main and replica. Bytes
/// written to a queue are added to the read buffer until it is full, at which point they are
/// written to the wait buffer. Bytes are processed (i.e. undergo termios transformations) as they
/// are added to the read buffer. The read buffer is readable when its length is nonzero and
/// readable is true.
#[derive(Debug, Default)]
pub struct Queue {
    /// The buffer of data ready to be read when readable is true. This data has been processed.
    read_buffer: Vec<u8>,

    /// Data that can't fit into readBuf. It is put here until it can be loaded into the read
    /// buffer. Contains data that hasn't been processed.
    wait_buffers: VecDeque<Vec<RawByte>>,

    /// The length of the data in `wait_buffers`.
    total_wait_buffer_length: usize,

    /// Whether the read buffer can be read from. In canonical mode, there can be an unterminated
    /// line in the read buffer, so readable must be checked.
    readable: bool,

    /// Whether this queue in the input queue. Needed to know how to transform received data.
    is_input: bool,
}

impl Queue {
    fn output_queue() -> Option<Self> {
        Some(Queue { is_input: false, ..Default::default() })
    }

    fn input_queue() -> Option<Self> {
        Some(Queue { is_input: true, ..Default::default() })
    }

    /// Returns whether the queue is ready to be written to.
    fn write_readyness(&self) -> FdEvents {
        if self.total_wait_buffer_length < WAIT_BUFFER_MAX_BYTES {
            FdEvents::POLLOUT
        } else {
            FdEvents::empty()
        }
    }

    /// Returns whether the queue is ready to be read from.
    fn read_readyness(&self) -> FdEvents {
        if self.readable {
            FdEvents::POLLIN
        } else {
            FdEvents::empty()
        }
    }

    /// Returns the number of bytes ready to be read.
    fn readable_size(&self) -> usize {
        if self.readable {
            self.read_buffer.len()
        } else {
            0
        }
    }

    /// Read from the queue into `data`. Returns the number of bytes copied.
    pub fn read(
        &mut self,
        terminal: TerminalStateMutRef<'_>,
        _current_task: &CurrentTask,
        data: &mut dyn OutputBuffer,
    ) -> Result<usize, Errno> {
        if !self.readable {
            return error!(EAGAIN);
        }
        let max_bytes_to_write = std::cmp::min(self.read_buffer.len(), CANON_MAX_BYTES);
        let written_to_userspace = data.write(&self.read_buffer[..max_bytes_to_write])?;
        self.read_buffer.drain(0..written_to_userspace);
        // If everything has been read, this queue is no longer readable.
        if self.read_buffer.is_empty() {
            self.readable = false;
        }

        let signals = self.drain_waiting_buffer(terminal);
        assert!(signals.signals().is_empty());
        Ok(written_to_userspace)
    }

    /// Writes to the queue from `data`. Returns the number of bytes copied.
    pub fn write(
        &mut self,
        terminal: TerminalStateMutRef<'_>,
        _current_task: &CurrentTask,
        data: &mut dyn InputBuffer,
    ) -> Result<(usize, PendingSignals), Errno> {
        let room = WAIT_BUFFER_MAX_BYTES - self.total_wait_buffer_length;
        let data_length = data.available();
        if room == 0 && data_length > 0 {
            return error!(EAGAIN);
        }
        let buffer = data.read_to_vec_exact(std::cmp::min(room, data_length))?;
        let read_from_userspace = buffer.len();
        let signals = self.push_to_waiting_buffer(terminal, buffer);
        Ok((read_from_userspace, signals))
    }

    /// Writes the given `buffer` to the queue.
    fn write_bytes(
        &mut self,
        terminal: TerminalStateMutRef<'_>,
        buffer: &[RawByte],
    ) -> PendingSignals {
        self.push_to_waiting_buffer(terminal, buffer.to_vec())
    }

    /// Pushes the given buffer into the wait_buffers, and process the wait_buffers.
    fn push_to_waiting_buffer(
        &mut self,
        terminal: TerminalStateMutRef<'_>,
        buffer: Vec<RawByte>,
    ) -> PendingSignals {
        self.total_wait_buffer_length += buffer.len();
        self.wait_buffers.push_back(buffer);
        self.drain_waiting_buffer(terminal)
    }

    /// Processes the wait_buffers, filling the read buffer.
    fn drain_waiting_buffer(&mut self, mut terminal: TerminalStateMutRef<'_>) -> PendingSignals {
        let mut total = 0;
        let mut signals_to_return = PendingSignals::new();
        while let Some(wait_buffer) = self.wait_buffers.pop_front() {
            let (count, signals) = terminal.transform(self.is_input, self, &wait_buffer);
            total += count;
            signals_to_return.append(signals);
            if count != wait_buffer.len() {
                self.wait_buffers.push_front(wait_buffer[count..].to_vec());
                break;
            }
        }
        self.total_wait_buffer_length -= total;
        signals_to_return
    }

    /// Called when the queue is moved from canonical mode, to non canonical mode.
    fn on_canon_disabled(&mut self, terminal: TerminalStateMutRef<'_>) -> PendingSignals {
        let signals = self.drain_waiting_buffer(terminal);
        if !self.read_buffer.is_empty() {
            self.readable = true;
        }
        signals
    }
}

// Returns the ASCII representation of the given char. This will assert if the character is not
// ascii.
fn get_ascii(c: char) -> u8 {
    let mut dest: [u8; 1] = [0];
    c.encode_utf8(&mut dest);
    dest[0]
}

// Returns the control character associated with the given letter.
fn get_control_character(c: char) -> cc_t {
    get_ascii(c) - get_ascii('A') + 1
}

// Returns the default control characters of a terminal.
fn get_default_control_characters() -> [cc_t; 19usize] {
    [
        get_control_character('C'),  // VINTR = ^C
        get_control_character('\\'), // VQUIT = ^\
        get_ascii('\x7f'),           // VERASE = DEL
        get_control_character('U'),  // VKILL = ^U
        get_control_character('D'),  // VEOF = ^D
        0,                           // VTIME
        1,                           // VMIN
        0,                           // VSWTC
        get_control_character('Q'),  // VSTART = ^Q
        get_control_character('S'),  // VSTOP = ^S
        get_control_character('Z'),  // VSUSP = ^Z
        0,                           // VEOL
        get_control_character('R'),  // VREPRINT = ^R
        get_control_character('O'),  // VDISCARD = ^O
        get_control_character('W'),  // VWERASE = ^W
        get_control_character('V'),  // VLNEXT = ^V
        0,                           // VEOL2
        0,                           // Remaining data in the array,
        0,                           // Remaining data in the array,
    ]
}

// Returns the default replica terminal configuration.
fn get_default_termios() -> uapi::termios {
    uapi::termios {
        c_iflag: uapi::ICRNL | uapi::IXON,
        c_oflag: uapi::OPOST | uapi::ONLCR,
        c_cflag: uapi::B38400 | uapi::CS8 | uapi::CREAD,
        c_lflag: uapi::ISIG
            | uapi::ICANON
            | uapi::ECHO
            | uapi::ECHOE
            | uapi::ECHOK
            | uapi::ECHOCTL
            | uapi::ECHOKE
            | uapi::IEXTEN,
        c_line: 0,
        c_cc: get_default_control_characters(),
    }
}

#[derive(Debug)]
struct PtsIdsSet {
    pts_count: u32,
    next_id: u32,
    reclaimed_ids: BTreeSet<u32>,
}

impl PtsIdsSet {
    fn new(pts_count: u32) -> Self {
        Self { pts_count, next_id: 0, reclaimed_ids: BTreeSet::new() }
    }

    fn release(&mut self, id: u32) {
        assert!(self.reclaimed_ids.insert(id))
    }

    fn acquire(&mut self) -> Result<u32, Errno> {
        match self.reclaimed_ids.iter().next() {
            Some(e) => {
                let value = *e;
                self.reclaimed_ids.remove(&value);
                Ok(value)
            }
            None => {
                if self.next_id < self.pts_count {
                    let id = self.next_id;
                    self.next_id += 1;
                    Ok(id)
                } else {
                    error!(ENOSPC)
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[::fuchsia::test]
    fn test_ascii_conversion() {
        assert_eq!(get_ascii(' '), 32);
    }

    #[::fuchsia::test]
    fn test_control_character() {
        assert_eq!(get_control_character('C'), 3);
    }

    #[::fuchsia::test]
    #[should_panic]
    fn test_invalid_ascii_conversion() {
        get_ascii('é');
    }

    #[::fuchsia::test]
    fn test_compute_next_character_size_non_utf8() {
        let termios = get_default_termios();
        for i in 0..=255 {
            let array: &[u8] = &[i, 0xa9, 0];
            assert_eq!(compute_next_character_size(array, &termios), 1);
        }
    }

    #[::fuchsia::test]
    fn test_compute_next_character_size_utf8() {
        let mut termios = get_default_termios();
        termios.c_iflag |= IUTF8;
        for i in 0..128 {
            let array: &[RawByte] = &[i, 0xa9, 0];
            assert_eq!(compute_next_character_size(array, &termios), 1);
        }
        let array: &[RawByte] = &[0xc2, 0xa9, 0];
        assert_eq!(compute_next_character_size(array, &termios), 2);
        let array: &[RawByte] = &[0xc2, 255, 0];
        assert_eq!(compute_next_character_size(array, &termios), 1);
    }
}
