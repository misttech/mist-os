// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::experimental::clock::Timed;
use crate::experimental::event::Event;

/// A type that can be converted into a [`Reactor`].
///
/// This trait is notably implemented for collection types of [`Reactor`]s for chaining. For
/// example, `IntoReactor` is implemented for tuples of [`Reactor`] types in [`ThenChain`] and so
/// such tuples can be used in functions like [`then`] to ergonomically sequence a chain of
/// [`Reactor`]s.
///
/// [`Reactor`]: crate::experimental::event::Reactor
/// [`then`]: crate::experimental::event::then
/// [`ThenChain`]: crate::experimental::event::ThenChain
pub trait IntoReactor<T, S = ()> {
    type Reactor: Reactor<T, S>;

    fn into_reactor(self) -> Self::Reactor;
}

/// A type that reacts to [timed][`Timed`] [`Event`]s.
///
/// A reactor is a function that responds to [system][`SystemEvent`] and [data][`DataEvent`]
/// events. Reactors are formed from combinators, which compose behaviors.
///
/// [`DataEvent`]: crate::experimental::event::DataEvent
/// [`Event`]: crate::experimental::event::Event
/// [`SystemEvent`]: crate::experimental::event::SystemEvent
/// [`Timed`]: crate::experimental::clock::Timed
pub trait Reactor<T, S = ()> {
    /// The output type of successful responses from the reactor.
    type Response;
    /// The error type of failed responses from the reactor.
    type Error;

    /// Reacts to a [timed][`Timed`] [`Event`].
    ///
    /// # Errors
    ///
    /// Returns an [`Error`] if the reaction fails or the reactor cannot otherwise respond to the
    /// event. Errors conditions are defined by implementations.
    ///
    /// [`Event`]: crate::experimental::event::Event
    /// [`Timed`]: crate::experimental::clock::Timed
    fn react(
        &mut self,
        event: Timed<Event<T>>,
        context: Context<'_, S>,
    ) -> Result<Self::Response, Self::Error>;

    fn map_response<P, F>(self, f: F) -> MapResponse<Self, F>
    where
        Self: Sized,
        F: FnMut(Self::Response) -> P,
    {
        MapResponse { reactor: self, f }
    }

    fn map_error<E, F>(self, f: F) -> MapError<Self, F>
    where
        Self: Sized,
        F: FnMut(Self::Error) -> E,
    {
        MapError { reactor: self, f }
    }

    /// Reacts with this reactor and then responds with the given `response`, regardless of this
    /// reactor's output.
    fn respond<P>(self, response: P) -> Respond<Self, P>
    where
        Self: Sized,
        P: Clone,
    {
        Respond { reactor: self, response }
    }

    /// Reacts with this reactor and then fails with the given `error`, regardless of this
    /// reactor's output.
    fn fail<E>(self, error: E) -> Fail<Self, E>
    where
        Self: Sized,
        E: Clone,
    {
        Fail { reactor: self, error }
    }

    /// Reacts with this reactor and then the given reactor (regardless of outputs).
    ///
    /// The constructed reactor returns the output of the given (subsequent) reactor. See also the
    /// [`event::then`] function.
    ///
    /// [`event::then`]: crate::experimental::event::then
    fn then<R>(self, reactor: R) -> Then<Self, R>
    where
        Self: Sized,
        T: Clone,
        R: Reactor<T, S>,
    {
        Then { reactor: self, then: reactor }
    }

    /// Reacts with this reactor and then the given reactor if and only if this reactor returns
    /// `Ok`.
    ///
    /// The constructed reactor returns either an error from this reactor or the output of the
    /// given (subsequent) reactor. See also the [`event::and`] function.
    ///
    /// [`event::and`]: crate::experimental::event::and
    fn and<R>(self, reactor: R) -> And<Self, R>
    where
        Self: Sized,
        Self::Error: From<R::Error>,
        T: Clone,
        R: Reactor<T, S>,
    {
        And { reactor: self, and: reactor }
    }

    /// Reacts with this reactor and then the given reactor if and only if this reactor returns
    /// `Err`.
    ///
    /// The constructed reactor returns either a response from this reactor or the output of the
    /// given (subsequent) reactor. See also the [`event::or`] function.
    ///
    /// [`event::or`]: crate::experimental::event::or
    fn or<R>(self, reactor: R) -> Or<Self, R>
    where
        Self: Sized,
        T: Clone,
        R: Reactor<T, S, Response = Self::Response>,
    {
        Or { reactor: self, or: reactor }
    }

