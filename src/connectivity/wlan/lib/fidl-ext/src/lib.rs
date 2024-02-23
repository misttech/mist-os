// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#[doc(hidden)]
pub use paste::paste as __paste;

use {anyhow::format_err, itertools::Itertools, std::fmt::Display};

pub trait SendResultExt<T>: Sized {
    fn format_send_err(self) -> Result<T, anyhow::Error>;

    fn format_send_err_with_context<C>(self, context: C) -> Result<T, anyhow::Error>
    where
        C: Display;
}

impl<T> SendResultExt<T> for Result<T, fidl::Error> {
    fn format_send_err(self) -> Result<T, anyhow::Error> {
        self.map_err(|error| format_err!("Failed to respond: {}", error))
    }

    fn format_send_err_with_context<C>(self, context: C) -> Result<T, anyhow::Error>
    where
        C: Display,
    {
        self.map_err(|error| format_err!("Failed to respond ({}): {}", context, error))
    }
}

#[derive(Clone, Copy, Debug)]
pub struct NamedField<T, A> {
    pub value: T,
    pub name: A,
}

pub trait WithName<T>: Sized {
    fn with_name<A>(self, name: A) -> NamedField<T, A>;
}

impl<T> WithName<Option<T>> for Option<T> {
    fn with_name<A>(self, name: A) -> NamedField<Option<T>, A> {
        NamedField { value: self, name }
    }
}

/// A trait for attempting to unpack the values of one type into another.
///
/// # Examples
///
/// Basic usage:
///
/// ```
/// impl<T, U> TryUnpack for (Option<T>, Option<U>) {
///     type Unpacked = (T, U);
///     type Error = ();
///
///     fn try_unpack(self) -> Result<Self::Unpacked, Self::Error> {
///         match self {
///             (Some(t), Some(u)) => Ok((t, u)),
///             _ => Err(()),
///         }
///     }
/// }
///
/// assert_eq!(Ok((1i8, 2u8)), (Some(1i8), Some(2u8)).try_unpack());
/// assert_eq!(Err(()), (Some(1i8), None).try_unpack());
/// ```
pub trait TryUnpack {
    type Unpacked;
    type Error;

    /// Tries to unpack value(s) from `self` and returns a `Self::Error` if the operation fails.
    fn try_unpack(self) -> Result<Self::Unpacked, Self::Error>;
}

#[doc(hidden)]
#[macro_export]
macro_rules! __one {
    ($_t:tt) => {
        1
    };
}

macro_rules! impl_try_unpack_named_fields {
    (($t:ident, $a:ident)) => {
        impl<$t, $a: Display> TryUnpack
            for NamedField<Option<$t>, $a>
        {
            type Unpacked = $t;
            type Error = [String; 1];

            /// Tries to unpack the value a `NamedField<Option<T>, A: Display>`.
            ///
            /// If the `Option<T>` contains a value, then value is unwrapped from the `Option<T>`
            /// and returned, discarding the name.  Otherwise, the returned value is a `Self::Error`
            /// is an array containing the name. An array is returned so that `Self::Error` is an
            /// iterable like in the `TryUnpack` implementation for tuples.
            fn try_unpack(self) -> Result<Self::Unpacked, Self::Error> {
                let NamedField { value, name } = self;
                value.ok_or_else(|| [name.to_string()])
            }
        }
    };

    ($(($t:ident, $a:ident)),+) => {
        $crate::__paste!{
            impl<$($t, $a: Display),+> TryUnpack
                for ($(NamedField<Option<$t>, $a>),+)
            {
                type Unpacked = ($($t),+);
                type Error = Vec<String>;

                /// Tries to unpack values from a tuple of *n* fields each with type
                /// `NamedField<Option<Tᵢ>, Aᵢ: Display>`.
                ///
                /// If every `Option<Tᵢ>` contains a value, then the returned value has the type
                /// `(T₁, T₂, …, Tₙ)`, i.e., every value is unwrapped, and every name is
                /// discarded. Otherwise, the returned value is a `Self::Error` containing name of
                /// each field missing a value.
                fn try_unpack(self) -> Result<Self::Unpacked, Self::Error> {
                    // Fill `names` with the name of each missing field. The variable `arity`
                    // is the arity of the tuple this trait is implemented for.
                    let mut names = None;
                    let names = &mut names;
                    let arity =  0usize $(+ $crate::__one!($t))+;

                    // Deconstruct the tuple `self` into variables `t1`, `t2`, ...
                    let ($([<$t:lower>]),+) = self;

                    // Rebind each `tn` to an `Option<Tn>` that contains a value if `tn` contains
                    // a success value. If `tn` contains an error value, then append the error
                    // value (name) to `names`.
                    let ($([<$t:lower>]),+) = (
                        $({
                            let NamedField { value, name } = [<$t:lower>];
                            value.or_else(|| {
                                names
                                    .get_or_insert_with(|| Vec::with_capacity(arity))
                                    .push(name.to_string());
                                None
                            })
                        }),+
                    );

                    // If `names` contains a value, then one of the `tn` variables must have
                    // contained an error value. In that case, this function returns the error value
                    // `names`. Otherwise, each `tn` variable is returned without its name.
                    match names.take() {
                        Some(names) => Err(names),
                        _ => Ok(($([<$t:lower>].unwrap()),+)),
                    }
                }
            }
        }
    };
}

