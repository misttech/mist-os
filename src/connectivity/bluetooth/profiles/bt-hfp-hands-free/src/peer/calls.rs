// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! At a high level, the primary element of this module is the `Calls` struct.
//! This is a container for a number of `Call` structs.
//!
//! Each `Call` is a `Stream` of `ProcedureInputs` which are used to drive the
//! current running procedure or initiate a new one.  Internally, the `Call`
//! wraps a request stream for the `Call` protocol which allows FIDL clients to
//! control the call.
//!
//!  For example, a headset or carkit may send a FIDL request to hang up an in
//!  progress call, and this would eventually cause the corresponding `Call`
//!  stream to yield the appropriate `ProcedureInput`,
//!  `ProcedureInput::CommandFromHf(CommandFromHf::HangupCall)`.  Additionally
//!  `Call` has several methods on it that allow the `ProcedureManager` to
//!  manipulate it, such as by setting the phone number associated with the
//!  call.
//!
//!  However, the `Call` struct is not public--instead it is accessed through
//!  the `Calls` struct, which contains zero or more `Call` structs. The `Calls`
//!  struct also is a `Stream`.  It multiplexes the `Stream`s of the internal
//!  `Call` structs.  Similarly, the `Calls` struct has several public methods
//!  to set `Call` state that are routed to the proper `Call`.

use anyhow::{format_err, Error};
use async_helpers::maybe_stream::MaybeStream;
use bt_hfp::call::list::{Idx as CallIdx, List as CallList};
use bt_hfp::call::{indicators as call_indicators, Direction, Number};
use fidl_fuchsia_bluetooth_hfp::{
    CallDirection, CallMarker, CallRequest, CallRequestStream, CallState, CallWatchStateResponder,
    NextCall, PeerHandlerWatchNextCallResponder,
};
use fuchsia_bluetooth::types::PeerId;
use futures::{Stream, StreamExt};
use log::{debug, error, info, warn};
use std::fmt;
use std::pin::Pin;
use std::task::{Context, Poll};

use crate::one_to_one::OneToOneMatcher;
use crate::peer::ag_indicators::CallIndicator;
use crate::peer::procedure::ProcedureInput;

type ResponderResult = Result<(), fidl::Error>;
type WatchStateMatcher = OneToOneMatcher<CallState, CallWatchStateResponder, ResponderResult>;

/// This struct contains information about individual calls and methods to
/// manipulate that state.  Additionally, it acts as a stream of `ProcedureInput`
/// which translate incoming FIDL requests to change the call's state.
struct Call {
    peer_id: PeerId,             // Used for logging
    call_index: Option<CallIdx>, // Set once the call is inserted in a CallList

    state: Option<CallState>, // Set to Some when we get any +CIEVs for this call.
    // For incoming calls, set to Some when we get a +CLIP.  For incoming calls,
    // set to Some initially.
    number: Option<Number>,
    direction: Direction,

    // This is set to None when creating a Call, and then set to Some when a
    // request stream is created when creating the NextCall to yield to a
    // hanging get call to WatchNextCall.
    request_stream: MaybeStream<CallRequestStream>,

    watch_state_hanging_get_matcher: WatchStateMatcher,
}

fn respond_to_watch_call_state(
    state: CallState,
    responder: CallWatchStateResponder,
) -> Result<(), fidl::Error> {
    responder.send(state)
}

impl Call {
    #[allow(unused)]
    pub fn new(peer_id: PeerId, number_option: Option<Number>, direction: Direction) -> Self {
        let watch_state_hanging_get_matcher = OneToOneMatcher::new(respond_to_watch_call_state);

        Self {
            peer_id,
            call_index: None,
            state: None,
            number: number_option,
            direction,
            request_stream: MaybeStream::default(),
            watch_state_hanging_get_matcher,
        }
    }

    pub fn set_call_index(&mut self, call_index: CallIdx) {
        self.call_index = Some(call_index);
    }

    pub fn set_number(&mut self, number: Number) {
        info!("Setting {:?} for peer {:} for call {:?}.", number, self.peer_id, self.call_index);
        self.number = Some(number);
    }

