// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Event reactors and combinators.
//!
//! This module provides APIs for constructing [`Reactor`] types that respond to [state and data
//! events][`Event`] by configuring and sampling data with [time matrices][`TimeMatrix`].
//!
//! [`Event`]: crate::experimental::event::Event
//! [`Reactor`]: crate::experimental::event::Reactor
//! [`TimeMatrix`]: crate::experimental::series::TimeMatrix

mod builder;
mod reactor;

use crate::experimental::clock::Timed;

pub use crate::experimental::event::builder::{sample_data_record, SampleDataRecord};
pub use crate::experimental::event::reactor::{
    and, fail, filter_map_data_record, map_data_record, map_state, on_data_record, or, respond,
    then, with_state, And, AndChain, Context, Fail, FilterMapDataRecord, Inspect, IntoReactor,
    MapError, MapResponse, Or, OrChain, Reactor, Respond, Then, ThenChain, WithState,
};

/// Extension methods for [`Reactor`] types.
pub trait ReactorExt<T, S = ()>: Reactor<T, S> {
    /// Reacts to a [data record][`DataEvent::record`].
    ///
    /// This function constructs and [reacts to][`Reactor::react`] a data event with the given
    /// record at [`Timestamp::now`].
    ///
    /// [`DataEvent::record`]: crate::experimental::event::DataEvent::record
    /// [`Reactor::react`]: crate::experimental::event::Reactor::react
    /// [`Timestamp::now`]: crate::experimental::clock::Timed
    fn react_to_data_record(&mut self, record: T) -> Result<Self::Response, Self::Error>
    where
        S: Default,
    {
        self.react(Timed::now(DataEvent { record }.into()), Context::from_state(&mut S::default()))
    }
}

impl<R, T> ReactorExt<T> for R where R: Reactor<T, ()> {}

impl<T> Timed<Event<T>> {
    pub(crate) fn to_timed_sample(&self) -> Option<Timed<T>>
    where
        T: Clone,
    {
        self.clone()
            .map(|event| match event {
                Event::Data(DataEvent { record, .. }) => Some(record),
                _ => None,
            })
            .transpose()
    }

    pub fn as_data_record(&self) -> Option<&T> {
        self.inner().as_data_record()
    }

    pub fn map_data_record<U, F>(self, f: F) -> Timed<Event<U>>
    where
        F: FnOnce(T) -> U,
    {
        self.map(move |event| event.map_data_record(f))
    }

    pub fn filter_map_data_record<U, F>(self, f: F) -> Option<Timed<Event<U>>>
    where
        F: FnOnce(T) -> Option<U>,
    {
        self.map_data_record(f).map(Event::transpose).transpose()
    }
}

/// An event that describes a change to [the environment][`SystemEvent`] or the arrival of a [data
/// record][`DataEvent::record`].
///
/// [`DataEvent::record`]: crate::experimental::event::DataEvent::record
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Event<T> {
    System(SystemEvent),
    Data(DataEvent<T>),
}

impl<T> Event<T> {
    pub fn from_data_record(record: T) -> Self {
        Event::Data(DataEvent { record })
    }

    pub fn map_data_record<U, F>(self, f: F) -> Event<U>
    where
        F: FnOnce(T) -> U,
    {
        match self {
            Event::System(event) => Event::System(event),
            Event::Data(event) => Event::Data(event.map(f)),
        }
    }

    pub fn as_data_record(&self) -> Option<&T> {
        match self {
            Event::System(_) => None,
            Event::Data(ref event) => Some(&event.record),
        }
    }
}

impl<T> Event<Option<T>> {
    pub fn transpose(self) -> Option<Event<T>> {
        match self {
            Event::System(event) => Some(Event::System(event)),
            Event::Data(event) => event.record.map(|record| Event::Data(DataEvent { record })),
        }
    }
}