impl_try_unpack_named_fields!((T1, A1));
impl_try_unpack_named_fields!((T1, A1), (T2, A2));
impl_try_unpack_named_fields!((T1, A1), (T2, A2), (T3, A3));
impl_try_unpack_named_fields!((T1, A1), (T2, A2), (T3, A3), (T4, A4));
impl_try_unpack_named_fields!((T1, A1), (T2, A2), (T3, A3), (T4, A4), (T5, A5));

/// Defines an abstract ResponderExt trait usually implemented using the impl_responder_ext!() macro.
pub trait ResponderExt {
    type Response;
    const REQUEST_NAME: &'static str;

    fn send(self, response: Self::Response) -> Result<(), fidl::Error>;

    /// Returns an success value containing all unpacked fields and the responder, or an error value
    /// if any of the values in `fields` is missing.
    ///
    /// The last argument is a closure which will compute a `Self::Response`.  If any field is
    /// missing a value, then this function will return an error and send the computed
    /// `Self::Response`.
    ///
    /// Example Usage:
    ///
    /// ```
    ///   enum Error {
    ///       UnableToStart,
    ///   }
    ///   let ((status, id), responder) = responder.unpack_fields_or_else_send(
    ///       (payload.status.with_name("status"), payload.id.with_name("id")),
    ///       |e| (e.context(format_err!("Unable to start.")), Error::UnableToStart),
    ///   )?;
    /// ```
    fn unpack_fields_or_else_send<T, F>(
        self,
        fields: T,
        f: F,
    ) -> Result<(T::Unpacked, Self), anyhow::Error>
    where
        T: TryUnpack,
        T::Error: IntoIterator<Item = String>,
        F: FnOnce() -> Self::Response,
        Self: Sized,
    {
        match fields.try_unpack() {
            Ok(values) => Ok((values, self)),
            Err(missing_field_names) => {
                let error = format_err!(
                    "Missing required field(s) in response to {}: {}",
                    Self::REQUEST_NAME,
                    missing_field_names.into_iter().join(", "),
                );
                match self.send(f()).format_send_err() {
                    Ok(_) => Err(error),
                    Err(send_error) => Err(send_error.context(error)),
                }
            }
        }
    }

    fn unpack_fields_or_respond<T>(self, fields: T) -> Result<(T::Unpacked, Self), anyhow::Error>
    where
        T: TryUnpack,
        T::Error: IntoIterator<Item = String>,
        Self: ResponderExt<Response = ()> + Sized,
    {
        self.unpack_fields_or_else_send(fields, || ())
    }
}

#[cfg(test)]
mod tests {
    use {super::*, fidl, fuchsia_zircon as zx, test_case::test_case};

