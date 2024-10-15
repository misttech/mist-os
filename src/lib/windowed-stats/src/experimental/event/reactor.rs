// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::experimental::clock::Timed;
use crate::experimental::event::{DataEvent, Event};

// TODO(https://fxbug.dev/372958478): Implement combinators with explicit types instead of
//                                    RPIT(IT). While terse and expressive, RPIT outputs are
//                                    entirely opaque and notably omit important bounds on
//                                    auto-traits like `Send` and `Sync`.

/// A type that reacts to [timed][`Timed`] [`Event`]s.
///
/// A reactor is a function that responds to [system][`SystemEvent`] and [data][`DataEvent`]
/// events. Reactors are formed from combinators, which compose behaviors.
///
/// [`DataEvent`]: crate::experimental::event::DataEvent
/// [`Event`]: crate::experimental::event::Event
/// [`SystemEvent`]: crate::experimental::event::SystemEvent
/// [`Timed`]: crate::experimental::clock::Timed
pub trait Reactor<T> {
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
    fn react(&mut self, event: Timed<Event<T>>) -> Result<Self::Response, Self::Error>;

    /// Reacts to a [data record][`DataEvent::record`].
    ///
    /// This function constructs and [reacts to][`Reactor::react`] a data event with the given
    /// record at [`Timestamp::now`].
    ///
    /// [`DataEvent::record`]: crate::experimental::event::DataEvent::record
    /// [`Reactor::react`]: crate::experimental::event::Reactor::react
    /// [`Timestamp::now`]: crate::experimental::clock::Timed
    fn react_to_data_record(&mut self, record: T) -> Result<Self::Response, Self::Error> {
        self.react(Timed::now(DataEvent { record }.into()))
    }

    fn map_response<O, F>(mut self, mut f: F) -> impl Reactor<T, Response = O, Error = Self::Error>
    where
        Self: Sized,
        F: FnMut(Self::Response) -> O,
    {
        move |event| self.react(event).map(|response| f(response))
    }

    fn map_error<E, F>(mut self, mut f: F) -> impl Reactor<T, Response = Self::Response, Error = E>
    where
        Self: Sized,
        F: FnMut(Self::Error) -> E,
    {
        move |event| self.react(event).map_err(|error| f(error))
    }

    /// Reacts with this reactor and then the given reactor (regardless of outputs).
    ///
    /// The constructed reactor returns the output of the given (subsequent) reactor. See also the
    /// [`event::then`] function.
    ///
    /// [`event::then`]: crate::experimental::event::then
    fn then<R>(
        mut self,
        mut reactor: R,
    ) -> impl Reactor<T, Response = R::Response, Error = R::Error>
    where
        Self: Sized,
        T: Clone,
        R: Reactor<T>,
    {
        move |event: Timed<Event<T>>| {
            let _ = self.react(event.clone());
            reactor.react(event)
        }
    }

    /// Reacts with this reactor and then the given reactor if and only if this reactor returns
    /// `Ok`.
    ///
    /// The constructed reactor returns either an error from this reactor or the output of the
    /// given (subsequent) reactor. See also the [`event::and`] function.
    ///
    /// [`event::and`]: crate::experimental::event::and
    fn and<R>(
        mut self,
        mut reactor: R,
    ) -> impl Reactor<T, Response = R::Response, Error = Self::Error>
    where
        Self: Sized,
        Self::Error: From<R::Error>,
        T: Clone,
        R: Reactor<T>,
    {
        move |event: Timed<Event<T>>| {
            self.react(event.clone()).and_then(|_| reactor.react(event).map_err(From::from))
        }
    }