    pub fn set_state(&mut self, new_state: CallState) {
        info!(
            "Setting call state {:?} for peer {:} for call {:?}.",
            new_state, self.peer_id, self.call_index
        );

        self.state = Some(new_state);
        self.watch_state_hanging_get_matcher.enqueue_left(new_state);
    }

    fn handle_watch_state(&mut self, responder: CallWatchStateResponder) {
        info!(
            "Handling Call::WatchState for peer {:} for call {:?}.",
            self.peer_id, self.call_index
        );

        // Enqueue the WatchState responder.  This will matach with any current or future enqueued
        // call states changes and respond to the hanging get.
        self.watch_state_hanging_get_matcher.enqueue_right(responder);
    }

    fn call_request_to_procedure_input(_call_request: CallRequest) -> ProcedureInput {
        unimplemented!()
    }

    /// Generate a NextCall if possible. This is possible if the number and
    /// state fields have been set and the request_stream field has not yet been
    /// set when creating a previous NextCall.
    ///
    /// Failure to generate a NextCall is indicated by the Err branch of a
    /// Result but is not a true error; it just means the information needed to
    /// generate a NextCall hasn't been provided yet or that a NextCall has
    /// already been crated for this Call.
    pub fn possibly_generate_next_call(&mut self) -> Result<NextCall, Error> {
        // TODO (https://fxbug.dev/135158) It's not clear we will always have a number for all calls, so handle,
        // that case.
        let result = match (self.state.is_some(), self.number.is_some(), !self.request_stream.is_some()) {
            (false, _, _) => Err(format_err!("Call {:?} does not yet have a state.", self )),
            (true, false, _) => Err(format_err!("Call {:?} does not yet have a number.", self)),
            (true, true, false) =>
                Err(format_err!(
                    "Call {:?} is missing client_end, indicating that this call has already been converted to a NextCall",
                    self)),
            (true, true, true) => {
                let (client_end, server_end) = fidl::endpoints::create_endpoints::<CallMarker>();
                self.request_stream.set(server_end.into_stream());

                let number = String::from(self.number.clone().expect("Number should be set."));
                let state = self.state.expect("State should be set.");
                let direction = CallDirection::from(self.direction);
                Ok(NextCall {
                    call: Some(client_end),
                    remote: Some(number),
                    state: Some(state),
                    direction: Some(direction),
                    ..Default::default()
                })
            }
        };

        result
    }
}

impl fmt::Debug for Call {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Call")
            .field("peer_id", &format!("{:}", &self.peer_id))
            .field("call_index", &self.call_index)
            .field("state", &self.state)
            .field("number", &self.number)
            .field("direction", &self.direction)
            .field("request_stream.is_some()", &self.request_stream.is_some())
            .finish_non_exhaustive()
    }
}

/// Stream of procedure inputs generated by converting the underlying Call protocol
/// FIDL request stream into the procedure input needed to start the procedure
/// that was requested.  This will also drive reporting state changes for a given
/// call on a WatchNextCall hanging get request.
impl Stream for Call {
    type Item = Result<ProcedureInput, Error>;

    fn poll_next(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let call_request = self.request_stream.poll_next_unpin(context);
        if let Poll::Ready(_) = call_request {
            info!(
                "Received call request {:?} for peer {:} for call {:?}.",
                call_request, self.peer_id, self.call_index
            );
        }

        let item = match call_request {
            Poll::Pending => Poll::Pending, // Stream contained nothing, but has registered waker
            Poll::Ready(None) => Poll::Ready(None), // Stream was terminated
            Poll::Ready(Some(Err(error))) => Poll::Ready(Some(Err(error.into()))), // FIDL error, which is converted to anyhow
            Poll::Ready(Some(Ok(CallRequest::WatchState { responder }))) => {
                self.handle_watch_state(responder);
                Poll::Pending
            }
            Poll::Ready(Some(Ok(CallRequest::SendDtmfCode { code: _, responder }))) => {
                // TODO(https://fxbug.dev/129577) Implement DTMF codes.
                warn!("Unimplemented: send DTMF command");
                let _result = responder.send(Ok(()));
                Poll::Pending
            }
            Poll::Ready(Some(Ok(state_update_request))) => {
                let procedure_input = Self::call_request_to_procedure_input(state_update_request);
                Poll::Ready(Some(Ok(procedure_input)))
            }
        };

        // Respond to any outstanding WatchState requests if we can.
        while let Poll::Ready(Some(result)) =
            self.watch_state_hanging_get_matcher.poll_next_unpin(context)
        {
            if let Err(err) = result {
                return Poll::Ready(Some(Err(err.into())));
            }
        }

        item
    }
}