    // The following type definitions emulate the definition of FIDL requests and responder types.
    // In addition to testing the types defined in this crate, these tests demonstrate how those
    // types would be used in the context of a FIDL request.
    //
    // As such, the `Request` type is superfluous but provides a meaningful example for the reader.
    enum Request {
        Ax {
            payload: RequestAx,
            responder: RequestAxResponder,
        },
        Bx {
            payload: RequestBx,
            responder: RequestBxResponder,
        },
        Cx {
            payload: RequestCx,
            responder: RequestCxResponder,
        },
        Dx {
            payload: RequestDx,
            responder: RequestDxResponder,
        },
        Ex {
            #[allow(dead_code)]
            payload: RequestEx,
            responder: RequestExResponder,
        },
        Fx {
            #[allow(dead_code)]
            payload: RequestFx,
            responder: RequestFxResponder,
        },
    }

    #[derive(Debug)]
    struct RequestAx {
        x: Option<()>,
    }

    #[derive(Debug)]
    struct RequestAxResponder {}
    impl RequestAxResponder {
        fn send(self) -> Result<(), fidl::Error> {
            Ok(())
        }
    }

    #[derive(Debug)]
    struct RequestBx {
        x: Option<String>,
        y: Option<u64>,
    }

    #[derive(Debug)]
    struct RequestBxResponder {}
    impl RequestBxResponder {
        fn send(self) -> Result<(), fidl::Error> {
            Ok(())
        }
    }

    #[derive(Debug)]
    struct RequestCx {
        x: Option<i32>,
    }

    #[derive(Debug)]
    struct RequestCxResponder {}
    impl RequestCxResponder {
        fn send(self, _result: Result<u64, u64>) -> Result<(), fidl::Error> {
            Ok(())
        }
    }

    #[derive(Debug)]
    struct RequestDx {
        x: Option<Option<()>>,
        y: Option<bool>,
    }

    #[derive(Debug)]
    struct RequestDxResponder {}
    impl RequestDxResponder {
        fn send(self, _result: Result<(), zx::Status>) -> Result<(), fidl::Error> {
            Ok(())
        }
    }

    #[derive(Debug)]
    struct RequestEx {
        #[allow(dead_code)]
        x: Option<()>,
    }

    #[derive(Debug)]
    struct RequestExResponder {}
    impl RequestExResponder {
        fn send(self) -> Result<(), fidl::Error> {
            Err(fidl::Error::InvalidHeader)
        }
    }

    #[derive(Debug)]
    struct RequestFx {
        #[allow(dead_code)]
        x: Option<()>,
    }

    #[derive(Debug)]
    struct RequestFxResponder {}
    impl RequestFxResponder {
        fn send(self, _result: Result<(), zx::Status>) -> Result<(), fidl::Error> {
            Err(fidl::Error::InvalidHeader)
        }
    }

    impl ResponderExt for RequestAxResponder {
        type Response = ();
        const REQUEST_NAME: &'static str = stringify!(RequestAx);

        fn send(self, _: Self::Response) -> Result<(), fidl::Error> {
            Self::send(self)
        }
    }

    impl ResponderExt for RequestBxResponder {
        type Response = ();
        const REQUEST_NAME: &'static str = stringify!(RequestBx);

        fn send(self, _: Self::Response) -> Result<(), fidl::Error> {
            Self::send(self)
        }
    }

    impl ResponderExt for RequestCxResponder {
        type Response = Result<u64, u64>;
        const REQUEST_NAME: &'static str = stringify!(RequestCx);

        fn send(self, response: Self::Response) -> Result<(), fidl::Error> {
            Self::send(self, response)
        }
    }

    impl ResponderExt for RequestDxResponder {
        type Response = Result<(), zx::Status>;
        const REQUEST_NAME: &'static str = stringify!(RequestDx);

        fn send(self, response: Self::Response) -> Result<(), fidl::Error> {
            Self::send(self, response)
        }
    }

    impl ResponderExt for RequestExResponder {
        type Response = ();
        const REQUEST_NAME: &'static str = stringify!(RequestEx);

        fn send(self, _: Self::Response) -> Result<(), fidl::Error> {
            Self::send(self)
        }
    }

    impl ResponderExt for RequestFxResponder {
        type Response = Result<(), zx::Status>;
        const REQUEST_NAME: &'static str = stringify!(RequestFx);