    /// Reacts with this reactor and then the given reactor if and only if this reactor returns
    /// `Err`.
    ///
    /// The constructed reactor returns either a response from this reactor or the output of the
    /// given (subsequent) reactor. See also the [`event::or`] function.
    ///
    /// [`event::or`]: crate::experimental::event::or
    fn or<R>(
        mut self,
        mut reactor: R,
    ) -> impl Reactor<T, Response = Self::Response, Error = Self::Error>
    where
        Self: Sized,
        Self::Error: From<R::Error>,
        T: Clone,
        R: Reactor<T, Response = Self::Response>,
    {
        move |event: Timed<Event<T>>| match self.react(event.clone()) {
            Err(_) => reactor.react(event).map_err(From::from),
            output => output,
        }
    }

    /// Constructs a `Reactor` that inspects the event and output of `self` with the given
    /// function.
    fn inspect<F>(
        mut self,
        mut f: F,
    ) -> impl Reactor<T, Response = Self::Response, Error = Self::Error>
    where
        Self: Sized,
        T: Clone,
        F: FnMut(&Timed<Event<T>>, &Result<Self::Response, Self::Error>),
    {
        move |event: Timed<Event<T>>| {
            let output = self.react(event.clone());
            f(&event, &output);
            output
        }
    }
}

impl<T, R, E, F> Reactor<T> for F
where
    F: FnMut(Timed<Event<T>>) -> Result<R, E>,
{
    type Response = R;
    type Error = E;

    fn react(&mut self, event: Timed<Event<T>>) -> Result<Self::Response, Self::Error> {
        (self)(event)
    }
}

pub trait IntoReactor<T> {
    type Response;
    type Error;

    fn into_reactor(self) -> impl Reactor<T, Response = Self::Response, Error = Self::Error>;
}

#[derive(Clone, Copy, Debug)]
#[repr(transparent)]
pub struct ThenChain<R>(R);

impl<T, R> IntoReactor<T> for ThenChain<Vec<R>>
where
    T: Clone,
    R: Reactor<T>,
{
    type Response = R::Response;
    type Error = R::Error;

    fn into_reactor(mut self) -> impl Reactor<T, Response = Self::Response, Error = Self::Error> {
        move |event: Timed<Event<T>>| {
            self.0
                .iter_mut()
                .map(|reactor| reactor.react(event.clone()))
                .last()
                .expect("empty `then` combinator")
        }
    }
}

#[derive(Clone, Copy, Debug)]
#[repr(transparent)]
pub struct AndChain<R>(R);

impl<T, R> IntoReactor<T> for AndChain<Vec<R>>
where
    T: Clone,
    R: Reactor<T>,
{
    type Response = Vec<R::Response>;
    type Error = R::Error;

    fn into_reactor(mut self) -> impl Reactor<T, Response = Self::Response, Error = Self::Error> {
        move |event: Timed<Event<T>>| {
            self.0.iter_mut().map(|reactor| reactor.react(event.clone())).collect()
        }
    }
}

#[derive(Clone, Copy, Debug)]
#[repr(transparent)]
pub struct OrChain<R>(R);

impl<T, R> IntoReactor<T> for OrChain<Vec<R>>
where
    T: Clone,
    R: Reactor<T>,
{
    type Response = R::Response;
    type Error = R::Error;

    fn into_reactor(mut self) -> impl Reactor<T, Response = Self::Response, Error = Self::Error> {
        move |event: Timed<Event<T>>| {
            let mut outputs = self.0.iter_mut().map(|reactor| reactor.react(event.clone()));
            let error = outputs.by_ref().take_while(Result::is_err).last();
            outputs.next().or(error).expect("empty `or` combinator")
        }
    }
}

/// Constructs a [`Reactor`] that reacts to the [data record][`DataEvent::record`] `T`.
///
/// This function is typically paired with a multiplexing combinator like [`then`] or [`and`] to
/// construct a reactor for a particular type of data record.
///
/// [`and`]: crate::experimental::event::and
/// [`DataEvent::record`]: crate::experimental::event::DataEvent::record
/// [`Reactor`]: crate::experimental::event::Reactor
/// [`then`]: crate::experimental::event::then
pub fn on_data_record<T, R>(
    mut reactor: R,
) -> impl Reactor<T, Response = R::Response, Error = R::Error>
where
    R: Reactor<T>,
{
    move |event| reactor.react(event)
}