impl<T> From<SystemEvent> for Event<T> {
    fn from(event: SystemEvent) -> Self {
        Event::System(event)
    }
}

impl<T> From<DataEvent<T>> for Event<T> {
    fn from(event: DataEvent<T>) -> Self {
        Event::Data(event)
    }
}

/// Describes a change to the environment that may require reconfiguration.
///
/// System events may change the behavior of a [`Reactor`]. For example, some [`Reactor`]s that
/// configure a [`TimeMatrix`] may apply an alternative interpolation when a [`Sleep`] event is
/// received.
///
/// [`Reactor`]: crate::experimental::event::Reactor
/// [`Sleep`]: crate::experimental::event::SuspendEvent::Sleep
/// [`TimeMatrix`]: crate::experimental::series::TimeMatrix
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum SystemEvent {
    Suspend(SuspendEvent),
}

impl From<SuspendEvent> for SystemEvent {
    fn from(event: SuspendEvent) -> Self {
        SystemEvent::Suspend(event)
    }
}

/// Describes entering and exiting a mode of suspended execution.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum SuspendEvent {
    /// Indicates that the system is entering a mode that suspends execution.
    ///
    /// This event describes a state in which any system clock is inactive. On [`Wake`], there may
    /// be an arbitrarily large difference between [`Timestamp::now`] before and after suspension.
    ///
    /// [`Timestamp::now`]: crate::experimental::clock::Timestamp::now
    /// [`Wake`]: crate::experimental::event::SuspendEvent::Wake
    Sleep,
    /// Indicates that the system has exited [`Sleep`].
    ///
    /// [`Sleep`]: crate::experimental::event::SuspendEvent::Sleep
    Wake,
}

/// Describes an arbitrary event with associated data of interest.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct DataEvent<T> {
    /// The record associated with the event.
    ///
    /// This type typically describes information or metrics associated with the event and can be
    /// sampled by a [`Reactor`] using a [`TimeMatrix`] via combinators like
    /// [`sample_data_record`].
    ///
    /// [`Reactor`]: crate::experimental::event::Reactor
    /// [`sample_data_record`]: crate::experimental::event::sample_data_record
    /// [`TimeMatrix`]: crate::experimental::series::TimeMatrix
    pub record: T,
}

impl<T> DataEvent<T> {
    pub fn map<U, F>(self, f: F) -> DataEvent<U>
    where
        F: FnOnce(T) -> U,
    {
        DataEvent { record: f(self.record) }
    }
}

#[cfg(test)]
pub(crate) mod harness {
    use fuchsia_async as fasync;
    use fuchsia_inspect::{Inspector, Node};
    use futures::task::Poll;
    use futures::Future;
    use std::fmt::Debug;
    use std::marker::PhantomData;
    use std::pin::Pin;

    use crate::experimental::clock::Timed;
    use crate::experimental::event::{self, Context, Event, Reactor};
    use crate::experimental::series::interpolation::LastSample;
    use crate::experimental::series::statistic::{Max, Sum};
    use crate::experimental::series::{FoldError, SamplingProfile};
    use crate::experimental::serve::TimeMatrixClient;

    pub const TIME_ZERO: fasync::MonotonicInstant = fasync::MonotonicInstant::from_nanos(0);
    pub const TIME_ONE_SECOND: fasync::MonotonicInstant =
        fasync::MonotonicInstant::from_nanos(1_000_000_000);

    pub const TEST_NODE_NAME: &str = "event_test_node";