        fn send(self, response: Self::Response) -> Result<(), fidl::Error> {
            Self::send(self, response)
        }
    }

    #[test]
    fn responder_send_error_without_context_compiles() {
        if let Request::Ax { responder, .. } =
            (Request::Ax { payload: RequestAx { x: Some(()) }, responder: RequestAxResponder {} })
        {
            let _: Result<(), anyhow::Error> = ResponderExt::send(responder, ()).format_send_err();
        } else {
            panic!("Failed to match request");
        }
    }

    #[test]
    fn responder_send_error_with_context_compiles() {
        if let Request::Cx { responder, .. } =
            (Request::Cx { payload: RequestCx { x: Some(5) }, responder: RequestCxResponder {} })
        {
            let _: Result<(), anyhow::Error> =
                ResponderExt::send(responder, Ok(10)).format_send_err_with_context("");
        }
    }

    #[test]
    fn responder_send_error_without_type_fails_to_send() {
        if let Request::Ex { responder, .. } =
            (Request::Ex { payload: RequestEx { x: Some(()) }, responder: RequestExResponder {} })
        {
            let e = ResponderExt::send(responder, ())
                .format_send_err()
                .expect_err("Unexpected success upon send().");
            assert!(e.to_string().contains("Failed to respond: "));
        }
    }

    #[test]
    fn responder_send_error_with_context_fails_to_send() {
        if let Request::Fx { responder, .. } =
            (Request::Fx { payload: RequestFx { x: Some(()) }, responder: RequestFxResponder {} })
        {
            let e = ResponderExt::send(responder, Err(zx::Status::INVALID_ARGS))
                .format_send_err_with_context("foo")
                .expect_err("Unexpected success upon send().");
            assert!(e.to_string().contains("Failed to respond (foo): "));
        }
    }

    macro_rules! assert_debug_fmt_contains {
            ($s:expr, $($substr:expr),+ $(,)?) => {
                let s = format!("{:?}", $s);
                $(
                    assert!(s.contains($substr), "String is \"{}\"", s);
                )+
            };
        }

    #[test_case(Some(()))]
    #[test_case(None)]
    fn unpack_single_annotated_field(x: Option<()>) {
        if let Request::Ax { payload, responder } =
            (Request::Ax { payload: RequestAx { x }, responder: RequestAxResponder {} })
        {
            let result = responder.unpack_fields_or_respond(payload.x.with_name("x"));
            if x.is_some() {
                let (_x, _responder) = result.expect("Failed to unpack x");
            } else {
                assert_debug_fmt_contains!(
                    result.expect_err("Took x when it wasn't present"),
                    "Missing required field",
                    "RequestAx",
                    ": x",
                );
            }
        } else {
            panic!("Failed to match request");
        }
    }

    #[test_case(Some(format!("")), Some(754))]
    #[test_case(Some(format!("")), None)]
    #[test_case(None, Some(754))]
    #[test_case(None, None)]
    fn unpack_multiple_annotated_fields(x: Option<String>, y: Option<u64>) {
        if let Request::Bx { payload, responder } = (Request::Bx {
            payload: RequestBx { x: x.clone(), y },
            responder: RequestBxResponder {},
        }) {
            let result = responder
                .unpack_fields_or_respond((payload.x.with_name("x"), payload.y.with_name("y")));
            match (x, y) {
                (None, None) => {
                    assert_debug_fmt_contains!(
                        result.expect_err("Took x when it wasn't present"),
                        "Missing required field",
                        "RequestBx",
                        ": x, y",
                    );
                }
                (None, Some(_)) => {
                    assert_debug_fmt_contains!(
                        result.expect_err("Took x when it wasn't present"),
                        "Missing required field",
                        "RequestBx",
                        ": x",
                    );
                }
                (Some(_), None) => {
                    assert_debug_fmt_contains!(
                        result.expect_err("Took y when it wasn't present"),
                        "Missing required field",
                        "RequestBx",
                        ": y",
                    );
                }
                (Some(_), Some(_)) => {
                    result.expect("Failed to unpack x and y");
                }
            }
        } else {
            panic!("Failed to match request");
        }
    }