/// Constructs a [`Reactor`] that reacts with the given `response` regardless the event.
pub fn ok<T, R, E>(response: R) -> impl Reactor<T, Response = R, Error = E>
where
    R: Clone,
{
    move |_| Ok(response.clone())
}

/// Constructs a [`Reactor`] that reacts with the given `error` regardless of event.
pub fn error<T, R, E>(error: E) -> impl Reactor<T, Response = R, Error = E>
where
    E: Clone,
{
    move |_| Err(error.clone())
}

pub fn map_data_record<T, U, F, R>(
    mut f: F,
    mut reactor: R,
) -> impl Reactor<T, Response = R::Response, Error = R::Error>
where
    F: FnMut(T) -> U,
    R: Reactor<U>,
{
    move |event: Timed<Event<T>>| reactor.react(event.map_data_record(|record| f(record)))
}

/// Reacts with the given reactors in order (regardless of outputs).
///
/// This function accepts a type `R` for which `ThenChain<R>` implements [`IntoReactor`].
/// [`ThenChain`] implements this trait for collection types, most notably non-unary tuples and
/// [`Vec`] of [`Reactor`] types.
///
/// The constructed reactor returns the output of the last reactor. See also the [`Reactor::then`]
/// function.
///
/// [`IntoReactor`]: crate::experimental::event::IntoReactor
/// [`Reactor`]: crate::experimental::event::Reactor
/// [`Reactor::then`]: crate::experimental::event::Reactor::then
/// [`ThenChain`]: crate::experimental::event::ThenChain
pub fn then<T, R>(
    reactors: R,
) -> impl Reactor<
    T,
    Response = <ThenChain<R> as IntoReactor<T>>::Response,
    Error = <ThenChain<R> as IntoReactor<T>>::Error,
>
where
    ThenChain<R>: IntoReactor<T>,
    T: Clone,
{
    let mut reactor = ThenChain(reactors).into_reactor();
    move |event| reactor.react(event)
}

/// Reacts with the given reactors in order until the first error.
///
/// This function accepts a type `R` for which `AndChain<R>` implements [`IntoReactor`].
/// [`AndChain`] implements this trait for collection types, most notably non-unary tuples and
/// [`Vec`] of [`Reactor`] types.
///
/// The constructed reactor returns either the responses from the given reactors or the first
/// encountered error. See also the [`Reactor::and`] function.
///
/// [`AndChain`]: crate::experimental::event::AndChain
/// [`IntoReactor`]: crate::experimental::event::IntoReactor
/// [`Reactor`]: crate::experimental::event::Reactor
/// [`Reactor::and`]: crate::experimental::event::Reactor::and
pub fn and<T, R>(
    reactors: R,
) -> impl Reactor<
    T,
    Response = <AndChain<R> as IntoReactor<T>>::Response,
    Error = <AndChain<R> as IntoReactor<T>>::Error,
>
where
    AndChain<R>: IntoReactor<T>,
    T: Clone,
{
    let mut reactor = AndChain(reactors).into_reactor();
    move |event| reactor.react(event)
}

pub fn or<T, R>(
    reactors: R,
) -> impl Reactor<
    T,
    Response = <OrChain<R> as IntoReactor<T>>::Response,
    Error = <OrChain<R> as IntoReactor<T>>::Error,