type NextCallMatcher =
    OneToOneMatcher<NextCall, PeerHandlerWatchNextCallResponder, ResponderResult>;

/// This struct contains a list of `Call`s and methods to manipulate their
/// state.  Additionally, it acts as a stream of `ProcedureInput` by
/// multiplexing the underlying Call`s' streams.
pub struct Calls {
    #[allow(unused)]
    peer_id: PeerId,
    call_list: CallList<Call>,
    watch_next_call_hanging_get_matcher: NextCallMatcher,
}

fn respond_to_watch_next_call(
    call: NextCall,
    responder: PeerHandlerWatchNextCallResponder,
) -> Result<(), fidl::Error> {
    responder.send(call)
}

impl Calls {
    #[allow(unused)]
    pub fn new(peer_id: PeerId) -> Self {
        let watch_next_call_hanging_get_matcher = OneToOneMatcher::new(respond_to_watch_next_call);
        Self { peer_id, call_list: CallList::default(), watch_next_call_hanging_get_matcher }
    }

    #[allow(unused)]
    pub fn insert_new_call(
        &mut self,
        number_option: Option<Number>,
        direction: Direction,
    ) -> CallIdx {
        // TODO(https://fxbug.dev/135119) Handle multiple calls
        if self.call_list.len() > 0 {
            unimplemented!(
                "Inserting new call for peer {:} when calls currently exist: {:?}",
                self.peer_id,
                self.call_list
            );
        }

        let call = Call::new(self.peer_id, number_option, direction);
        let call_index = self.call_list.insert(call);

        let call = self.call_list.get_mut(call_index);
        let call = call.expect("Call was just inserted and so must be present.");
        call.set_call_index(call_index);

        info!("Inserted call {:?} for peer {:}.", call, self.peer_id);

        call_index
    }

    #[allow(unused)]
    pub fn remove_current_call(&mut self) {
        // TODO(https://fxbug.dev/135119) Handle multiple calls
        let idx = 1; // Calls are 1-indexed
        let call_option = self.call_list.get_mut(idx);
        match call_option {
            Some(call) => info!("Removing call {:?} for peer {:?}", self.peer_id, call),
            None => warn!(
                "No call found for for peer {:} at index {:?} while removing call.",
                self.peer_id, idx
            ),
        }
        if self.call_list.remove(idx).is_none() {
            warn!("Attempted to remove unknown call {:} from peer {:}.", idx, self.peer_id);
        }
    }

    #[allow(unused)]
    pub fn set_call_state_by_indicator(&mut self, indicator: CallIndicator) {
        debug!(
            "Setting call state for peer {:} with indicator {:?} for call list {:?}",
            self.peer_id, indicator, self.call_list
        );

        // TODO(https://fxbug.dev/135119) Handle multiple calls.
        // In the future, this will need to find the oldest call that this indicator could apply
        // to and compute the state update it causes for that call.  The calls module in the AG
        // component has several methods to help with with that; these should be factored out into
        // the bt_hfp crate.
        let idx = 1; // Calls are 1-indexed

        let call_state = match indicator {
            CallIndicator::Call(call_indicators::Call::None) => Some(CallState::Terminated),
            CallIndicator::Call(call_indicators::Call::Some) => Some(CallState::OngoingActive),
            // TODO(https://fxbug.dev/135119) Handle multiple calls.
            CallIndicator::CallHeld(_) => {
                error!(
                    "Receeived indicator {:?} for peer {:} but call holding unimplemented.",
                    indicator, self.peer_id
                );
                None
            }
            // CallSetup::None indicates the end of the call setup and not a new state, which
            // should be set by a Call or CallHeld indicator.
            CallIndicator::CallSetup(call_indicators::CallSetup::None) => None,
            CallIndicator::CallSetup(call_indicators::CallSetup::Incoming) => {
                Some(CallState::IncomingRinging)
            }
            CallIndicator::CallSetup(call_indicators::CallSetup::OutgoingDialing) => {
                Some(CallState::OutgoingDialing)
            }
            CallIndicator::CallSetup(call_indicators::CallSetup::OutgoingAlerting) => {
                Some(CallState::OutgoingAlerting)
            }
        };

        debug!(
            "Setting call state for peer {:} for call {:} to {:?}",
            self.peer_id, idx, call_state
        );
        call_state.into_iter().for_each(|s| self.set_state_for_call(idx, s));
    }