    pub trait ReactorExt<T, S = ()>: Reactor<T, S> {
        /// Asserts that `self` observes the given [`Event`] at least once.
        ///
        /// If `self` is leaked, then this function asserts nothing.
        ///
        /// # Panics
        ///
        /// This function panics if `self` has not observed an event that is partially equivalent
        /// to `expected` when dropped.
        fn assert_observes_event(
            self,
            expected: Event<T>,
        ) -> impl Reactor<T, S, Response = Self::Response, Error = Self::Error>
        where
            Self: Sized,
            T: Clone + Debug + PartialEq,
        {
            #[derive(Debug)]
            struct Assertion<T, R>
            where
                T: Debug,
            {
                reactor: R,
                expected: Event<T>,
                is_observed: bool,
            }

            impl<T, R> Drop for Assertion<T, R>
            where
                T: Debug,
            {
                fn drop(&mut self) {
                    assert!(
                        self.is_observed,
                        "reactor never received an expected event before drop: {:?}",
                        self.expected,
                    );
                }
            }

            impl<T, S, R> Reactor<T, S> for Assertion<T, R>
            where
                T: Debug + PartialEq,
                R: Reactor<T, S>,
            {
                type Response = R::Response;
                type Error = R::Error;

                fn react(
                    &mut self,
                    event: Timed<Event<T>>,
                    context: Context<'_, S>,
                ) -> Result<Self::Response, Self::Error> {
                    if &self.expected == event.inner() {
                        self.is_observed = true;
                    }
                    self.reactor.react(event, context)
                }
            }

            Assertion { reactor: self, expected, is_observed: false }
        }

        /// Asserts that `self` reacts to events exactly `n` times.
        ///
        /// If `self` is leaked, then this function only asserts that `self` reacts to no more than
        /// `n` events.
        ///
        /// # Panics
        ///
        /// This function panics if `self` reacts to more than `n` events or fewer than `n` events
        /// when dropped.
        fn assert_reacts_times(
            self,
            n: usize,
        ) -> impl Reactor<T, S, Response = Self::Response, Error = Self::Error>
        where
            Self: Sized,
        {
            #[derive(Debug)]
            struct Assertion<T, R> {
                reactor: R,
                observed: usize,
                expected: usize,
                phantom: PhantomData<fn() -> T>,
            }

            impl<T, R> Drop for Assertion<T, R> {
                fn drop(&mut self) {
                    assert!(
                        self.observed == self.expected,
                        "reactor received unexpected number of events on drop: \
                         observed {}, but expected {}",
                        self.observed,
                        self.expected,
                    );
                }
            }

            impl<T, S, R> Reactor<T, S> for Assertion<T, R>
            where
                R: Reactor<T, S>,
            {
                type Response = R::Response;
                type Error = R::Error;

                fn react(
                    &mut self,
                    event: Timed<Event<T>>,
                    context: Context<'_, S>,
                ) -> Result<Self::Response, Self::Error> {
                    self.observed =
                        self.observed.checked_add(1).expect("overflow in observed event count");
                    assert!(
                        self.observed <= self.expected,
                        "reactor received unexpected number of events before drop: \
                         observed {}, but expected {}",
                        self.observed,
                        self.expected,
                    );
                    self.reactor.react(event, context)
                }
            }

            Assertion { reactor: self, observed: 0, expected: n, phantom: PhantomData }
        }
    }

    impl<T, S, R> ReactorExt<T, S> for R where R: Reactor<T, S> {}

    /// A data record with counts of transmission outcomes.
    #[derive(Clone, Copy, Debug)]
    pub struct TxCount {
        pub failed: u64,
        pub retried: u64,
    }

    /// Constructs an executor with its clock set to time zero.
    pub fn executor_at_time_zero() -> fasync::TestExecutor {
        let executor = fasync::TestExecutor::new_with_fake_time();
        executor.set_fake_time(TIME_ZERO);
        executor
    }

    /// Constructs an inspector and child node with the name defined by `TEST_NODE_NAME`.
    pub fn inspector_and_test_node() -> (Inspector, Node) {
        let inspector = Inspector::default();
        let node = inspector.root().create_child(TEST_NODE_NAME);
        (inspector, node)
    }