    /// Constructs a `Reactor` that inspects the event and output of `self` with the given
    /// function.
    fn inspect<F>(self, f: F) -> impl Reactor<T, S, Response = Self::Response, Error = Self::Error>
    where
        Self: Sized,
        T: Clone,
        F: FnMut(&Timed<Event<T>>, &Result<Self::Response, Self::Error>),
    {
        Inspect { reactor: self, f }
    }
}

impl<T, S, R, E, F> Reactor<T, S> for F
where
    F: FnMut(Timed<Event<T>>, Context<'_, S>) -> Result<R, E>,
{
    type Response = R;
    type Error = E;

    fn react(
        &mut self,
        event: Timed<Event<T>>,
        context: Context<'_, S>,
    ) -> Result<Self::Response, Self::Error> {
        (self)(event, context)
    }
}

#[derive(Debug)]
pub struct Context<'s, S = ()> {
    pub state: &'s mut S,
}

impl<'s, S> Context<'s, S> {
    pub fn from_state(state: &'s mut S) -> Self {
        Context { state }
    }

    pub fn with_state<'q, U>(self, state: &'q mut U) -> Context<'q, U> {
        Context { state }
    }

    // Note too that this function must copy or clone any non-reference fields.
    /// Reborrows the interior of the `Context`.
    ///
    /// This function constructs a `Context` from this one by reborrowing its reference fields. In
    /// particular, this reborrows `state` and provides an ergonomic way to forward a `Context`
    /// from one `Reactor` to another. It is **not** the `Context` that is borrowed in
    /// `Reactor::react`, but the **fields** (`state`).
    pub fn reborrow<'q>(&'q mut self) -> Context<'q, S>
    where
        's: 'q,
    {
        Context { state: self.state }
    }
}

// This type merely forwards events to an inner `Reactor` with no additional behavior. It provides
// an entrypoint for constructing `Reactor`s with the `on_data_record` function, which closes the
// data record type `T`.
#[derive(Clone, Copy, Debug)]
#[repr(transparent)]
pub struct OnDataRecord<R> {
    reactor: R,
}

impl<T, S, R> Reactor<T, S> for OnDataRecord<R>
where
    R: Reactor<T, S>,
{
    type Response = R::Response;
    type Error = R::Error;

    #[inline(always)]
    fn react(
        &mut self,
        event: Timed<Event<T>>,
        context: Context<'_, S>,
    ) -> Result<Self::Response, Self::Error> {
        self.reactor.react(event, context)
    }
}

#[derive(Clone, Copy, Debug)]
pub struct MapResponse<R, F> {
    reactor: R,
    f: F,
}

impl<T, S, R, O, F> Reactor<T, S> for MapResponse<R, F>
where
    R: Reactor<T, S>,
    F: FnMut(R::Response) -> O,
{
    type Response = O;
    type Error = R::Error;

    fn react(
        &mut self,
        event: Timed<Event<T>>,
        context: Context<'_, S>,
    ) -> Result<Self::Response, Self::Error> {
        self.reactor.react(event, context).map(|response| (self.f)(response))
    }
}

#[derive(Clone, Copy, Debug)]
pub struct MapError<R, F> {
    reactor: R,
    f: F,
}

impl<T, S, R, O, F> Reactor<T, S> for MapError<R, F>
where
    R: Reactor<T, S>,
    F: FnMut(R::Error) -> O,
{
    type Response = R::Response;
    type Error = O;

    fn react(
        &mut self,
        event: Timed<Event<T>>,
        context: Context<'_, S>,
    ) -> Result<Self::Response, Self::Error> {
        self.reactor.react(event, context).map_err(|error| (self.f)(error))
    }
}

#[derive(Clone, Copy, Debug)]
pub struct MapDataRecord<R, F> {
    reactor: R,
    f: F,
}