    fn set_state_for_call(&mut self, idx: CallIdx, new_state: CallState) {
        let call_option = self.call_list.get_mut(idx);
        debug!(
            "Setting call {} -> {:?} for peer {:} state to {:?}",
            idx, call_option, self.peer_id, new_state
        );
        match call_option {
            Some(call) => {
                call.set_state(new_state);
                Self::possibly_respond_to_watch_next_call(
                    self.peer_id,
                    &mut self.watch_next_call_hanging_get_matcher,
                    call,
                )
            }
            None => warn!(
                "No call found for for peer {:} at index {:} while setting state to {:?}",
                self.peer_id, idx, new_state
            ),
        }

        if new_state == CallState::Terminated {
            debug!("Removing call {:?} for peer {:}.", idx, self.peer_id);
            let remove_option = self.call_list.remove(idx);
            if let None = remove_option {
                warn!("Removed call {:?} for peer {:} but no call was present.", idx, self.peer_id);
            }
        }
    }

    #[allow(unused)]
    pub fn set_number_for_current_call(&mut self, number: Number) {
        // TODO(https://fxbug.dev/135119) Handle multiple calls
        let idx = 1; // Calls are 1-indexed
        let call_option = self.call_list.get_mut(idx);
        match call_option {
            Some(call) => {
                call.set_number(number);
                Self::possibly_respond_to_watch_next_call(
                    self.peer_id,
                    &mut self.watch_next_call_hanging_get_matcher,
                    call,
                )
            }
            None => warn!(
                "No call found for for peer {:} at index {:} while setting number to {:?}",
                self.peer_id, idx, number
            ),
        }
    }

    #[allow(unused)]
    pub fn handle_watch_next_call(&mut self, responder: PeerHandlerWatchNextCallResponder) {
        self.watch_next_call_hanging_get_matcher.enqueue_right(responder);
    }

    fn possibly_respond_to_watch_next_call(
        peer_id: PeerId,
        matcher: &mut NextCallMatcher,
        call: &mut Call,
    ) {
        let next_call_result = call.possibly_generate_next_call();
        match next_call_result {
            Ok(next_call) => {
                debug!("Enqueueing WatchNextCall response {:?} for peer {:}", next_call, peer_id);
                matcher.enqueue_left(next_call);
            }
            Err(err) => {
                // This isn't a real error but just indicates we don't have all the information we
                // need for the NextCall yet, or we have already sent one.
                debug!(
                    "Unable to generate WatchNextCall response for peer {:}: {:?}",
                    peer_id, err
                );
            }
        }
    }
}

/// Produces a single stream by selecting over all the streams for each call.
impl Stream for Calls {
    type Item = Result<ProcedureInput, Error>;

    fn poll_next(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        // Respond to any outstanding WatchNextCall requests if we can.
        while let Poll::Ready(Some(result)) =
            self.watch_next_call_hanging_get_matcher.poll_next_unpin(context)
        {
            if let Err(err) = result {
                return Poll::Ready(Some(Err(err.into())));
            }
        }

        // TODO(https://fxbug.dev/135119) Handle multiple calls.
        let idx = 1; // Calls are 1-indexed
        let call_option = self.call_list.get_mut(idx);
        let call = match call_option {
            Some(call) => call,
            None => {
                debug!(
                    "No call found for for peer {:} at index {:} while polling calls.",
                    self.peer_id, idx
                );
                return Poll::Pending;
            }
        };

        call.poll_next_unpin(context)
    }
}

#[cfg(test)]
mod test {
    // TODO(https://fxbug.dev/410610394) Add more tests.

    use super::*;

    use assert_matches::assert_matches;
    use fidl_fuchsia_bluetooth_hfp as fidl_hfp;
    use futures::future::{select, Either, FutureExt};