    // This function demonstrates how `Reactor`s can be parameterized and returned from functions.
    // Such `Reactor`s can be further composed as needed.
    /// Constructs a `Reactor` that samples `TxCount` fields.
    pub fn sample_tx_count<'client, 'record>(
        client: &'client TimeMatrixClient,
    ) -> impl Reactor<&'record TxCount, (), Response = (), Error = FoldError> {
        event::on_data_record::<&TxCount, _>(event::then((
            event::map_data_record(
                |count: &TxCount, _| count.failed,
                event::then((
                    event::sample_data_record(Sum::<u64>::default()).in_time_matrix::<LastSample>(
                        &client,
                        "tx_failed_sum",
                        SamplingProfile::granular(),
                        LastSample::or(0u64),
                    ),
                    event::sample_data_record(Max::<u64>::default()).in_time_matrix::<LastSample>(
                        &client,
                        "tx_failed_max",
                        SamplingProfile::granular(),
                        LastSample::or(0u64),
                    ),
                )),
            ),
            event::map_data_record(
                |count: &TxCount, _| count.retried,
                event::sample_data_record(Sum::<u64>::default()).in_time_matrix::<LastSample>(
                    &client,
                    "tx_retried_sum",
                    SamplingProfile::granular(),
                    LastSample::or(0u64),
                ),
            ),
        )))
    }

    /// A `Reactor` of only the unit type `()` that always responds with `Ok`.
    pub const fn respond(_: Timed<Event<()>>, _: Context<'_, ()>) -> Result<(), ()> {
        Ok(())
    }

    /// A `Reactor` of only the unit type `()` that always fails with `Err`.
    pub const fn fail(_: Timed<Event<()>>, _: Context<'_, ()>) -> Result<(), ()> {
        Err(())
    }

    /// Asserts that an Inspect time matrix server future is `Pending` (not terminated).
    pub fn assert_inspect_time_matrix_server_polls_pending(
        executor: &mut fasync::TestExecutor,
        server: &mut Pin<&mut impl Future>,
    ) {
        let Poll::Pending = executor.run_until_stalled(server) else {
            panic!("time matrix inspection server terminated unexpectedly");
        };
    }
}

#[cfg(test)]
mod tests {
    use diagnostics_assertions::{assert_data_tree, AnyBytesProperty};
    use std::pin::pin;

    use crate::experimental::clock::Timed;
    use crate::experimental::event::harness::{self, ReactorExt as _};
    use crate::experimental::event::{
        self, Context, DataEvent, Event, Reactor, ReactorExt as _, SuspendEvent, SystemEvent,
    };
    use crate::experimental::series::interpolation::LastSample;
    use crate::experimental::series::metadata::BitSetMap;
    use crate::experimental::series::statistic::{Max, Sum, Union};
    use crate::experimental::series::SamplingProfile;
    use crate::experimental::serve;

    #[test]
    #[should_panic]
    fn observes_event_assertion_observes_no_such_event_then_panics() {
        let _executor = harness::executor_at_time_zero();

        let mut reactor = harness::respond.assert_observes_event(Event::from_data_record(()));
        let _ = reactor.react(
            Timed::now(SystemEvent::Suspend(SuspendEvent::Sleep).into()),
            Context::from_state(&mut ()),
        );
    }

    #[test]
    #[should_panic]
    fn reacts_times_assertion_reacts_too_few_times_then_panics() {
        let _executor = harness::executor_at_time_zero();

        let mut reactor = harness::respond.assert_reacts_times(2);
        let _ = reactor.react_to_data_record(());
    }

    #[test]
    #[should_panic]
    fn reacts_times_assertion_reacts_too_many_times_then_panics() {
        let _executor = harness::executor_at_time_zero();

        let mut reactor = harness::respond.assert_reacts_times(1);
        let _ = reactor.react_to_data_record(());
        let _ = reactor.react_to_data_record(());
    }