impl<U, S, R, O, F> Reactor<U, S> for MapDataRecord<R, F>
where
    R: Reactor<O, S>,
    F: FnMut(U, Context<'_, S>) -> O,
{
    type Response = R::Response;
    type Error = R::Error;

    fn react(
        &mut self,
        event: Timed<Event<U>>,
        mut context: Context<'_, S>,
    ) -> Result<Self::Response, Self::Error> {
        let event = event.map_data_record(|record| (self.f)(record, context.reborrow()));
        self.reactor.react(event, context)
    }
}

#[derive(Clone, Copy, Debug)]
pub struct FilterMapDataRecord<R, F> {
    reactor: R,
    f: F,
}

impl<U, S, R, O, F> Reactor<U, S> for FilterMapDataRecord<R, F>
where
    R: Reactor<O, S>,
    F: FnMut(U, Context<'_, S>) -> Option<O>,
{
    type Response = Option<R::Response>;
    type Error = R::Error;

    fn react(
        &mut self,
        event: Timed<Event<U>>,
        mut context: Context<'_, S>,
    ) -> Result<Self::Response, Self::Error> {
        event
            .filter_map_data_record(|record| (self.f)(record, context.reborrow()))
            .map(|event| self.reactor.react(event, context.reborrow()))
            .transpose()
    }
}

#[derive(Clone, Copy, Debug)]
pub struct WithState<R, S> {
    reactor: R,
    state: S,
}

impl<T, S, U, R> Reactor<T, U> for WithState<R, S>
where
    R: Reactor<T, S>,
{
    type Response = R::Response;
    type Error = R::Error;

    fn react(
        &mut self,
        event: Timed<Event<T>>,
        context: Context<'_, U>,
    ) -> Result<Self::Response, Self::Error> {
        self.reactor.react(event, context.with_state(&mut self.state))
    }
}

#[derive(Clone, Copy, Debug)]
pub struct MapState<R, F> {
    reactor: R,
    f: F,
}

impl<T, S, U, R, F> Reactor<T, S> for MapState<R, F>
where
    R: Reactor<T, U>,
    F: FnMut(&mut S) -> U,
{
    type Response = R::Response;
    type Error = R::Error;

    fn react(
        &mut self,
        event: Timed<Event<T>>,
        context: Context<'_, S>,
    ) -> Result<Self::Response, Self::Error> {
        let mut state = (self.f)(context.state);
        self.reactor.react(event, context.with_state(&mut state))
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Respond<R, P> {
    reactor: R,
    response: P,
}

impl<T, S, P, R> Reactor<T, S> for Respond<R, P>
where
    P: Clone,
    R: Reactor<T, S>,
{
    type Response = P;
    type Error = R::Error;

    fn react(
        &mut self,
        event: Timed<Event<T>>,
        context: Context<'_, S>,
    ) -> Result<Self::Response, Self::Error> {
        let _ = self.reactor.react(event, context);
        Ok(self.response.clone())
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Fail<R, E> {
    reactor: R,
    error: E,
}

impl<T, S, E, R> Reactor<T, S> for Fail<R, E>
where
    E: Clone,
    R: Reactor<T, S>,
{
    type Response = R::Response;
    type Error = E;

    fn react(
        &mut self,
        event: Timed<Event<T>>,
        context: Context<'_, S>,
    ) -> Result<Self::Response, Self::Error> {
        let _ = self.reactor.react(event, context);
        Err(self.error.clone())
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Then<R1, R2> {
    reactor: R1,
    then: R2,
}

impl<T, S, R1, R2> Reactor<T, S> for Then<R1, R2>
where
    T: Clone,
    R1: Reactor<T, S>,
    R2: Reactor<T, S>,
{
    type Response = R2::Response;
    type Error = R2::Error;

    fn react(
        &mut self,
        event: Timed<Event<T>>,
        mut context: Context<'_, S>,
    ) -> Result<Self::Response, Self::Error> {
        let _ = self.reactor.react(event.clone(), context.reborrow());
        self.then.react(event, context)
    }
}

#[derive(Clone, Copy, Debug)]
pub struct And<R1, R2> {
    reactor: R1,
    and: R2,
}

impl<T, S, R1, R2> Reactor<T, S> for And<R1, R2>
where
    T: Clone,
    R1: Reactor<T, S>,
    R1::Error: From<R2::Error>,
    R2: Reactor<T, S>,
{
    type Response = R2::Response;
    type Error = R1::Error;

    fn react(
        &mut self,
        event: Timed<Event<T>>,
        mut context: Context<'_, S>,
    ) -> Result<Self::Response, Self::Error> {
        self.reactor
            .react(event.clone(), context.reborrow())
            .and_then(|_| self.and.react(event, context.reborrow()).map_err(From::from))
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Or<R1, R2> {
    reactor: R1,
    or: R2,
}

impl<T, S, R1, R2> Reactor<T, S> for Or<R1, R2>
where
    T: Clone,
    R1: Reactor<T, S>,
    R2: Reactor<T, S, Response = R1::Response>,
{
    type Response = R1::Response;
    type Error = R2::Error;

    fn react(
        &mut self,
        event: Timed<Event<T>>,
        mut context: Context<'_, S>,
    ) -> Result<Self::Response, Self::Error> {
        match self.reactor.react(event.clone(), context.reborrow()) {
            Ok(response) => Ok(response),
            Err(_) => self.or.react(event, context.reborrow()),
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Inspect<R, F> {
    reactor: R,
    f: F,
}

impl<T, S, R, F> Reactor<T, S> for Inspect<R, F>
where
    T: Clone,
    R: Reactor<T, S>,
    F: FnMut(&Timed<Event<T>>, &Result<R::Response, R::Error>),
{
    type Response = R::Response;
    type Error = R::Error;

    fn react(
        &mut self,
        event: Timed<Event<T>>,
        mut context: Context<'_, S>,
    ) -> Result<Self::Response, Self::Error> {
        let output = self.reactor.react(event.clone(), context.reborrow());
        (self.f)(&event, &output);
        output
    }
}

/// Container for dynamic collections in chain types.
///
/// This type is used in implementations of [`Reactor`] for chain types like [`ThenChain`] and
/// provides a distinction with [`IntoReactor`] implementations. For example, `ThenChain<Vec<R>>`
/// is **not** a reactor, but can be converted into `ThenChain<Dynamic<Vec<R>>>`, which is a
/// reactor.
///
/// In this context, a dynamic collection is a collection of [`Reactor`]s constructed at runtime
/// with an unknown length, namely [`Vec`]. For such collections, it is not possible to chain
/// binary combinators like [`Then`], so a dynamic implementation is used instead like that for
/// `ThenChain<Dynamic<Vec<R>>>`. Contrast this with static collections, like a tuple of
/// [`Reactor`] types, for which the output reactor type is a chain of binary combinators like
/// `Then<Then<A, B>, C>`.
///
/// [`IntoReactor`]: crate::experimental::event::IntoReactor
/// [`Reactor`]: crate::experimental::event::Reactor
/// [`Then`]: crate::experimental::event::Then
/// [`ThenChain`]: crate::experimental::event::ThenChain
/// [`Vec`]: std::vec::Vec
#[derive(Clone, Copy, Debug)]
#[repr(transparent)]
pub struct Dynamic<R>(R);

/// A type that can convert a collection of [`Reactor`]s into an ordered chain of [`Then`]
/// combinators.
///
/// [`Reactor`]: crate::experimental::event::Reactor
/// [`Then`]: crate::experimental::event::Then
#[derive(Clone, Copy, Debug)]
#[repr(transparent)]
pub struct ThenChain<R>(R);

impl<T, S, R> IntoReactor<T, S> for ThenChain<Vec<R>>
where
    T: Clone,
    R: Reactor<T, S>,
{
    type Reactor = ThenChain<Dynamic<Vec<R>>>;

    fn into_reactor(self) -> Self::Reactor {
        ThenChain(Dynamic(self.0))
    }
}

impl<T, S, R> Reactor<T, S> for ThenChain<Dynamic<Vec<R>>>
where
    T: Clone,
    R: Reactor<T, S>,
{
    type Response = R::Response;
    type Error = R::Error;

    fn react(
        &mut self,
        event: Timed<Event<T>>,
        mut context: Context<'_, S>,
    ) -> Result<Self::Response, Self::Error> {
        self.0
             .0
            .iter_mut()
            .map(|reactor| reactor.react(event.clone(), context.reborrow()))
            .next_back()
            .expect("empty `then` combinator")
    }
}

/// A type that can convert a collection of [`Reactor`]s into an ordered chain of [`And`]
/// combinators.
///
/// [`And`]: crate::experimental::event::And
/// [`Reactor`]: crate::experimental::event::Reactor
#[derive(Clone, Copy, Debug)]
#[repr(transparent)]
pub struct AndChain<R>(R);

impl<T, S, R> IntoReactor<T, S> for AndChain<Vec<R>>
where
    T: Clone,
    R: Reactor<T, S>,
{
    type Reactor = AndChain<Dynamic<Vec<R>>>;

    fn into_reactor(self) -> Self::Reactor {
        AndChain(Dynamic(self.0))
    }
}

impl<T, S, R> Reactor<T, S> for AndChain<Dynamic<Vec<R>>>
where
    T: Clone,
    R: Reactor<T, S>,
{
    type Response = Vec<R::Response>;
    type Error = R::Error;

    fn react(
        &mut self,
        event: Timed<Event<T>>,
        mut context: Context<'_, S>,
    ) -> Result<Self::Response, Self::Error> {
        self.0
             .0
            .iter_mut()
            .map(|reactor| reactor.react(event.clone(), context.reborrow()))
            .collect()
    }
}

/// A type that can convert a collection of [`Reactor`]s into an ordered chain of [`Or`]
/// combinators.
///
/// [`Or`]: crate::experimental::event::Ord
/// [`Reactor`]: crate::experimental::event::Reactor
#[derive(Clone, Copy, Debug)]
#[repr(transparent)]
pub struct OrChain<R>(R);

impl<T, S, R> IntoReactor<T, S> for OrChain<Vec<R>>
where
    T: Clone,
    R: Reactor<T, S>,
{
    type Reactor = OrChain<Dynamic<Vec<R>>>;

    fn into_reactor(self) -> Self::Reactor {
        OrChain(Dynamic(self.0))
    }
}

impl<T, S, R> Reactor<T, S> for OrChain<Dynamic<Vec<R>>>
where
    T: Clone,
    R: Reactor<T, S>,
{
    type Response = R::Response;
    type Error = R::Error;

    fn react(
        &mut self,
        event: Timed<Event<T>>,
        mut context: Context<'_, S>,
    ) -> Result<Self::Response, Self::Error> {
        let mut outputs =
            self.0 .0.iter_mut().map(|reactor| reactor.react(event.clone(), context.reborrow()));
        let error = outputs.by_ref().take_while(Result::is_err).last();
        outputs.next().or(error).expect("empty `or` combinator")
    }
}

/// Constructs a [`Reactor`] that reacts to the [data record][`DataEvent::record`] `T`.
///
/// This function is typically paired with a sequencing combinator like [`then`] or [`and`] to
/// construct a reactor for a particular type of data record.
///
/// [`and`]: crate::experimental::event::and
/// [`DataEvent::record`]: crate::experimental::event::DataEvent::record
/// [`Reactor`]: crate::experimental::event::Reactor
/// [`then`]: crate::experimental::event::then
pub fn on_data_record<T, R>(reactor: R) -> OnDataRecord<R>
where
    R: Reactor<T, ()>,
{
    OnDataRecord { reactor }
}

pub fn respond<T, S, P, R>(response: P, reactor: R) -> Respond<R, P>
where
    P: Clone,
    R: Reactor<T, S>,
{
    reactor.respond(response)
}

pub fn fail<T, S, E, R>(error: E, reactor: R) -> Fail<R, E>
where
    E: Clone,
    R: Reactor<T, S>,
{
    reactor.fail(error)
}

pub fn map_data_record<T, S, O, F, R>(f: F, reactor: R) -> MapDataRecord<R, F>
where
    F: FnMut(T, Context<'_, S>) -> O,
    R: Reactor<O, S>,
{
    MapDataRecord { reactor, f }
}

pub fn filter_map_data_record<T, U, S, O, F, R>(f: F, reactor: R) -> FilterMapDataRecord<R, F>
where
    F: FnMut(T, Context<'_, S>) -> Option<O>,
    R: Reactor<U, S>,
{
    FilterMapDataRecord { reactor, f }
}

pub fn with_state<T, S, R>(state: S, reactor: R) -> WithState<R, S>
where
    R: Reactor<T, S>,
{
    WithState { reactor, state }
}

pub fn map_state<T, S, O, F, R>(f: F, reactor: R) -> MapState<R, F>
where
    F: FnMut(&mut S) -> O,
    R: Reactor<T, O>,
{
    MapState { reactor, f }
}

/// Reacts with the given reactors in order (regardless of outputs).
///
/// This function accepts a type `R` for which `ThenChain<R>` implements [`IntoReactor`].
/// [`ThenChain`] implements this trait for collection types, most notably non-unary tuple types
/// and [`Vec`] of [`Reactor`] types.
///
/// The constructed reactor returns the output of the last reactor. See also the [`Reactor::then`]
/// function.
///
/// [`IntoReactor`]: crate::experimental::event::IntoReactor
/// [`Reactor`]: crate::experimental::event::Reactor
/// [`Reactor::then`]: crate::experimental::event::Reactor::then
/// [`ThenChain`]: crate::experimental::event::ThenChain
pub fn then<T, S, R>(reactors: R) -> <ThenChain<R> as IntoReactor<T, S>>::Reactor
where
    ThenChain<R>: IntoReactor<T, S>,
    T: Clone,
{
    ThenChain(reactors).into_reactor()
}

/// Reacts with the given reactors in order until the first error.
///
/// This function accepts a type `R` for which `AndChain<R>` implements [`IntoReactor`].
/// [`AndChain`] implements this trait for collection types, most notably non-unary tuple types and
/// [`Vec`] of [`Reactor`] types.
///
/// The constructed reactor returns either the responses from the given reactors or the first
/// encountered error. See also the [`Reactor::and`] function.
///
/// [`AndChain`]: crate::experimental::event::AndChain
/// [`IntoReactor`]: crate::experimental::event::IntoReactor
/// [`Reactor`]: crate::experimental::event::Reactor
/// [`Reactor::and`]: crate::experimental::event::Reactor::and
pub fn and<T, S, R>(reactors: R) -> <AndChain<R> as IntoReactor<T, S>>::Reactor
where
    AndChain<R>: IntoReactor<T, S>,
    T: Clone,
{
    AndChain(reactors).into_reactor()
}

/// Reacts with the given reactors in order until the first response.
///
/// This function accepts a type `R` for which `OrChain<R>` implements [`IntoReactor`]. [`OrChain`]
/// implements this trait for collection types, most notably non-unary tuple types and [`Vec`] of
/// [`Reactor`] types.
///
/// The constructed reactor returns either the first response from the given reactors or the error
/// from the last reactor. See also the [`Reactor::or`] function.
///
/// [`IntoReactor`]: crate::experimental::event::IntoReactor
/// [`OrChain`]: crate::experimental::event::OrChain
/// [`Reactor`]: crate::experimental::event::Reactor
/// [`Reactor::or`]: crate::experimental::event::Reactor::or
pub fn or<T, S, R>(reactors: R) -> <OrChain<R> as IntoReactor<T, S>>::Reactor
where
    OrChain<R>: IntoReactor<T, S>,
    T: Clone,
{
    OrChain(reactors).into_reactor()
}

/// Invokes another macro with non-unary tuple subsequences.
///
/// This macro invokes another macro with the non-unary subsequences of a single tuple parameter
/// (down to a binary tuple). That is, given a macro `f` and the starting tuple `(T1, T2, T3)`,
/// this macro invokes `f!((T1, T2, T3))` and `f!((T2, T3))`. Note that in this example `f!((T3,))`
/// is **not** invoked, as `(T3,)` is a unary tuple.
macro_rules! with_nonunary_tuples {
    ($f:ident, ( $head:ident, $tail:ident $(,)? ) $(,)?) => {
        $f!(($head, $tail));
    };
    ($f:ident, ( $head:ident, $body:ident, $($tail:ident), +$(,)? ) $(,)?) => {
        $f!(($head,$body,$($tail,)*));
        with_nonunary_tuples!($f, ( $body,$($tail,)+ ));
    };
}

/// Invokes another macro with the non-unary subsequences of supported combinator tuples.
macro_rules! with_reactor_combinator_chain_tuples {
    ($f:ident) => {
        // This defines the set of tuples supported by chaining combinators.
        with_nonunary_tuples!($f, (T1, T2, T3, T4, T5, T6, T7, T8));
    };
}

/// Constructs the chained type name of a sequencing combinator for a **reversed** tuple of
/// `Reactor` types.
///
/// See `forward_combinator_chain_tuple_output_type`.
macro_rules! reverse_combinator_chain_tuple_output_type {
    ($combinator:ident, ( $head:ident,$body:ident,$($tail:ident,)+ )$(,)?) => {
        $combinator<reverse_combinator_chain_tuple_output_type!($combinator, ($body,$($tail,)+)), $head>
    };
    ($combinator:ident, ( $head:ident,$tail:ident$(,)? )$(,)?) => {
        $combinator<$tail, $head>
    };
}

/// Constructs the chained type name of a sequencing combinator for a tuple of `Reactor` types.
///
/// Given the sequencing combinator `Then` and a tuple of `Reactor` types `(C, B, A)`, this macro
/// constructs the identifier `Then<Then<A, B>, C>`. This identifier names the `Reactor` type of
/// the chained expression `A.then(B).then(C)`.
macro_rules! forward_combinator_chain_tuple_output_type {
    ($combinator:ident, ( $($forward:ident),*$(,)? )) => {
        forward_combinator_chain_tuple_output_type!($combinator, ($($forward,)*); ())
    };
    ($combinator:ident, ( $head:ident,$($tail:ident),*$(,)? ); ( $($reverse:ident),*$(,)? )) => {
        forward_combinator_chain_tuple_output_type!($combinator, ($($tail,)*); ($head,$($reverse,)*))
    };
    // This matcher is the base case: the tuple is reversed and so is forwarded to
    // `reverse_combinator_chain_tuple_output_type`.
    ($combinator:ident, ( $(,)? ); ( $($reverse:ident),*$(,)? )) => {
        reverse_combinator_chain_tuple_output_type!($combinator, ( $($reverse,)* ))
    };
}

/// Implements [`IntoReactor`] for [`ThenChain`] of tuple types.
///
/// [`IntoReactor`]: crate::experimental::event::IntoReactor
/// [`ThenChain`]: crate::experimental::event::ThenChain
macro_rules! impl_into_reactor_for_then_chain_tuple {
    (( $head:ident, $($tail:ident $(,)?)+ )) => {
        impl<T, S, $head $(,$tail)+> IntoReactor<T, S> for ThenChain<($head $(,$tail)+)>
        where
            T: Clone,
            $head: Reactor<T, S>,
            $(
                $tail: Reactor<T, S, Response = $head::Response>,
                $head::Error: From<$tail::Error>,
            )+
        {
            type Reactor = forward_combinator_chain_tuple_output_type!(Then, ($head $(,$tail)+));

            #[allow(non_snake_case)]
            #[allow(unused_assignments)]
            fn into_reactor(self) -> Self::Reactor {
                let ($head $(, $tail)+) = self.0;
                $head
                $(
                    .then($tail)
                )+
            }
        }
    }
}
with_reactor_combinator_chain_tuples!(impl_into_reactor_for_then_chain_tuple);

/// Implements [`IntoReactor`] for [`AndChain`] of tuple types.
///
/// [`AndChain`]: crate::experimental::event::AndChain
/// [`IntoReactor`]: crate::experimental::event::IntoReactor
macro_rules! impl_into_reactor_for_and_chain_tuple {
    (( $head:ident, $($tail:ident $(,)?)+ )) => {
        impl<T, S, $head $(,$tail)+> IntoReactor<T, S> for AndChain<($head $(,$tail)+)>
        where
            T: Clone,
            $head: Reactor<T, S>,
            $(
                $tail: Reactor<T, S>,
                $head::Error: From<$tail::Error>,
            )+
        {
            type Reactor = forward_combinator_chain_tuple_output_type!(And, ($head $(,$tail)+));

            #[allow(non_snake_case)]
            #[allow(unused_assignments)]
            fn into_reactor(self) -> Self::Reactor {
                let ($head $(, $tail)+) = self.0;
                $head
                $(
                    .and($tail)
                )+
            }
        }
    }
}
with_reactor_combinator_chain_tuples!(impl_into_reactor_for_and_chain_tuple);

/// Implements [`IntoReactor`] for [`OrChain`] of tuple types.
///
/// [`IntoReactor`]: crate::experimental::event::IntoReactor
/// [`OrChain`]: crate::experimental::event::OrChain
macro_rules! impl_into_reactor_for_or_chain_tuple {
    (( $head:ident, $($tail:ident $(,)?)+ )) => {
        impl<T, S, $head $(,$tail)+> IntoReactor<T, S> for OrChain<($head $(,$tail)+)>
        where
            T: Clone,
            $head: Reactor<T, S>,
            $(
                $tail: Reactor<T, S, Response = $head::Response>,
                $head::Error: From<$tail::Error>,
            )+
        {
            type Reactor = forward_combinator_chain_tuple_output_type!(Or, ($head $(,$tail)+));

            #[allow(non_snake_case)]
            #[allow(unused_assignments)]
            fn into_reactor(self) -> Self::Reactor {
                let ($head $(, $tail)+) = self.0;
                $head
                $(
                    .or($tail)
                )+
            }
        }
    }
}
with_reactor_combinator_chain_tuples!(impl_into_reactor_for_or_chain_tuple);