    #[test_case(Some(2))]
    #[test_case(None)]
    fn unpack_single_annotated_field_with_error(x: Option<i32>) {
        if let Request::Cx { payload, responder } =
            (Request::Cx { payload: RequestCx { x }, responder: RequestCxResponder {} })
        {
            let mut closure_ran = false;
            let closure_ran_ref = &mut closure_ran;
            let result = responder.unpack_fields_or_else_send(payload.x.with_name("x"), || {
                *closure_ran_ref = true;
                Err(12)
            });
            if x.is_some() {
                let (x_ret, _responder) = result.expect("Failed to unpack x");
                assert!(!closure_ran);
                assert_eq!(x_ret, x.unwrap());
            } else {
                assert!(closure_ran);
                assert_debug_fmt_contains!(
                    result.expect_err("Took x when it wasn't present"),
                    "Missing required field",
                    "RequestCx",
                    ": x",
                );
            }
        }
    }

    #[test_case(Some(None), Some(true))]
    #[test_case(Some(None), None)]
    #[test_case(None, Some(true))]
    #[test_case(None, None)]
    fn unpack_multiple_annotated_fields_with_error(x: Option<Option<()>>, y: Option<bool>) {
        if let Request::Dx { payload, responder } = (Request::Dx {
            payload: RequestDx { x: x.clone(), y },
            responder: RequestDxResponder {},
        }) {
            let result = responder.unpack_fields_or_else_send(
                (payload.x.with_name("x"), payload.y.with_name("y")),
                || Err(zx::Status::INVALID_ARGS),
            );
            match (x, y) {
                (None, None) => {
                    assert_debug_fmt_contains!(
                        result.expect_err("Took x and y when they weren't present"),
                        "Missing required field",
                        "RequestDx",
                        ": x, y",
                    );
                }
                (None, Some(_)) => {
                    assert_debug_fmt_contains!(
                        result.expect_err("Took x when it wasn't present"),
                        "Missing required field",
                        "RequestDx",
                        ": x",
                    );
                }
                (Some(_), None) => {
                    assert_debug_fmt_contains!(
                        result.expect_err("Took y when it wasn't present"),
                        "Missing required field",
                        "RequestDx",
                        ": y",
                    );
                }
                (Some(_), Some(_)) => {
                    let ((_x, y_ret), _responder) = result.expect("Failed to unpack x and y");
                    assert_eq!(y_ret, y.unwrap());
                }
            }
        }
    }

    #[test_case(Some(None), Some(true))]
    #[test_case(Some(None), None)]
    #[test_case(None, Some(true))]
    #[test_case(None, None)]
    fn unpack_multiple_annotated_fields_over_separate_calls(
        x: Option<Option<()>>,
        y: Option<bool>,
    ) {
        if let Request::Dx { payload, responder } = (Request::Dx {
            payload: RequestDx { x: x.clone(), y },
            responder: RequestDxResponder {},
        }) {
            let mut closure_ran = false;
            let closure_ran_ref = &mut closure_ran;
            let result = responder.unpack_fields_or_else_send(payload.x.with_name("x"), || {
                *closure_ran_ref = true;
                Err(zx::Status::INVALID_ARGS)
            });
            if x.is_none() {
                assert_debug_fmt_contains!(
                    result.expect_err("Took x when it wasn't present"),
                    "Missing required field",
                    "RequestDx",
                    ": x",
                );
                assert!(closure_ran);
                return;
            }
            let (_x, responder): (Option<()>, RequestDxResponder) =
                result.expect("Failed to unpack x");
            assert!(!closure_ran);

            let closure_ran_ref = &mut closure_ran;
            let result = responder.unpack_fields_or_else_send(payload.y.with_name("y"), || {
                *closure_ran_ref = true;
                Err(zx::Status::INVALID_ARGS)
            });
            if y.is_none() {
                assert_debug_fmt_contains!(
                    result.expect_err("Took y when it wasn't present"),
                    "Missing required field",
                    "RequestDx",
                    ": y",
                );
                assert!(closure_ran);
                return;
            }
            let (y_ret, _responder) = result.expect("Failed to unpack y");
            assert!(!closure_ran);
            assert_eq!(y_ret, y.unwrap());
        }
    }
}