>
where
    OrChain<R>: IntoReactor<T>,
    T: Clone,
{
    let mut reactor = OrChain(reactors).into_reactor();
    move |event| reactor.react(event)
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

macro_rules! with_reactor_combinator_tuples {
    ($f:ident) => {
        with_nonunary_tuples!($f, (T1, T2, T3, T4, T5, T6, T7, T8));
    };
}

/// Implements [`IntoReactor`] for [`ThenChain`] of tuple types.
///
/// [`IntoReactor`]: crate::experimental::event::IntoReactor
/// [`ThenChain`]: crate::experimental::event::ThenChain
macro_rules! impl_into_reactor_for_then_chain_tuple {
    (( $head:ident, $($tail:ident $(,)?)+ )) => {
        impl<T, $head $(,$tail)+> IntoReactor<T> for ThenChain<($head $(,$tail)+)>
        where
            $head: Reactor<T>,
            $(
                $tail: Reactor<T, Response = $head::Response>,
                $head::Error: From<$tail::Error>,
            )+
            T: Clone,
        {
            type Response = $head::Response;
            type Error = $head::Error;

            fn into_reactor(
                mut self,
            ) -> impl Reactor<T, Response = Self::Response, Error = Self::Error> {
                #[allow(non_snake_case)]
                #[allow(unused_assignments)]
                move |event: Timed<Event<T>>| {
                    let (ref mut $head $(, ref mut $tail)+) = self.0;
                    let mut output = $head.react(event.clone());
                    $(
                        output = $tail.react(event.clone()).map_err(From::from);
                    )+
                    output
                }
            }
        }
    }
}
with_reactor_combinator_tuples!(impl_into_reactor_for_then_chain_tuple);

/// Implements [`IntoReactor`] for [`AndChain`] of tuple types.
///
/// [`AndChain`]: crate::experimental::event::AndChain
/// [`IntoReactor`]: crate::experimental::event::IntoReactor
macro_rules! impl_into_reactor_for_and_chain_tuple {
    (( $head:ident, $($tail:ident $(,)?)+ )) => {
        impl<T, $head $(,$tail)+> IntoReactor<T> for AndChain<($head $(,$tail)+)>
        where
            $head: Reactor<T>,
            $(
                $tail: Reactor<T>,
                $head::Error: From<$tail::Error>,
            )+
            T: Clone,
        {
            type Response = ($head::Response $(, $tail::Response)+);
            type Error = $head::Error;

            fn into_reactor(
                mut self,
            ) -> impl Reactor<T, Response = Self::Response, Error = Self::Error> {
                #[allow(non_snake_case)]
                move |event: Timed<Event<T>>| {
                    let (ref mut $head $(, ref mut $tail)+) = self.0;
                    let $head = $head.react(event.clone())?;
                    $(
                        let $tail = $tail.react(event.clone())?;
                    )+
                    Ok(($head $(, $tail)+))
                }
            }
        }
    }
}
with_reactor_combinator_tuples!(impl_into_reactor_for_and_chain_tuple);

/// Implements [`IntoReactor`] for [`OrChain`] of tuple types.
///
/// [`IntoReactor`]: crate::experimental::event::IntoReactor
/// [`OrChain`]: crate::experimental::event::OrChain
macro_rules! impl_into_reactor_for_or_chain_tuple {
    (( $head:ident, $($tail:ident $(,)?)+ )) => {
        impl<T, $head $(,$tail)+> IntoReactor<T> for OrChain<($head $(,$tail)+)>
        where
            $head: Reactor<T>,
            $(
                $tail: Reactor<T, Response = $head::Response>,
                $head::Error: From<$tail::Error>,
            )+
            T: Clone,
        {
            type Response = $head::Response;
            type Error = $head::Error;

            fn into_reactor(
                mut self,
            ) -> impl Reactor<T, Response = Self::Response, Error = Self::Error> {
                #[allow(non_snake_case)]
                move |event: Timed<Event<T>>| {
                    let (ref mut $head $(, ref mut $tail)+) = self.0;
                    $head.react(event.clone())$(.or_else(|_| $tail.react(event.clone()).map_err($head::Error::from)))+
                }
            }
        }
    }
}
with_reactor_combinator_tuples!(impl_into_reactor_for_or_chain_tuple);