    static PEER_ID: PeerId = PeerId(1);
    fn phone_number() -> Number {
        Number::from("8005550100")
    }

    #[fuchsia::test]
    async fn call_created_with_phone_number() {
        let mut calls = Calls::new(PEER_ID);

        let (peer_handler_proxy, mut peer_handler_request_stream) =
            fidl::endpoints::create_proxy_and_stream::<fidl_hfp::PeerHandlerMarker>();

        let watch_next_call_fut = peer_handler_proxy.watch_next_call();
        let watch_next_call_request_fut = peer_handler_request_stream.next();

        let (watch_next_call_request_result_option, watch_next_call_continue_fut) =
            match select(watch_next_call_fut, watch_next_call_request_fut)
                .now_or_never()
                .expect("Select hanging")
            {
                Either::Left(_) => panic!("WatchNextCall future terminated early."),
                Either::Right((req, wnc)) => (req, wnc),
            };
        let watch_next_call_request = watch_next_call_request_result_option
            .expect("Call request tream closed")
            .expect("FIDL error on CallRequestStream");

        let watch_next_call_responder = match watch_next_call_request {
            fidl_hfp::PeerHandlerRequest::WatchNextCall { responder } => responder,
            req => panic!("Unexpected PeerHandler request {req:?}."),
        };

        let _call_index = calls.insert_new_call(Some(phone_number()), Direction::MobileOriginated);

        calls.handle_watch_next_call(watch_next_call_responder);

        calls.set_call_state_by_indicator(CallIndicator::Call(call_indicators::Call::Some));

        // Pump stream to respond to WatchNextCall
        let procedure_input_option = calls.next().now_or_never();
        // No calls have been returned to client yet with WatchNextCall
        assert_matches!(procedure_input_option, None);

        let next_call = watch_next_call_continue_fut
            .now_or_never()
            .expect("watch_next_call hanging")
            .expect("FIDL Error on watch_next_call");
        let call_proxy = next_call.call.expect("Missing client end").into_proxy();

        let watch_state_fut = call_proxy.watch_state();

        // Pump stream to respond to WatchState
        let procedure_input_option = calls.next().now_or_never();
        // No FIDL calls causing a procedure update have happened yet.
        assert_matches!(procedure_input_option, None);

        let state = watch_state_fut
            .now_or_never()
            .expect("watch_state hanging")
            .expect("FIDL error on watch_state");
        assert_eq!(state, CallState::OngoingActive);
    }

    #[fuchsia::test]
    async fn call_created_without_phone_number() {
        let mut calls = Calls::new(PEER_ID);

        let (peer_handler_proxy, mut peer_handler_request_stream) =
            fidl::endpoints::create_proxy_and_stream::<fidl_hfp::PeerHandlerMarker>();

        let watch_next_call_fut = peer_handler_proxy.watch_next_call();
        let watch_next_call_request_fut = peer_handler_request_stream.next();

        let (watch_next_call_request_result_option, watch_next_call_continue_fut) =
            match select(watch_next_call_fut, watch_next_call_request_fut)
                .now_or_never()
                .expect("Select hanging")
            {
                Either::Left(_) => panic!("WatchNextCall future terminated early."),
                Either::Right((req, wnc)) => (req, wnc),
            };
        let watch_next_call_request = watch_next_call_request_result_option
            .expect("Call request tream closed")
            .expect("FIDL error on CallRequestStream");

        let watch_next_call_responder = match watch_next_call_request {
            fidl_hfp::PeerHandlerRequest::WatchNextCall { responder } => responder,
            req => panic!("Unexpected PeerHandler request {req:?}."),
        };

        let _call_index = calls.insert_new_call(None, Direction::MobileOriginated);

        calls.handle_watch_next_call(watch_next_call_responder);

        calls.set_call_state_by_indicator(CallIndicator::Call(call_indicators::Call::Some));

        // Pump stream to respond to WatchNextCall
        let procedure_input_option = calls.next().now_or_never();
        // No calls have been returned to client yet with WatchNextCall
        assert_matches!(procedure_input_option, None);

        let next_call_hang = watch_next_call_continue_fut.now_or_never();
        // The NextcCall is never ready to be sent to clients.
        assert_matches!(next_call_hang, None);
    }
}