    #[test]
    fn then_combinator_reacts_then_subsequent_reacts_on_ok_and_err() {
        let _executor = harness::executor_at_time_zero();

        let mut reactor =
            harness::respond.assert_reacts_times(1).then(harness::respond.assert_reacts_times(1));
        let _ = reactor.react_to_data_record(());

        let mut reactor =
            harness::fail.assert_reacts_times(1).then(harness::respond.assert_reacts_times(1));
        let _ = reactor.react_to_data_record(());

        let mut reactor = event::then((
            harness::respond.assert_reacts_times(1),
            harness::fail.assert_reacts_times(1),
            harness::respond.assert_reacts_times(1),
        ));
        let _ = reactor.react_to_data_record(());
    }

    #[test]
    fn and_combinator_reacts_then_subsequent_reacts_only_on_ok() {
        let _executor = harness::executor_at_time_zero();

        let mut reactor =
            harness::respond.assert_reacts_times(1).and(harness::respond.assert_reacts_times(1));
        let _ = reactor.react_to_data_record(());

        let mut reactor =
            harness::fail.assert_reacts_times(1).and(harness::respond.assert_reacts_times(0));
        let _ = reactor.react_to_data_record(());

        let mut reactor = event::and((
            harness::respond.assert_reacts_times(1),
            harness::fail.assert_reacts_times(1),
            harness::respond.assert_reacts_times(0),
        ));
        let _ = reactor.react_to_data_record(());
    }

    #[test]
    fn or_combinator_reacts_then_subsequent_reacts_only_on_err() {
        let _executor = harness::executor_at_time_zero();

        let mut reactor =
            harness::respond.assert_reacts_times(1).or(harness::respond.assert_reacts_times(0));
        let _ = reactor.react_to_data_record(());

        let mut reactor =
            harness::fail.assert_reacts_times(1).or(harness::fail.assert_reacts_times(1));
        let _ = reactor.react_to_data_record(());

        let mut reactor = event::or((
            harness::fail.assert_reacts_times(1),
            harness::respond.assert_reacts_times(1),
            harness::respond.assert_reacts_times(0),
        ));
        let _ = reactor.react_to_data_record(());
    }

    #[test]
    fn map_data_record_then_subtree_reacts_to_mapped_record() {
        let _executor = harness::executor_at_time_zero();

        #[derive(Debug, Eq, PartialEq)]
        struct Thread {
            nominal: u128,
            tpi: u128,
        }

        let thread = Thread { nominal: 1, tpi: 8 };
        let mut observed = None;
        let mut reactor = event::on_data_record::<&Thread, _>(event::map_data_record(
            |thread: &Thread, _| &thread.tpi,
            |event: Timed<Event<&u128>>, _: Context<'_, ()>| {
                let (_, event) = event.into();
                if let Event::Data(DataEvent { record: tpi, .. }) = event {
                    observed = Some(*tpi);
                }
                Ok::<_, ()>(())
            },
        ));
        let _ = reactor.react_to_data_record(&thread);
        assert_eq!(observed, Some(8));
    }

    #[test]
    fn retain_record_with_filter_map_data_record_then_subtree_reacts_to_mapped_record() {
        const RECORD: i8 = 0;

        let _executor = harness::executor_at_time_zero();

        let mut observed = None;
        let mut reactor = event::on_data_record::<usize, _>(event::filter_map_data_record(
            // Ignore the `usize` data record and map to `Some` constant `i8`.
            |_: usize, _| Some(RECORD),
            |event: Timed<Event<i8>>, _: Context<'_, ()>| {
                let (_, event) = event.into();
                if let Event::Data(DataEvent { record, .. }) = event {
                    observed = Some(record);
                }
                Ok::<_, ()>(())
            },
        ));
        let _ = reactor.react_to_data_record(0usize);
        assert_eq!(observed, Some(RECORD));
    }

    #[test]
    fn discard_record_with_filter_map_data_record_then_subtree_does_not_react_to_mapped_record() {
        let _executor = harness::executor_at_time_zero();

        let mut reactor = event::on_data_record::<usize, _>(event::filter_map_data_record(
            // Ignore the `usize` data record, map to `()`, and return `None`.
            |_: usize, _| None::<()>,
            harness::respond.assert_reacts_times(0),
        ));
        let _ = reactor.react_to_data_record(0usize);
    }

    // It is important that discarding data events does not interfere with the observation of
    // system events.
    #[test]
    fn discard_record_with_filter_map_data_record_then_subtree_reacts_to_system_event() {
        const SYSTEM_EVENT: SystemEvent = SystemEvent::Suspend(SuspendEvent::Sleep);

        let _executor = harness::executor_at_time_zero();

        let mut observed = None;
        let mut reactor = event::on_data_record::<usize, _>(event::filter_map_data_record(
            // Ignore the `usize` data record, map to `i8`, and return `None`.
            |_: usize, _| None::<i8>,
            |event: Timed<Event<i8>>, _: Context<'_, ()>| {
                let (_, event) = event.into();
                if let Event::System(event) = event {
                    observed = Some(event);
                }
                Ok::<_, ()>(())
            },
        ));
        let _ = reactor.react(Timed::now(SYSTEM_EVENT.into()), Context::from_state(&mut ()));
        // Despite discarding any and all data records, the system event must be observed.
        assert_eq!(observed, Some(SYSTEM_EVENT));
    }

    #[test]
    fn with_state_then_subtree_reacts_to_state() {
        let _executor = harness::executor_at_time_zero();

        #[derive(Debug, Eq, PartialEq)]
        struct ReactorState {
            n: u128,
        }

        let mut observed = None;
        let mut reactor = event::on_data_record::<(), _>(event::with_state(
            ReactorState { n: 8 },
            |_: Timed<Event<()>>, context: Context<'_, ReactorState>| {
                observed = Some(context.state.n);
                Ok::<_, ()>(())
            },
        ));
        let _ = reactor.react_to_data_record(());
        assert_eq!(observed, Some(8));
    }

    #[test]
    fn write_state_then_subtree_reacts_to_written_state() {
        let _executor = harness::executor_at_time_zero();

        let mut reactor = event::on_data_record::<(), _>(event::with_state(
            String::from("hello"),
            event::then((
                {
                    |_: Timed<Event<()>>, context: Context<'_, String>| {
                        assert_eq!(context.state, "hello");
                        *context.state = String::from("goodbye");
                        Ok::<_, ()>(())
                    }
                }
                .assert_reacts_times(1),
                {
                    |_: Timed<Event<()>>, context: Context<'_, String>| {
                        assert_eq!(context.state, "goodbye");
                        Ok::<_, ()>(())
                    }
                }
                .assert_reacts_times(1),
            )),
        ));
        let _ = reactor.react_to_data_record(());
    }

    #[test]
    fn map_state_then_subtree_reacts_to_mapped_state() {
        let _executor = harness::executor_at_time_zero();

        #[derive(Debug, Eq, PartialEq)]
        struct ReactorState {
            n: u128,
        }

        let mut observed = None;
        let mut reactor = event::on_data_record::<(), _>(event::map_state(
            |_| ReactorState { n: 8 },
            |_: Timed<Event<()>>, context: Context<'_, ReactorState>| {
                observed = Some(context.state.n);
                Ok::<_, ()>(())
            },
        ));
        let _ = reactor.react_to_data_record(());
        assert_eq!(observed, Some(8));
    }

    #[test]
    fn construct_reactor_with_samplers_then_inspect_data_tree_contains_buffers() {
        let mut executor = harness::executor_at_time_zero();
        let (inspector, node) = harness::inspector_and_test_node();

        let (client, server) = serve::serve_time_matrix_inspection(node);
        let mut server = pin!(server);
        let _reactor = harness::sample_tx_count(&client);

        executor.set_fake_time(harness::TIME_ONE_SECOND);
        harness::assert_inspect_time_matrix_server_polls_pending(&mut executor, &mut server);
        assert_data_tree!(
            inspector,
            root: contains {
                event_test_node: {
                    tx_failed_sum: {
                        "type": "gauge",
                        "data": AnyBytesProperty,
                    },
                    tx_failed_max: {
                        "type": "gauge",
                        "data": AnyBytesProperty,
                    },
                    tx_retried_sum: {
                        "type": "gauge",
                        "data": AnyBytesProperty,
                    },
                },
            }
        );
    }

    #[test]
    fn construct_reactor_with_metadata_then_inspect_data_tree_contains_metadata() {
        use Connectivity::Idle;

        #[derive(Clone, Copy, Debug, Eq, PartialEq)]
        #[repr(u64)]
        enum Connectivity {
            Idle = 1 << 0,
            Disconnected = 1 << 1,
            Connected = 1 << 2,
        }

        let mut executor = harness::executor_at_time_zero();
        let (inspector, node) = harness::inspector_and_test_node();

        let (client, server) = serve::serve_time_matrix_inspection(node);
        let mut server = pin!(server);
        let _reactor = event::on_data_record::<Connectivity, _>(event::map_data_record(
            |connectivity, _| connectivity as u64,
            event::sample_data_record(Union::<u64>::default())
                .with_metadata(BitSetMap::from_ordered(["idle", "disconnected", "connected"]))
                .in_time_matrix::<LastSample>(
                    &client,
                    "connectivity",
                    SamplingProfile::granular(),
                    LastSample::or(Idle as u64),
                ),
        ));

        executor.set_fake_time(harness::TIME_ONE_SECOND);
        harness::assert_inspect_time_matrix_server_polls_pending(&mut executor, &mut server);
        assert_data_tree!(
            inspector,
            root: contains {
                event_test_node: {
                    connectivity: {
                        "type": "bitset",
                        "data": AnyBytesProperty,
                        metadata: {
                            index: {
                                "0": "idle",
                                "1": "disconnected",
                                "2": "connected",
                            }
                        }
                    },
                },
            }
        );
    }

    #[test]
    fn sample_data_record_fields_with_reactor_then_reacts_one_time_with_mapped_fields() {
        let executor = harness::executor_at_time_zero();
        let (_inspector, node) = harness::inspector_and_test_node();

        let (client, _server) = serve::serve_time_matrix_inspection(node);
        let mut reactor = event::on_data_record::<&harness::TxCount, _>(event::then((
            event::map_data_record(
                |count: &harness::TxCount, _| count.failed,
                event::then((
                    event::sample_data_record(Sum::<u64>::default())
                        .in_time_matrix::<LastSample>(
                            &client,
                            "tx_failed_sum",
                            SamplingProfile::granular(),
                            LastSample::or(0u64),
                        )
                        .assert_observes_event(Event::from_data_record(1))
                        .assert_reacts_times(1),
                    event::sample_data_record(Max::<u64>::default())
                        .in_time_matrix::<LastSample>(
                            &client,
                            "tx_failed_max",
                            SamplingProfile::granular(),
                            LastSample::or(0u64),
                        )
                        .assert_observes_event(Event::from_data_record(1))
                        .assert_reacts_times(1),
                )),
            ),
            event::map_data_record(
                |count: &harness::TxCount, _| count.retried,
                event::sample_data_record(Sum::<u64>::default())
                    .in_time_matrix::<LastSample>(
                        &client,
                        "tx_retried_sum",
                        SamplingProfile::granular(),
                        LastSample::or(0u64),
                    )
                    .assert_observes_event(Event::from_data_record(3))
                    .assert_reacts_times(1),
            ),
        )));

        executor.set_fake_time(harness::TIME_ONE_SECOND);
        reactor.react_to_data_record(&harness::TxCount { failed: 1, retried: 3 }).unwrap();
    }
}
