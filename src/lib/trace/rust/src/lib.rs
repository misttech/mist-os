// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use pin_project::pin_project;
use std::boxed::Box;
use std::ffi::CStr;
use std::future::Future;
use std::marker::PhantomData;
use std::pin::Pin;
use std::task::Poll;
use std::{mem, ptr};

pub use sys::{
    trace_site_t, trace_string_ref_t, TRACE_BLOB_TYPE_DATA, TRACE_BLOB_TYPE_LAST_BRANCH,
    TRACE_BLOB_TYPE_PERFETTO,
};

/// `Scope` represents the scope of a trace event.
#[derive(Copy, Clone)]
pub enum Scope {
    Thread,
    Process,
    Global,
}

impl Scope {
    fn into_raw(self) -> sys::trace_scope_t {
        match self {
            Scope::Thread => sys::TRACE_SCOPE_THREAD,
            Scope::Process => sys::TRACE_SCOPE_PROCESS,
            Scope::Global => sys::TRACE_SCOPE_GLOBAL,
        }
    }
}

/// Returns true if tracing is enabled.
#[inline]
pub fn is_enabled() -> bool {
    // Trivial no-argument function that will not race
    unsafe { sys::trace_state() != sys::TRACE_STOPPED }
}

/// Returns true if tracing has been enabled for the given category.
pub fn category_enabled(category: &'static CStr) -> bool {
    // Function requires a pointer to a static null-terminated string literal,
    // which `&'static CStr` is.
    unsafe { sys::trace_is_category_enabled(category.as_ptr()) }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TraceState {
    Stopped,
    Started,
    Stopping,
}

pub fn trace_state() -> TraceState {
    match unsafe { sys::trace_state() } {
        sys::TRACE_STOPPED => TraceState::Stopped,
        sys::TRACE_STARTED => TraceState::Started,
        sys::TRACE_STOPPING => TraceState::Stopping,
        s => panic!("Unknown trace state {:?}", s),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(i32)]
pub enum BufferingMode {
    OneShot = sys::TRACE_BUFFERING_MODE_ONESHOT,
    Circular = sys::TRACE_BUFFERING_MODE_CIRCULAR,
    Streaming = sys::TRACE_BUFFERING_MODE_STREAMING,
}

/// An identifier for flows and async spans.
#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Id(u64);

impl Id {
    /// Creates a new `Id`. `Id`s created by separate calls to `new` in the same process are
    /// guaranteed to be distinct.
    ///
    /// WARNING: `Id::new` is likely to hit the UI bug where UIs group async
    /// durations with the same trace id but different process ids. Use
    /// `Id::random` instead. (Until https://fxbug.dev/42054669 is fixed.)
    pub fn new() -> Self {
        // Trivial no-argument function that cannot race.
        Self(unsafe { sys::trace_generate_nonce() })
    }

    /// Creates a new `Id` based on the current montonic time and a random `u16` to, with high
    /// probability, avoid the bug where UIs group async durations with the same trace id but
    /// different process ids.
    /// `Id::new` is likely to hit the UI bug because it (per process) generates trace ids
    /// consecutively starting from 1.
    /// https://cs.opensource.google/fuchsia/fuchsia/+/main:zircon/system/ulib/trace-engine/nonce.cc;l=15-17;drc=b1c2f508a59e6c87c617852ed3e424693a392646
    /// TODO(https://fxbug.dev/42054669) Delete this and migrate clients to `Id::new` when:
    /// 1. UIs stop grouping async durations with the same trace id but different process ids.
    /// 2. input events tracing cross components has uid for flow id.
    pub fn random() -> Self {
        let ts = zx::BootInstant::get().into_nanos() as u64;
        let high_order = ts << 16;
        let low_order = rand::random::<u16>() as u64;
        Self(high_order | low_order)
    }
}

impl From<u64> for Id {
    fn from(u: u64) -> Self {
        Self(u)
    }
}

impl From<Id> for u64 {
    fn from(id: Id) -> Self {
        id.0
    }
}

/// `Arg` holds an argument to a tracing function, which can be one of many types.
#[repr(transparent)]
pub struct Arg<'a>(sys::trace_arg_t, PhantomData<&'a ()>);

/// A trait for types that can be the values of an argument set.
///
/// This trait is not implementable by users of the library.
/// Users should instead use one of the common types which implements
/// `ArgValue`, such as `i32`, `f64`, or `&str`.
pub trait ArgValue {
    fn of<'a>(key: &'a str, value: Self) -> Arg<'a>
    where
        Self: 'a;
}

// Implements `arg_from` for many types.
// $valname is the name to which to bind the `Self` value in the $value expr
// $ty is the type
// $tag is the union tag indicating the variant of trace_arg_union_t being used
// $value is the union value for that particular type
macro_rules! arg_from {
    ($valname:ident, $(($type:ty, $tag:expr, $value:expr))*) => {
        $(
            impl ArgValue for $type {
                #[inline]
                fn of<'a>(key: &'a str, $valname: Self) -> Arg<'a>
                    where Self: 'a
                {
                    #[allow(unused)]
                    let $valname = $valname;

                    Arg(sys::trace_arg_t {
                        name_ref: trace_make_inline_string_ref(key),
                        value: sys::trace_arg_value_t {
                            type_: $tag,
                            value: $value,
                        },
                    }, PhantomData)
                }
            }
        )*
    }
}

// Implement ArgFrom for a variety of types
#[rustfmt::skip]
arg_from!(val,
    ((), sys::TRACE_ARG_NULL, sys::trace_arg_union_t { int32_value: 0 })
    (bool, sys::TRACE_ARG_BOOL, sys::trace_arg_union_t { bool_value: val })
    (i32, sys::TRACE_ARG_INT32, sys::trace_arg_union_t { int32_value: val })
    (u32, sys::TRACE_ARG_UINT32, sys::trace_arg_union_t { uint32_value: val })
    (i64, sys::TRACE_ARG_INT64, sys::trace_arg_union_t { int64_value: val })
    (u64, sys::TRACE_ARG_UINT64, sys::trace_arg_union_t { uint64_value: val })
    (isize, sys::TRACE_ARG_INT64, sys::trace_arg_union_t { int64_value: val as i64 })
    (usize, sys::TRACE_ARG_UINT64, sys::trace_arg_union_t { uint64_value: val as u64 })
    (f64, sys::TRACE_ARG_DOUBLE, sys::trace_arg_union_t { double_value: val })
    (zx::Koid, sys::TRACE_ARG_KOID, sys::trace_arg_union_t { koid_value: val.raw_koid() })
);

impl<T> ArgValue for *const T {
    #[inline]
    fn of<'a>(key: &'a str, val: Self) -> Arg<'a>
    where
        Self: 'a,
    {
        Arg(
            sys::trace_arg_t {
                name_ref: trace_make_inline_string_ref(key),
                value: sys::trace_arg_value_t {
                    type_: sys::TRACE_ARG_POINTER,
                    value: sys::trace_arg_union_t { pointer_value: val as usize },
                },
            },
            PhantomData,
        )
    }
}

impl<T> ArgValue for *mut T {
    #[inline]
    fn of<'a>(key: &'a str, val: Self) -> Arg<'a>
    where
        Self: 'a,
    {
        Arg(
            sys::trace_arg_t {
                name_ref: trace_make_inline_string_ref(key),
                value: sys::trace_arg_value_t {
                    type_: sys::TRACE_ARG_POINTER,
                    value: sys::trace_arg_union_t { pointer_value: val as usize },
                },
            },
            PhantomData,
        )
    }
}

impl<'a> ArgValue for &'a str {
    #[inline]
    fn of<'b>(key: &'b str, val: Self) -> Arg<'b>
    where
        Self: 'b,
    {
        Arg(
            sys::trace_arg_t {
                name_ref: trace_make_inline_string_ref(key),
                value: sys::trace_arg_value_t {
                    type_: sys::TRACE_ARG_STRING,
                    value: sys::trace_arg_union_t {
                        string_value_ref: trace_make_inline_string_ref(val),
                    },
                },
            },
            PhantomData,
        )
    }
}

/// Convenience macro for the `instant` function.
///
/// Example:
///
/// ```rust
/// instant!(c"foo", c"bar", Scope::Process, "x" => 5, "y" => "boo");
/// ```
///
/// is equivalent to
///
/// ```rust
/// instant(c"foo", c"bar", Scope::Process,
///     &[ArgValue::of("x", 5), ArgValue::of("y", "boo")]);
/// ```
/// or
/// ```rust
/// const FOO: &'static CStr = c"foo";
/// const BAR: &'static CStr = c"bar";
/// instant(FOO, BAR, Scope::Process,
///     &[ArgValue::of("x", 5), ArgValue::of("y", "boo")]);
/// ```
#[macro_export]
macro_rules! instant {
    ($category:expr, $name:expr, $scope:expr $(, $key:expr => $val:expr)*) => {
        {
            static CACHE: $crate::trace_site_t = $crate::trace_site_t::new(0);
            if let Some(context) = $crate::TraceCategoryContext::acquire_cached($category, &CACHE) {
                $crate::instant(&context, $name, $scope, &[$($crate::ArgValue::of($key, $val)),*]);
            }
        }
    }
}

/// Writes an instant event representing a single moment in time.
/// The number of `args` must not be greater than 15.
#[inline]
pub fn instant(
    context: &TraceCategoryContext,
    name: &'static CStr,
    scope: Scope,
    args: &[Arg<'_>],
) {
    assert!(args.len() <= 15, "no more than 15 trace arguments are supported");

    let name_ref = context.register_string_literal(name);
    context.write_instant(name_ref, scope, args);
}

/// Convenience macro for the `alert` function.
///
/// Example:
///
/// ```rust
/// alert!(c"foo", c"bar");
/// ```
///
/// is equivalent to
///
/// ```rust
/// alert(c"foo", c"bar");
/// ```
#[macro_export]
macro_rules! alert {
    ($category:expr, $name:expr) => {
        $crate::alert($category, $name)
    };
}

/// Sends an alert, which can be mapped to an action.
pub fn alert(category: &'static CStr, name: &'static CStr) {
    // trace_context_write_xxx functions require that:
    // - category and name are static null-terminated strings (`&'static CStr).
    // - the refs must be valid for the given call
    unsafe {
        let mut category_ref = mem::MaybeUninit::<sys::trace_string_ref_t>::uninit();
        let context =
            sys::trace_acquire_context_for_category(category.as_ptr(), category_ref.as_mut_ptr());
        if context != ptr::null() {
            sys::trace_context_send_alert(context, name.as_ptr());
            sys::trace_release_context(context);
        }
    }
}

/// Convenience macro for the `counter` function.
///
/// Example:
///
/// ```rust
/// let id = 555;
/// counter!(c"foo", c"bar", id, "x" => 5, "y" => 10);
/// ```
///
/// is equivalent to
///
/// ```rust
/// let id = 555;
/// counter(c"foo", c"bar", id,
///     &[ArgValue::of("x", 5), ArgValue::of("y", 10)]);
/// ```
#[macro_export]
macro_rules! counter {
    ($category:expr, $name:expr, $counter_id:expr $(, $key:expr => $val:expr)*) => {
        {
            static CACHE: $crate::trace_site_t = $crate::trace_site_t::new(0);
            if let Some(context) = $crate::TraceCategoryContext::acquire_cached($category, &CACHE) {
                $crate::counter(&context, $name, $counter_id,
                    &[$($crate::ArgValue::of($key, $val)),*])
            }
        }
    }
}

/// Writes a counter event with the specified id.
///
/// The arguments to this event are numeric samples and are typically
/// represented by the visualizer as a stacked area chart. The id serves to
/// distinguish multiple instances of counters which share the same category
/// and name within the same process.
///
/// 1 to 15 numeric arguments can be associated with an event, each of which is
/// interpreted as a distinct time series.
pub fn counter(
    context: &TraceCategoryContext,
    name: &'static CStr,
    counter_id: u64,
    args: &[Arg<'_>],
) {
    assert!(args.len() >= 1, "trace counter args must include at least one numeric argument");
    assert!(args.len() <= 15, "no more than 15 trace arguments are supported");

    let name_ref = context.register_string_literal(name);
    context.write_counter(name_ref, counter_id, args);
}

/// The scope of a duration event, returned by the `duration` function and the `duration!` macro.
/// The duration will be `end'ed` when this object is dropped.
#[must_use = "DurationScope must be `end`ed to be recorded"]
pub struct DurationScope<'a> {
    category: &'static CStr,
    name: &'static CStr,
    args: &'a [Arg<'a>],
    start_time: zx::BootTicks,
}

impl<'a> DurationScope<'a> {
    /// Starts a new duration scope that starts now and will be end'ed when
    /// this object is dropped.
    pub fn begin(category: &'static CStr, name: &'static CStr, args: &'a [Arg<'_>]) -> Self {
        let start_time = zx::BootTicks::get();
        Self { category, name, args, start_time }
    }
}

impl<'a> Drop for DurationScope<'a> {
    fn drop(&mut self) {
        if let Some(context) = TraceCategoryContext::acquire(self.category) {
            let name_ref = context.register_string_literal(self.name);
            context.write_duration(name_ref, self.start_time, self.args);
        }
    }
}

/// Write a "duration complete" record representing both the beginning and end of a duration.
pub fn complete_duration(
    category: &'static CStr,
    name: &'static CStr,
    start_time: zx::BootTicks,
    args: &[Arg<'_>],
) {
    if let Some(context) = TraceCategoryContext::acquire(category) {
        let name_ref = context.register_string_literal(name);
        context.write_duration(name_ref, start_time, args);
    }
}

/// Convenience macro for the `duration` function that can be used to trace
/// the duration of a scope. If you need finer grained control over when a
/// duration starts and stops, see `duration_begin` and `duration_end`.
///
/// Example:
///
/// ```rust
///   {
///       duration!(c"foo", c"bar", "x" => 5, "y" => 10);
///       ...
///       ...
///       // event will be recorded on drop.
///   }
/// ```
///
/// is equivalent to
///
/// ```rust
///   {
///       let mut args;
///       let _scope =  {
///           static CACHE: trace_site_t = trace_site_t::new(0);
///           if let Some(_context) = TraceCategoryContext::acquire_cached(c"foo", &CACHE) {
///               args = [ArgValue::of("x", 5), ArgValue::of("y", 10)];
///               Some($crate::duration(c"foo", c"bar", &args))
///           } else {
///               None
///           }
///       };
///       ...
///       ...
///       // event will be recorded on drop.
///   }
/// ```
#[macro_export]
macro_rules! duration {
    ($category:expr, $name:expr $(, $key:expr => $val:expr)* $(,)?) => {
        let mut args;
        let _scope =  {
            static CACHE: $crate::trace_site_t = $crate::trace_site_t::new(0);
            // NB: It is intentional that _context is not used here.  This cached context is used to
            // elide the expensive context lookup if tracing is disabled.  When the duration ends,
            // it will do a second lookup, but this cost is dwarfed by the cost of writing the trace
            // event, so this second lookup is irrelevant.  Retaining the context for the lifetime
            // of the DurationScope to avoid this second lookup would prevent the trace buffers from
            // flushing until the DurationScope is dropped.
            if let Some(_context) =
                    $crate::TraceCategoryContext::acquire_cached($category, &CACHE) {
                args = [$($crate::ArgValue::of($key, $val)),*];
                Some($crate::duration($category, $name, &args))
            } else {
                None
            }
        };
    }
}

/// Writes a duration event which ends when the current scope exits, or the
/// `end` method is manually called.
///
/// Durations describe work which is happening synchronously on one thread.
/// They can be nested to represent a control flow stack.
///
/// 0 to 15 arguments can be associated with the event, each of which is used
/// to annotate the duration with additional information.
///
/// NOTE: For performance reasons, it is advisable to create a cached context scope, which will
/// avoid expensive lookups when tracing is disabled.  See the example in the `duration!` macro.
pub fn duration<'a>(
    category: &'static CStr,
    name: &'static CStr,
    args: &'a [Arg<'_>],
) -> DurationScope<'a> {
    assert!(args.len() <= 15, "no more than 15 trace arguments are supported");
    DurationScope::begin(category, name, args)
}

/// Convenience macro for the `duration_begin` function.
///
/// Examples:
///
/// ```rust
/// duration_begin!(c"foo", c"bar", "x" => 5, "y" => "boo");
/// ```
///
/// ```rust
/// const FOO: &'static CStr = c"foo";
/// const BAR: &'static CStr = c"bar";
/// duration_begin!(FOO, BAR, "x" => 5, "y" => "boo");
/// ```
#[macro_export]
macro_rules! duration_begin {
    ($category:expr, $name:expr $(, $key:expr => $val:expr)* $(,)?) => {
        {
            static CACHE: $crate::trace_site_t = $crate::trace_site_t::new(0);
            if let Some(context) = $crate::TraceCategoryContext::acquire_cached($category, &CACHE) {
                $crate::duration_begin(&context, $name,
                                       &[$($crate::ArgValue::of($key, $val)),*])
            }
        }
    };
}

/// Convenience macro for the `duration_end` function.
///
/// Examples:
///
/// ```rust
/// duration_end!(c"foo", c"bar", "x" => 5, "y" => "boo");
/// ```
///
/// ```rust
/// const FOO: &'static CStr = c"foo";
/// const BAR: &'static CStr = c"bar";
/// duration_end!(FOO, BAR, "x" => 5, "y" => "boo");
/// ```
#[macro_export]
macro_rules! duration_end {
    ($category:expr, $name:expr $(, $key:expr => $val:expr)* $(,)?) => {
        {
            static CACHE: $crate::trace_site_t = $crate::trace_site_t::new(0);
            if let Some(context) = $crate::TraceCategoryContext::acquire_cached($category, &CACHE) {
                $crate::duration_end(&context, $name, &[$($crate::ArgValue::of($key, $val)),*])
            }
        }
    };
}

/// Writes a duration begin event only.
/// This event must be matched by a duration end event with the same category and name.
///
/// Durations describe work which is happening synchronously on one thread.
/// They can be nested to represent a control flow stack.
///
/// 0 to 15 arguments can be associated with the event, each of which is used
/// to annotate the duration with additional information.  The arguments provided
/// to matching duration begin and duration end events are combined together in
/// the trace; it is not necessary to repeat them.
pub fn duration_begin(context: &TraceCategoryContext, name: &'static CStr, args: &[Arg<'_>]) {
    let ticks = zx::BootTicks::get();
    assert!(args.len() <= 15, "no more than 15 trace arguments are supported");

    let name_ref = context.register_string_literal(name);
    context.write_duration_begin(ticks, name_ref, args);
}

/// Writes a duration end event only.
///
/// Durations describe work which is happening synchronously on one thread.
/// They can be nested to represent a control flow stack.
///
/// 0 to 15 arguments can be associated with the event, each of which is used
/// to annotate the duration with additional information.  The arguments provided
/// to matching duration begin and duration end events are combined together in
/// the trace; it is not necessary to repeat them.
pub fn duration_end(context: &TraceCategoryContext, name: &'static CStr, args: &[Arg<'_>]) {
    let ticks = zx::BootTicks::get();
    assert!(args.len() <= 15, "no more than 15 trace arguments are supported");

    let name_ref = context.register_string_literal(name);
    context.write_duration_end(ticks, name_ref, args);
}

/// AsyncScope maintains state around the context of async events generated via the
/// async_enter! macro.
#[must_use = "emits an end event when dropped, so if dropped immediately creates an essentially \
              zero length duration that should just be an instant instead"]
pub struct AsyncScope {
    // AsyncScope::end uses std::mem::forget to bypass AsyncScope's Drop impl, so if any fields
    // with Drop impls are added, AsyncScope::end should be updated.
    id: Id,
    category: &'static CStr,
    name: &'static CStr,
}
impl AsyncScope {
    /// Starts a new async event scope, generating a begin event now, and ended when the
    /// object is dropped.
    pub fn begin(id: Id, category: &'static CStr, name: &'static CStr, args: &[Arg<'_>]) -> Self {
        async_begin(id, category, name, args);
        Self { id, category, name }
    }

    /// Manually end the async event scope with `args` instead of waiting until the guard is
    /// dropped (which would end the event scope with an empty `args`).
    pub fn end(self, args: &[Arg<'_>]) {
        let Self { id, category, name } = self;
        async_end(id, category, name, args);
        std::mem::forget(self);
    }
}

impl Drop for AsyncScope {
    fn drop(&mut self) {
        // AsyncScope::end uses std::mem::forget to bypass this Drop impl (to avoid emitting
        // extraneous end events), so any logic added to this Drop impl (or any fields added to
        // AsyncScope that have Drop impls) should addressed (if necessary) in AsyncScope::end.
        let Self { id, category, name } = *self;
        async_end(id, category, name, &[]);
    }
}

/// Writes an async event which ends when the current scope exits, or the `end` method is is
/// manually called.
///
/// Async events describe concurrently-scheduled work items that may migrate between threads. They
/// may be nested by sharing id, and are otherwise differentiated by their id.
///
/// 0 to 15 arguments can be associated with the event, each of which is used to annotate the
/// duration with additional information.
pub fn async_enter(
    id: Id,
    category: &'static CStr,
    name: &'static CStr,
    args: &[Arg<'_>],
) -> AsyncScope {
    assert!(args.len() <= 15, "no more than 15 trace arguments are supported");
    AsyncScope::begin(id, category, name, args)
}

/// Convenience macro for the `async_enter` function, which can be used to trace the duration of a
/// scope containing async code. This macro returns the drop guard, which the caller may then
/// choose to manage.
///
/// Example:
///
/// ```rust
/// {
///     let id = Id::new();
///     let _guard = async_enter!(id, c"foo", c"bar", "x" => 5, "y" => 10);
///     ...
///     ...
///     // event recorded on drop
/// }
/// ```
///
/// is equivalent to
///
/// ```rust
/// {
///     let id = Id::new();
///     let _guard = AsyncScope::begin(id, c"foo", c"bar", &[ArgValue::of("x", 5),
///         ArgValue::of("y", 10)]);
///     ...
///     ...
///     // event recorded on drop
/// }
/// ```
///
/// Calls to async_enter! may be nested.
#[macro_export]
macro_rules! async_enter {
    ($id:expr, $category:expr, $name:expr $(, $key:expr => $val:expr)*) => {
        {
            static CACHE: $crate::trace_site_t = $crate::trace_site_t::new(0);
            if let Some(context) = $crate::TraceCategoryContext::acquire_cached($category, &CACHE) {
                Some($crate::AsyncScope::begin($id, $category, $name, &[$($crate::ArgValue::of($key, $val)),*]))
            } else {
                None
            }
        }
    }
}

/// Convenience macro for the `async_instant` function, which can be used to emit an async instant
/// event.
///
/// Example:
///
/// ```rust
/// {
///     let id = Id::new();
///     async_instant!(id, c"foo", c"bar", "x" => 5, "y" => 10);
/// }
/// ```
///
/// is equivalent to
///
/// ```rust
/// {
///     let id = Id::new();
///     async_instant(
///         id, c"foo", c"bar",
///         &[ArgValue::of(c"x", 5), ArgValue::of("y", 10)]
///     );
/// }
/// ```
#[macro_export]
macro_rules! async_instant {
    ($id:expr, $category:expr, $name:expr $(, $key:expr => $val:expr)*) => {
        {
            static CACHE: $crate::trace_site_t = $crate::trace_site_t::new(0);
            if let Some(context) = $crate::TraceCategoryContext::acquire_cached($category, &CACHE) {
                $crate::async_instant($id, &context, $name, &[$($crate::ArgValue::of($key, $val)),*]);
            }
        }
    }
}

/// Writes an async begin event. This event must be matched by an async end event with the same
/// id, category, and name. This function is intended to be called through use of the
/// `async_enter!` macro.
///
/// Async events describe concurrent work that may or may not migrate threads, or be otherwise
/// interleaved with other work on the same thread. They can be nested to represent a control
/// flow stack.
///
/// 0 to 15 arguments can be associated with the event, each of which is used to annotate the
/// async event with additional information. Arguments provided in matching async begin and end
/// events are combined together in the trace; it is not necessary to repeat them.
pub fn async_begin(id: Id, category: &'static CStr, name: &'static CStr, args: &[Arg<'_>]) {
    assert!(args.len() <= 15, "no more than 15 trace arguments are supported");

    if let Some(context) = TraceCategoryContext::acquire(category) {
        let name_ref = context.register_string_literal(name);
        context.write_async_begin(id, name_ref, args);
    }
}

/// Writes an async end event. This event must be associated with a prior async begin event
/// with the same id, category, and name. This function is intended to be called implicitly
/// when the `AsyncScope` object created through use of the `async_enter!` macro is dropped.
///
/// Async events describe concurrent work that may or may not migrate threads, or be otherwise
/// interleaved with other work on the same thread. They can be nested to represent a control
/// flow stack.
///
/// 0 to 15 arguments can be associated with the event, each of which is used to annotate the
/// async event with additional information. Arguments provided in matching async begin and end
/// events are combined together in the trace; it is not necessary to repeat them.
pub fn async_end(id: Id, category: &'static CStr, name: &'static CStr, args: &[Arg<'_>]) {
    assert!(args.len() <= 15, "no more than 15 trace arguments are supported");

    if let Some(context) = TraceCategoryContext::acquire(category) {
        let name_ref = context.register_string_literal(name);
        context.write_async_end(id, name_ref, args);
    }
}

/// Writes an async instant event with the specified id.
///
/// Asynchronous events describe work that is happening asynchronously and that
/// may span multiple threads.  Asynchronous events do not nest.  The id serves
/// to correlate the progress of distinct asynchronous operations that share
/// the same category and name within the same process.
///
/// 0 to 15 arguments can be associated with the event, each of which is used
/// to annotate the asynchronous operation with additional information.  The
/// arguments provided to matching async begin, async instant, and async end
/// events are combined together in the trace; it is not necessary to repeat them.
pub fn async_instant(
    id: Id,
    context: &TraceCategoryContext,
    name: &'static CStr,
    args: &[Arg<'_>],
) {
    assert!(args.len() <= 15, "no more than 15 trace arguments are supported");

    let name_ref = context.register_string_literal(name);
    context.write_async_instant(id, name_ref, args);
}

#[macro_export]
macro_rules! blob {
    ($category:expr, $name:expr, $bytes:expr $(, $key:expr => $val:expr)*) => {
        {
            static CACHE: $crate::trace_site_t = $crate::trace_site_t::new(0);
            if let Some(context) = $crate::TraceCategoryContext::acquire_cached($category, &CACHE) {
                $crate::blob_fn(&context, $name, $bytes, &[$($crate::ArgValue::of($key, $val)),*])
            }
        }
    }
}
pub fn blob_fn(
    context: &TraceCategoryContext,
    name: &'static CStr,
    bytes: &[u8],
    args: &[Arg<'_>],
) {
    let name_ref = context.register_string_literal(name);
    context.write_blob(name_ref, bytes, args);
}

/// Convenience macro for the `flow_begin` function.
///
/// Example:
///
/// ```rust
/// let flow_id = 1234;
/// flow_begin!(c"foo", c"bar", flow_id, "x" => 5, "y" => "boo");
/// ```
///
/// ```rust
/// const FOO: &'static CStr = c"foo";
/// const BAR: &'static CStr = c"bar";
/// flow_begin!(c"foo", c"bar", flow_id);
/// ```
#[macro_export]
macro_rules! flow_begin {
    ($category:expr, $name:expr, $flow_id:expr $(, $key:expr => $val:expr)*) => {
        {
            static CACHE: $crate::trace_site_t = $crate::trace_site_t::new(0);
            if let Some(context) = $crate::TraceCategoryContext::acquire_cached($category, &CACHE) {
                $crate::flow_begin(&context, $name, $flow_id,
                                   &[$($crate::ArgValue::of($key, $val)),*])
            }
        }
    }
}

/// Convenience macro for the `flow_step` function.
///
/// Example:
///
/// ```rust
/// let flow_id = 1234;
/// flow_step!(c"foo", c"bar", flow_id, "x" => 5, "y" => "boo");
/// ```
///
/// ```rust
/// const FOO: &'static CStr = c"foo";
/// const BAR: &'static CStr = c"bar";
/// flow_step!(c"foo", c"bar", flow_id);
/// ```
#[macro_export]
macro_rules! flow_step {
    ($category:expr, $name:expr, $flow_id:expr $(, $key:expr => $val:expr)*) => {
        {
            static CACHE: $crate::trace_site_t = $crate::trace_site_t::new(0);
            if let Some(context) = $crate::TraceCategoryContext::acquire_cached($category, &CACHE) {
                $crate::flow_step(&context, $name, $flow_id,
                                  &[$($crate::ArgValue::of($key, $val)),*])
            }
        }
    }
}

/// Convenience macro for the `flow_end` function.
///
/// Example:
///
/// ```rust
/// let flow_id = 1234;
/// flow_end!(c"foo", c"bar", flow_id, "x" => 5, "y" => "boo");
/// ```
///
/// ```rust
/// const FOO: &'static CStr = c"foo";
/// const BAR: &'static CStr = c"bar";
/// flow_end!(c"foo", c"bar", flow_id);
/// ```
#[macro_export]
macro_rules! flow_end {
    ($category:expr, $name:expr, $flow_id:expr $(, $key:expr => $val:expr)*) => {
        {
            static CACHE: $crate::trace_site_t = $crate::trace_site_t::new(0);
            if let Some(context) = $crate::TraceCategoryContext::acquire_cached($category, &CACHE) {
                $crate::flow_end(&context, $name, $flow_id,
                                 &[$($crate::ArgValue::of($key, $val)),*])
            }
        }
    }
}

/// Writes a flow begin event with the specified id.
/// This event may be followed by flow steps events and must be matched by
/// a flow end event with the same category, name, and id.
///
/// Flow events describe control flow handoffs between threads or across processes.
/// They are typically represented as arrows in a visualizer.  Flow arrows are
/// from the end of the duration event which encloses the beginning of the flow
/// to the beginning of the duration event which encloses the next step or the
/// end of the flow.  The id serves to correlate flows which share the same
/// category and name across processes.
///
/// This event must be enclosed in a duration event which represents where
/// the flow handoff occurs.
///
/// 0 to 15 arguments can be associated with the event, each of which is used
/// to annotate the flow with additional information.  The arguments provided
/// to matching flow begin, flow step, and flow end events are combined together
/// in the trace; it is not necessary to repeat them.
pub fn flow_begin(
    context: &TraceCategoryContext,
    name: &'static CStr,
    flow_id: Id,
    args: &[Arg<'_>],
) {
    let ticks = zx::BootTicks::get();
    assert!(args.len() <= 15, "no more than 15 trace arguments are supported");

    let name_ref = context.register_string_literal(name);
    context.write_flow_begin(ticks, name_ref, flow_id, args);
}

/// Writes a flow end event with the specified id.
///
/// Flow events describe control flow handoffs between threads or across processes.
/// They are typically represented as arrows in a visualizer.  Flow arrows are
/// from the end of the duration event which encloses the beginning of the flow
/// to the beginning of the duration event which encloses the next step or the
/// end of the flow.  The id serves to correlate flows which share the same
/// category and name across processes.
///
/// This event must be enclosed in a duration event which represents where
/// the flow handoff occurs.
///
/// 0 to 15 arguments can be associated with the event, each of which is used
/// to annotate the flow with additional information.  The arguments provided
/// to matching flow begin, flow step, and flow end events are combined together
/// in the trace; it is not necessary to repeat them.
pub fn flow_end(
    context: &TraceCategoryContext,
    name: &'static CStr,
    flow_id: Id,
    args: &[Arg<'_>],
) {
    let ticks = zx::BootTicks::get();
    assert!(args.len() <= 15, "no more than 15 trace arguments are supported");

    let name_ref = context.register_string_literal(name);
    context.write_flow_end(ticks, name_ref, flow_id, args);
}

/// Writes a flow step event with the specified id.
///
/// Flow events describe control flow handoffs between threads or across processes.
/// They are typically represented as arrows in a visualizer.  Flow arrows are
/// from the end of the duration event which encloses the beginning of the flow
/// to the beginning of the duration event which encloses the next step or the
/// end of the flow.  The id serves to correlate flows which share the same
/// category and name across processes.
///
/// This event must be enclosed in a duration event which represents where
/// the flow handoff occurs.
///
/// 0 to 15 arguments can be associated with the event, each of which is used
/// to annotate the flow with additional information.  The arguments provided
/// to matching flow begin, flow step, and flow end events are combined together
/// in the trace; it is not necessary to repeat them.
pub fn flow_step(
    context: &TraceCategoryContext,
    name: &'static CStr,
    flow_id: Id,
    args: &[Arg<'_>],
) {
    let ticks = zx::BootTicks::get();
    assert!(args.len() <= 15, "no more than 15 trace arguments are supported");

    let name_ref = context.register_string_literal(name);
    context.write_flow_step(ticks, name_ref, flow_id, args);
}

/// Convenience macro to emit the beginning of a flow attached to an instant event.
///
/// Flows must be attached to a duration event. This can be awkward when there isn't an obvious
/// duration event to attach to, or the relevant duration is very small, which makes visualizing
/// difficult. This emits a flow event wrapped in a self contained instant event that is also easy
/// to see in the tracing UI.
///
/// Example:
///
/// ```rust
/// let flow_id = 1234;
/// instaflow_begin!(c"category", c"flow", c"step", flow_id, "x" => 5, "y" => "boo");
/// ```
#[macro_export]
macro_rules! instaflow_begin {
    (
        $category:expr,
        $flow_name:expr,
        $step_name:expr,
        $flow_id:expr
        $(, $key:expr => $val:expr)*
    ) => {
        {
            static CACHE: $crate::trace_site_t = $crate::trace_site_t::new(0);
            if let Some(context) = $crate::TraceCategoryContext::acquire_cached($category, &CACHE) {
                $crate::instaflow_begin(
                    &context,
                    $flow_name,
                    $step_name,
                    $flow_id,
                    &[$($crate::ArgValue::of($key, $val)),*],
                )
            }
        }
    }
}

/// Convenience macro to emit the end of a flow attached to an instant event.
///
/// Flows must be attached to a duration event. This can be awkward when there isn't an obvious
/// duration event to attach to, or the relevant duration is very small, which makes visualizing
/// difficult. This emits a flow event wrapped in a self contained instant event that is also easy
/// to see in the tracing UI.
///
/// Example:
///
/// ```rust
/// let flow_id = 1234;
/// instaflow_end!(c"category", c"flow", c"step", flow_id, "x" => 5, "y" => "boo");
/// ```
#[macro_export]
macro_rules! instaflow_end {
    (
        $category:expr,
        $flow_name:expr,
        $step_name:expr,
        $flow_id:expr
        $(, $key:expr => $val:expr)*
    ) => {
        {
            static CACHE: $crate::trace_site_t = $crate::trace_site_t::new(0);
            if let Some(context) = $crate::TraceCategoryContext::acquire_cached($category, &CACHE) {
                $crate::instaflow_end(
                    &context,
                    $flow_name,
                    $step_name,
                    $flow_id,
                    &[$($crate::ArgValue::of($key, $val)),*],
                )
            }
        }
    }
}

/// Convenience macro to emit a step in a flow attached to an instant event.
///
/// Flows must be attached to a duration event. This can be awkward when there isn't an obvious
/// duration event to attach to, or the relevant duration is very small, which makes visualizing
/// difficult. This emits a flow event wrapped in a self contained instant event that is also easy
/// to see in the tracing UI.
///
/// Example:
///
/// ```rust
/// let flow_id = 1234;
/// instaflow_step!(c"category", c"flow", c"step", flow_id, "x" => 5, "y" => "boo");
/// ```
#[macro_export]
macro_rules! instaflow_step {
    (
        $category:expr,
        $flow_name:expr,
        $step_name:expr,
        $flow_id:expr
        $(, $key:expr => $val:expr)*
    ) => {
        {
            static CACHE: $crate::trace_site_t = $crate::trace_site_t::new(0);
            if let Some(context) = $crate::TraceCategoryContext::acquire_cached($category, &CACHE) {
                $crate::instaflow_step(
                    &context,
                    $flow_name,
                    $step_name,
                    $flow_id,
                    &[$($crate::ArgValue::of($key, $val)),*],
                )
            }
        }
    }
}

/// Convenience function to emit the beginning of a flow attached to an instant event.
///
/// Flow events describe control flow handoffs between threads or across processes. They are
/// typically represented as arrows in a visualizer. Flow arrows are from the end of the duration
/// event which encloses the beginning of the flow to the beginning of the duration event which
/// encloses the next step or the end of the flow. The id serves to correlate flows which share the
/// same category and name across processes.
///
/// 0 to 15 arguments can be associated with the event, each of which is used to annotate the flow
/// with additional information. The arguments provided to matching flow begin, flow step, and flow
/// end events are combined together in the trace; it is not necessary to repeat them.
pub fn instaflow_begin(
    context: &TraceCategoryContext,
    flow_name: &'static CStr,
    step_name: &'static CStr,
    flow_id: Id,
    args: &[Arg<'_>],
) {
    let ticks = zx::BootTicks::get();
    assert!(args.len() <= 15, "no more than 15 trace arguments are supported");

    let flow_name_ref = context.register_string_literal(flow_name);
    let step_name_ref = context.register_string_literal(step_name);

    context.write_duration_begin(ticks, step_name_ref, args);
    context.write_flow_begin(ticks, flow_name_ref, flow_id, args);
    context.write_duration_end(ticks, step_name_ref, args);
}

/// Convenience function to the end of a flow attached to an instant event.
///
/// Flow events describe control flow handoffs between threads or across processes. They are
/// typically represented as arrows in a visualizer. Flow arrows are from the end of the duration
/// event which encloses the beginning of the flow to the beginning of the duration event which
/// encloses the next step or the end of the flow. The id serves to correlate flows which share the
/// same category and name across processes.
///
/// 0 to 15 arguments can be associated with the event, each of which is used to annotate the flow
/// with additional information. The arguments provided to matching flow begin, flow step, and flow
/// end events are combined together in the trace; it is not necessary to repeat them.
pub fn instaflow_end(
    context: &TraceCategoryContext,
    flow_name: &'static CStr,
    step_name: &'static CStr,
    flow_id: Id,
    args: &[Arg<'_>],
) {
    let ticks = zx::BootTicks::get();
    assert!(args.len() <= 15, "no more than 15 trace arguments are supported");

    let flow_name_ref = context.register_string_literal(flow_name);
    let step_name_ref = context.register_string_literal(step_name);

    context.write_duration_begin(ticks, step_name_ref, args);
    context.write_flow_end(ticks, flow_name_ref, flow_id, args);
    context.write_duration_end(ticks, step_name_ref, args);
}

/// Convenience function to emit a step in a flow attached to an instant event.
///
/// Flow events describe control flow handoffs between threads or across processes. They are
/// typically represented as arrows in a visualizer. Flow arrows are from the end of the duration
/// event which encloses the beginning of the flow to the beginning of the duration event which
/// encloses the next step or the end of the flow. The id serves to correlate flows which share the
/// same category and name across processes.
///
/// 0 to 15 arguments can be associated with the event, each of which is used to annotate the flow
/// with additional information. The arguments provided to matching flow begin, flow step, and flow
/// end events are combined together in the trace; it is not necessary to repeat them.
pub fn instaflow_step(
    context: &TraceCategoryContext,
    flow_name: &'static CStr,
    step_name: &'static CStr,
    flow_id: Id,
    args: &[Arg<'_>],
) {
    let ticks = zx::BootTicks::get();
    assert!(args.len() <= 15, "no more than 15 trace arguments are supported");

    let flow_name_ref = context.register_string_literal(flow_name);
    let step_name_ref = context.register_string_literal(step_name);

    context.write_duration_begin(ticks, step_name_ref, args);
    context.write_flow_step(ticks, flow_name_ref, flow_id, args);
    context.write_duration_end(ticks, step_name_ref, args);
}

// translated from trace-engine/types.h for inlining
const fn trace_make_empty_string_ref() -> sys::trace_string_ref_t {
    sys::trace_string_ref_t {
        encoded_value: sys::TRACE_ENCODED_STRING_REF_EMPTY,
        inline_string: ptr::null(),
    }
}

#[inline]
fn trim_to_last_char_boundary(string: &str, max_len: usize) -> &[u8] {
    let mut len = string.len();
    if string.len() > max_len {
        // Trim to the last unicode character that fits within the max length.
        // We search for the last character boundary that is immediately followed
        // by another character boundary (end followed by beginning).
        len = max_len;
        while len > 0 {
            if string.is_char_boundary(len - 1) && string.is_char_boundary(len) {
                break;
            }
            len -= 1;
        }
    }
    &string.as_bytes()[0..len]
}

// translated from trace-engine/types.h for inlining
// The resulting `trace_string_ref_t` only lives as long as the input `string`.
#[inline]
fn trace_make_inline_string_ref(string: &str) -> sys::trace_string_ref_t {
    let len = string.len() as u16;
    if len == 0 {
        return trace_make_empty_string_ref();
    }

    let string = trim_to_last_char_boundary(string, sys::TRACE_ENCODED_STRING_REF_MAX_LENGTH);

    sys::trace_string_ref_t {
        encoded_value: sys::TRACE_ENCODED_STRING_REF_INLINE_FLAG | len,
        inline_string: string.as_ptr() as *const libc::c_char,
    }
}

/// RAII wrapper for a trace context for a specific category.
pub struct TraceCategoryContext {
    raw: *const sys::trace_context_t,
    category_ref: sys::trace_string_ref_t,
}

impl TraceCategoryContext {
    #[inline]
    pub fn acquire_cached(
        category: &'static CStr,
        site: &sys::trace_site_t,
    ) -> Option<TraceCategoryContext> {
        unsafe {
            // SAFETY: The call to `trace_acquire_context_for_category_cached` is sound because
            // all arguments are live and non-null. If this function returns a non-null
            // pointer then it also guarantees that `category_ref` will have been initialized.
            // Internally, it uses relaxed atomic semantics to load and store site.
            let mut category_ref = mem::MaybeUninit::<sys::trace_string_ref_t>::uninit();
            let raw = sys::trace_acquire_context_for_category_cached(
                category.as_ptr(),
                site.as_ptr(),
                category_ref.as_mut_ptr(),
            );
            if raw != ptr::null() {
                Some(TraceCategoryContext { raw, category_ref: category_ref.assume_init() })
            } else {
                None
            }
        }
    }

    pub fn acquire(category: &'static CStr) -> Option<TraceCategoryContext> {
        unsafe {
            let mut category_ref = mem::MaybeUninit::<sys::trace_string_ref_t>::uninit();
            let raw = sys::trace_acquire_context_for_category(
                category.as_ptr(),
                category_ref.as_mut_ptr(),
            );
            if raw != ptr::null() {
                Some(TraceCategoryContext { raw, category_ref: category_ref.assume_init() })
            } else {
                None
            }
        }
    }

    #[inline]
    pub fn register_string_literal(&self, name: &'static CStr) -> sys::trace_string_ref_t {
        unsafe {
            let mut name_ref = mem::MaybeUninit::<sys::trace_string_ref_t>::uninit();
            sys::trace_context_register_string_literal(
                self.raw,
                name.as_ptr(),
                name_ref.as_mut_ptr(),
            );
            name_ref.assume_init()
        }
    }

    #[inline]
    fn register_current_thread(&self) -> sys::trace_thread_ref_t {
        unsafe {
            let mut thread_ref = mem::MaybeUninit::<sys::trace_thread_ref_t>::uninit();
            sys::trace_context_register_current_thread(self.raw, thread_ref.as_mut_ptr());
            thread_ref.assume_init()
        }
    }

    #[inline]
    pub fn write_instant(&self, name_ref: sys::trace_string_ref_t, scope: Scope, args: &[Arg<'_>]) {
        let ticks = zx::BootTicks::get();
        let thread_ref = self.register_current_thread();
        unsafe {
            sys::trace_context_write_instant_event_record(
                self.raw,
                ticks.into_raw(),
                &thread_ref,
                &self.category_ref,
                &name_ref,
                scope.into_raw(),
                args.as_ptr() as *const sys::trace_arg_t,
                args.len(),
            );
        }
    }

    pub fn write_instant_with_inline_name(&self, name: &str, scope: Scope, args: &[Arg<'_>]) {
        let name_ref = trace_make_inline_string_ref(name);
        self.write_instant(name_ref, scope, args)
    }

    fn write_counter(&self, name_ref: sys::trace_string_ref_t, counter_id: u64, args: &[Arg<'_>]) {
        let ticks = zx::BootTicks::get();
        let thread_ref = self.register_current_thread();
        unsafe {
            sys::trace_context_write_counter_event_record(
                self.raw,
                ticks.into_raw(),
                &thread_ref,
                &self.category_ref,
                &name_ref,
                counter_id,
                args.as_ptr() as *const sys::trace_arg_t,
                args.len(),
            );
        }
    }

    pub fn write_counter_with_inline_name(&self, name: &str, counter_id: u64, args: &[Arg<'_>]) {
        let name_ref = trace_make_inline_string_ref(name);
        self.write_counter(name_ref, counter_id, args);
    }

    fn write_duration(
        &self,
        name_ref: sys::trace_string_ref_t,
        start_time: zx::BootTicks,
        args: &[Arg<'_>],
    ) {
        let ticks = zx::BootTicks::get();
        let thread_ref = self.register_current_thread();
        unsafe {
            sys::trace_context_write_duration_event_record(
                self.raw,
                start_time.into_raw(),
                ticks.into_raw(),
                &thread_ref,
                &self.category_ref,
                &name_ref,
                args.as_ptr() as *const sys::trace_arg_t,
                args.len(),
            );
        }
    }

    pub fn write_duration_with_inline_name(
        &self,
        name: &str,
        start_time: zx::BootTicks,
        args: &[Arg<'_>],
    ) {
        let name_ref = trace_make_inline_string_ref(name);
        self.write_duration(name_ref, start_time, args);
    }

    fn write_duration_begin(
        &self,
        ticks: zx::BootTicks,
        name_ref: sys::trace_string_ref_t,
        args: &[Arg<'_>],
    ) {
        let thread_ref = self.register_current_thread();
        unsafe {
            sys::trace_context_write_duration_begin_event_record(
                self.raw,
                ticks.into_raw(),
                &thread_ref,
                &self.category_ref,
                &name_ref,
                args.as_ptr() as *const sys::trace_arg_t,
                args.len(),
            );
        }
    }

    pub fn write_duration_begin_with_inline_name(&self, name: &str, args: &[Arg<'_>]) {
        let name_ref = trace_make_inline_string_ref(name);
        self.write_duration_begin(zx::BootTicks::get(), name_ref, args);
    }

    fn write_duration_end(
        &self,
        ticks: zx::BootTicks,
        name_ref: sys::trace_string_ref_t,
        args: &[Arg<'_>],
    ) {
        let thread_ref = self.register_current_thread();
        unsafe {
            sys::trace_context_write_duration_end_event_record(
                self.raw,
                ticks.into_raw(),
                &thread_ref,
                &self.category_ref,
                &name_ref,
                args.as_ptr() as *const sys::trace_arg_t,
                args.len(),
            );
        }
    }

    pub fn write_duration_end_with_inline_name(&self, name: &str, args: &[Arg<'_>]) {
        let name_ref = trace_make_inline_string_ref(name);
        self.write_duration_end(zx::BootTicks::get(), name_ref, args);
    }

    fn write_async_begin(&self, id: Id, name_ref: sys::trace_string_ref_t, args: &[Arg<'_>]) {
        let ticks = zx::BootTicks::get();
        let thread_ref = self.register_current_thread();
        unsafe {
            sys::trace_context_write_async_begin_event_record(
                self.raw,
                ticks.into_raw(),
                &thread_ref,
                &self.category_ref,
                &name_ref,
                id.into(),
                args.as_ptr() as *const sys::trace_arg_t,
                args.len(),
            );
        }
    }

    pub fn write_async_begin_with_inline_name(&self, id: Id, name: &str, args: &[Arg<'_>]) {
        let name_ref = trace_make_inline_string_ref(name);
        self.write_async_begin(id, name_ref, args);
    }

    fn write_async_end(&self, id: Id, name_ref: sys::trace_string_ref_t, args: &[Arg<'_>]) {
        let ticks = zx::BootTicks::get();
        let thread_ref = self.register_current_thread();
        unsafe {
            sys::trace_context_write_async_end_event_record(
                self.raw,
                ticks.into_raw(),
                &thread_ref,
                &self.category_ref,
                &name_ref,
                id.into(),
                args.as_ptr() as *const sys::trace_arg_t,
                args.len(),
            );
        }
    }

    pub fn write_async_end_with_inline_name(&self, id: Id, name: &str, args: &[Arg<'_>]) {
        let name_ref = trace_make_inline_string_ref(name);
        self.write_async_end(id, name_ref, args);
    }

    fn write_async_instant(&self, id: Id, name_ref: sys::trace_string_ref_t, args: &[Arg<'_>]) {
        let ticks = zx::BootTicks::get();
        let thread_ref = self.register_current_thread();
        unsafe {
            sys::trace_context_write_async_instant_event_record(
                self.raw,
                ticks.into_raw(),
                &thread_ref,
                &self.category_ref,
                &name_ref,
                id.into(),
                args.as_ptr() as *const sys::trace_arg_t,
                args.len(),
            );
        }
    }

    fn write_blob(&self, name_ref: sys::trace_string_ref_t, bytes: &[u8], args: &[Arg<'_>]) {
        let ticks = zx::BootTicks::get();
        let thread_ref = self.register_current_thread();
        unsafe {
            sys::trace_context_write_blob_event_record(
                self.raw,
                ticks.into_raw(),
                &thread_ref,
                &self.category_ref,
                &name_ref,
                bytes.as_ptr() as *const core::ffi::c_void,
                bytes.len(),
                args.as_ptr() as *const sys::trace_arg_t,
                args.len(),
            );
        }
    }

    fn write_flow_begin(
        &self,
        ticks: zx::BootTicks,
        name_ref: sys::trace_string_ref_t,
        flow_id: Id,
        args: &[Arg<'_>],
    ) {
        let thread_ref = self.register_current_thread();
        unsafe {
            sys::trace_context_write_flow_begin_event_record(
                self.raw,
                ticks.into_raw(),
                &thread_ref,
                &self.category_ref,
                &name_ref,
                flow_id.into(),
                args.as_ptr() as *const sys::trace_arg_t,
                args.len(),
            );
        }
    }

    fn write_flow_end(
        &self,
        ticks: zx::BootTicks,
        name_ref: sys::trace_string_ref_t,
        flow_id: Id,
        args: &[Arg<'_>],
    ) {
        let thread_ref = self.register_current_thread();
        unsafe {
            sys::trace_context_write_flow_end_event_record(
                self.raw,
                ticks.into_raw(),
                &thread_ref,
                &self.category_ref,
                &name_ref,
                flow_id.into(),
                args.as_ptr() as *const sys::trace_arg_t,
                args.len(),
            );
        }
    }

    fn write_flow_step(
        &self,
        ticks: zx::BootTicks,
        name_ref: sys::trace_string_ref_t,
        flow_id: Id,
        args: &[Arg<'_>],
    ) {
        let thread_ref = self.register_current_thread();
        unsafe {
            sys::trace_context_write_flow_step_event_record(
                self.raw,
                ticks.into_raw(),
                &thread_ref,
                &self.category_ref,
                &name_ref,
                flow_id.into(),
                args.as_ptr() as *const sys::trace_arg_t,
                args.len(),
            );
        }
    }
}

impl std::ops::Drop for TraceCategoryContext {
    fn drop(&mut self) {
        unsafe {
            sys::trace_release_context(self.raw);
        }
    }
}

/// RAII wrapper for trace contexts without a specific associated category.
pub struct Context {
    context: *const sys::trace_context_t,
}

impl Context {
    #[inline]
    pub fn acquire() -> Option<Self> {
        let context = unsafe { sys::trace_acquire_context() };
        if context.is_null() {
            None
        } else {
            Some(Self { context })
        }
    }

    #[inline]
    pub fn register_string_literal(&self, s: &'static CStr) -> sys::trace_string_ref_t {
        unsafe {
            let mut s_ref = mem::MaybeUninit::<sys::trace_string_ref_t>::uninit();
            sys::trace_context_register_string_literal(
                self.context,
                s.as_ptr(),
                s_ref.as_mut_ptr(),
            );
            s_ref.assume_init()
        }
    }

    pub fn write_blob_record(
        &self,
        type_: sys::trace_blob_type_t,
        name_ref: &sys::trace_string_ref_t,
        data: &[u8],
    ) {
        unsafe {
            sys::trace_context_write_blob_record(
                self.context,
                type_,
                name_ref as *const sys::trace_string_ref_t,
                data.as_ptr() as *const libc::c_void,
                data.len(),
            );
        }
    }

    // Write fxt formatted bytes to the trace buffer
    //
    // returns Ok(num_bytes_written) on success
    pub fn copy_record(&self, buffer: &[u64]) -> Option<usize> {
        unsafe {
            let ptr =
                sys::trace_context_alloc_record(self.context, 8 * buffer.len() as libc::size_t);
            if ptr == std::ptr::null_mut() {
                return None;
            }
            ptr.cast::<u64>().copy_from(buffer.as_ptr(), buffer.len());
        };
        Some(buffer.len())
    }

    pub fn buffering_mode(&self) -> BufferingMode {
        match unsafe { sys::trace_context_get_buffering_mode(self.context) } {
            sys::TRACE_BUFFERING_MODE_ONESHOT => BufferingMode::OneShot,
            sys::TRACE_BUFFERING_MODE_CIRCULAR => BufferingMode::Circular,
            sys::TRACE_BUFFERING_MODE_STREAMING => BufferingMode::Streaming,
            m => panic!("Unknown trace buffering mode: {:?}", m),
        }
    }
}

impl std::ops::Drop for Context {
    fn drop(&mut self) {
        unsafe { sys::trace_release_context(self.context) }
    }
}

pub struct ProlongedContext {
    context: *const sys::trace_prolonged_context_t,
}

impl ProlongedContext {
    pub fn acquire() -> Option<Self> {
        let context = unsafe { sys::trace_acquire_prolonged_context() };
        if context.is_null() {
            None
        } else {
            Some(Self { context })
        }
    }
}

impl Drop for ProlongedContext {
    fn drop(&mut self) {
        unsafe { sys::trace_release_prolonged_context(self.context) }
    }
}

unsafe impl Send for ProlongedContext {}

mod sys {
    #![allow(non_camel_case_types, unused)]
    use zx::sys::{zx_handle_t, zx_koid_t, zx_obj_type_t, zx_status_t, zx_ticks_t};

    pub type trace_ticks_t = zx_ticks_t;
    pub type trace_counter_id_t = u64;
    pub type trace_async_id_t = u64;
    pub type trace_flow_id_t = u64;
    pub type trace_thread_state_t = u32;
    pub type trace_cpu_number_t = u32;
    pub type trace_string_index_t = u32;
    pub type trace_thread_index_t = u32;
    pub type trace_context_t = libc::c_void;
    pub type trace_prolonged_context_t = libc::c_void;

    pub type trace_encoded_string_ref_t = u16;
    pub const TRACE_ENCODED_STRING_REF_EMPTY: trace_encoded_string_ref_t = 0;
    pub const TRACE_ENCODED_STRING_REF_INLINE_FLAG: trace_encoded_string_ref_t = 0x8000;
    pub const TRACE_ENCODED_STRING_REF_LENGTH_MASK: trace_encoded_string_ref_t = 0x7fff;
    pub const TRACE_ENCODED_STRING_REF_MAX_LENGTH: usize = 32000;
    pub const TRACE_ENCODED_STRING_REF_MIN_INDEX: trace_encoded_string_ref_t = 0x1;
    pub const TRACE_ENCODED_STRING_REF_MAX_INDEX: trace_encoded_string_ref_t = 0x7fff;

    pub type trace_encoded_thread_ref_t = u32;
    pub const TRACE_ENCODED_THREAD_REF_INLINE: trace_encoded_thread_ref_t = 0;
    pub const TRACE_ENCODED_THREAD_MIN_INDEX: trace_encoded_thread_ref_t = 0x01;
    pub const TRACE_ENCODED_THREAD_MAX_INDEX: trace_encoded_thread_ref_t = 0xff;

    pub type trace_state_t = libc::c_int;
    pub const TRACE_STOPPED: trace_state_t = 0;
    pub const TRACE_STARTED: trace_state_t = 1;
    pub const TRACE_STOPPING: trace_state_t = 2;

    pub type trace_scope_t = libc::c_int;
    pub const TRACE_SCOPE_THREAD: trace_scope_t = 0;
    pub const TRACE_SCOPE_PROCESS: trace_scope_t = 1;
    pub const TRACE_SCOPE_GLOBAL: trace_scope_t = 2;

    pub type trace_blob_type_t = libc::c_int;
    pub const TRACE_BLOB_TYPE_DATA: trace_blob_type_t = 1;
    pub const TRACE_BLOB_TYPE_LAST_BRANCH: trace_blob_type_t = 2;
    pub const TRACE_BLOB_TYPE_PERFETTO: trace_blob_type_t = 3;

    pub type trace_buffering_mode_t = libc::c_int;
    pub const TRACE_BUFFERING_MODE_ONESHOT: trace_buffering_mode_t = 0;
    pub const TRACE_BUFFERING_MODE_CIRCULAR: trace_buffering_mode_t = 1;
    pub const TRACE_BUFFERING_MODE_STREAMING: trace_buffering_mode_t = 2;

    #[repr(C)]
    #[derive(Copy, Clone)]
    pub struct trace_string_ref_t {
        pub encoded_value: trace_encoded_string_ref_t,
        pub inline_string: *const libc::c_char,
    }

    // trace_site_t is an opaque type that trace-engine uses per callsite to cache if the trace
    // point is enabled. Internally, it is a 8 byte allocation accessed with relaxed atomic
    // semantics.
    pub type trace_site_t = std::sync::atomic::AtomicU64;

    // A trace_string_ref_t object is created from a string slice.
    // The trace_string_ref_t object is contained inside an Arg object.
    // whose lifetime matches the string slice to ensure that the memory
    // cannot be de-allocated during the trace.
    //
    // trace_string_ref_t is safe for Send + Sync because the memory that
    // inline_string points to is guaranteed to be valid throughout the trace.
    //
    // For more information, see the ArgValue implementation for &str in this file.
    unsafe impl Send for trace_string_ref_t {}
    unsafe impl Sync for trace_string_ref_t {}

    #[repr(C)]
    pub struct trace_thread_ref_t {
        pub encoded_value: trace_encoded_thread_ref_t,
        pub inline_process_koid: zx_koid_t,
        pub inline_thread_koid: zx_koid_t,
    }

    #[repr(C)]
    pub struct trace_arg_t {
        pub name_ref: trace_string_ref_t,
        pub value: trace_arg_value_t,
    }

    #[repr(C)]
    pub union trace_arg_union_t {
        pub int32_value: i32,
        pub uint32_value: u32,
        pub int64_value: i64,
        pub uint64_value: u64,
        pub double_value: libc::c_double,
        pub string_value_ref: trace_string_ref_t,
        pub pointer_value: libc::uintptr_t,
        pub koid_value: zx_koid_t,
        pub bool_value: bool,
        pub reserved_for_future_expansion: [libc::uintptr_t; 2],
    }

    pub type trace_arg_type_t = libc::c_int;
    pub const TRACE_ARG_NULL: trace_arg_type_t = 0;
    pub const TRACE_ARG_INT32: trace_arg_type_t = 1;
    pub const TRACE_ARG_UINT32: trace_arg_type_t = 2;
    pub const TRACE_ARG_INT64: trace_arg_type_t = 3;
    pub const TRACE_ARG_UINT64: trace_arg_type_t = 4;
    pub const TRACE_ARG_DOUBLE: trace_arg_type_t = 5;
    pub const TRACE_ARG_STRING: trace_arg_type_t = 6;
    pub const TRACE_ARG_POINTER: trace_arg_type_t = 7;
    pub const TRACE_ARG_KOID: trace_arg_type_t = 8;
    pub const TRACE_ARG_BOOL: trace_arg_type_t = 9;

    #[repr(C)]
    pub struct trace_arg_value_t {
        pub type_: trace_arg_type_t,
        pub value: trace_arg_union_t,
    }

    #[repr(C)]
    pub struct trace_handler_ops_t {
        pub is_category_enabled:
            unsafe fn(handler: *const trace_handler_t, category: *const libc::c_char) -> bool,
        pub trace_started: unsafe fn(handler: *const trace_handler_t),
        pub trace_stopped: unsafe fn(
            handler: *const trace_handler_t,
            async_ptr: *const (), //async_t,
            disposition: zx_status_t,
            buffer_bytes_written: libc::size_t,
        ),
        pub buffer_overflow: unsafe fn(handler: *const trace_handler_t),
    }

    #[repr(C)]
    pub struct trace_handler_t {
        pub ops: *const trace_handler_ops_t,
    }

    // From libtrace-engine.so
    extern "C" {
        // From trace-engine/context.h

        pub fn trace_context_is_category_enabled(
            context: *const trace_context_t,
            category_literal: *const libc::c_char,
        ) -> bool;

        pub fn trace_context_register_string_copy(
            context: *const trace_context_t,
            string: *const libc::c_char,
            length: libc::size_t,
            out_ref: *mut trace_string_ref_t,
        );

        pub fn trace_context_register_string_literal(
            context: *const trace_context_t,
            string_literal: *const libc::c_char,
            out_ref: *mut trace_string_ref_t,
        );

        pub fn trace_context_register_category_literal(
            context: *const trace_context_t,
            category_literal: *const libc::c_char,
            out_ref: *mut trace_string_ref_t,
        ) -> bool;

        pub fn trace_context_register_current_thread(
            context: *const trace_context_t,
            out_ref: *mut trace_thread_ref_t,
        );

        pub fn trace_context_register_thread(
            context: *const trace_context_t,
            process_koid: zx_koid_t,
            thread_koid: zx_koid_t,
            out_ref: *mut trace_thread_ref_t,
        );

        pub fn trace_context_write_kernel_object_record(
            context: *const trace_context_t,
            koid: zx_koid_t,
            type_: zx_obj_type_t,
            args: *const trace_arg_t,
            num_args: libc::size_t,
        );

        pub fn trace_context_write_kernel_object_record_for_handle(
            context: *const trace_context_t,
            handle: zx_handle_t,
            args: *const trace_arg_t,
            num_args: libc::size_t,
        );

        pub fn trace_context_write_process_info_record(
            context: *const trace_context_t,
            process_koid: zx_koid_t,
            process_name_ref: *const trace_string_ref_t,
        );

        pub fn trace_context_write_thread_info_record(
            context: *const trace_context_t,
            process_koid: zx_koid_t,
            thread_koid: zx_koid_t,
            thread_name_ref: *const trace_string_ref_t,
        );

        pub fn trace_context_write_context_switch_record(
            context: *const trace_context_t,
            event_time: trace_ticks_t,
            cpu_number: trace_cpu_number_t,
            outgoing_thread_state: trace_thread_state_t,
            outgoing_thread_ref: *const trace_thread_ref_t,
            incoming_thread_ref: *const trace_thread_ref_t,
        );

        pub fn trace_context_write_log_record(
            context: *const trace_context_t,
            event_time: trace_ticks_t,
            thread_ref: *const trace_thread_ref_t,
            log_message: *const libc::c_char,
            log_message_length: libc::size_t,
        );

        pub fn trace_context_write_instant_event_record(
            context: *const trace_context_t,
            event_time: trace_ticks_t,
            thread_ref: *const trace_thread_ref_t,
            category_ref: *const trace_string_ref_t,
            name_ref: *const trace_string_ref_t,
            scope: trace_scope_t,
            args: *const trace_arg_t,
            num_args: libc::size_t,
        );

        pub fn trace_context_send_alert(context: *const trace_context_t, name: *const libc::c_char);

        pub fn trace_context_write_counter_event_record(
            context: *const trace_context_t,
            event_time: trace_ticks_t,
            thread_ref: *const trace_thread_ref_t,
            category_ref: *const trace_string_ref_t,
            name_ref: *const trace_string_ref_t,
            counter_id: trace_counter_id_t,
            args: *const trace_arg_t,
            num_args: libc::size_t,
        );

        pub fn trace_context_write_duration_event_record(
            context: *const trace_context_t,
            start_time: trace_ticks_t,
            end_time: trace_ticks_t,
            thread_ref: *const trace_thread_ref_t,
            category_ref: *const trace_string_ref_t,
            name_ref: *const trace_string_ref_t,
            args: *const trace_arg_t,
            num_args: libc::size_t,
        );

        pub fn trace_context_write_blob_event_record(
            context: *const trace_context_t,
            event_time: trace_ticks_t,
            thread_ref: *const trace_thread_ref_t,
            category_ref: *const trace_string_ref_t,
            name_ref: *const trace_string_ref_t,
            blob: *const libc::c_void,
            blob_size: libc::size_t,
            args: *const trace_arg_t,
            num_args: libc::size_t,
        );

        pub fn trace_context_write_duration_begin_event_record(
            context: *const trace_context_t,
            event_time: trace_ticks_t,
            thread_ref: *const trace_thread_ref_t,
            category_ref: *const trace_string_ref_t,
            name_ref: *const trace_string_ref_t,
            args: *const trace_arg_t,
            num_args: libc::size_t,
        );

        pub fn trace_context_write_duration_end_event_record(
            context: *const trace_context_t,
            event_time: trace_ticks_t,
            thread_ref: *const trace_thread_ref_t,
            category_ref: *const trace_string_ref_t,
            name_ref: *const trace_string_ref_t,
            args: *const trace_arg_t,
            num_args: libc::size_t,
        );

        pub fn trace_context_write_async_begin_event_record(
            context: *const trace_context_t,
            event_time: trace_ticks_t,
            thread_ref: *const trace_thread_ref_t,
            category_ref: *const trace_string_ref_t,
            name_ref: *const trace_string_ref_t,
            async_id: trace_async_id_t,
            args: *const trace_arg_t,
            num_args: libc::size_t,
        );

        pub fn trace_context_write_async_instant_event_record(
            context: *const trace_context_t,
            event_time: trace_ticks_t,
            thread_ref: *const trace_thread_ref_t,
            category_ref: *const trace_string_ref_t,
            name_ref: *const trace_string_ref_t,
            async_id: trace_async_id_t,
            args: *const trace_arg_t,
            num_args: libc::size_t,
        );

        pub fn trace_context_write_async_end_event_record(
            context: *const trace_context_t,
            event_time: trace_ticks_t,
            thread_ref: *const trace_thread_ref_t,
            category_ref: *const trace_string_ref_t,
            name_ref: *const trace_string_ref_t,
            async_id: trace_async_id_t,
            args: *const trace_arg_t,
            num_args: libc::size_t,
        );

        pub fn trace_context_write_flow_begin_event_record(
            context: *const trace_context_t,
            event_time: trace_ticks_t,
            thread_ref: *const trace_thread_ref_t,
            category_ref: *const trace_string_ref_t,
            name_ref: *const trace_string_ref_t,
            flow_id: trace_flow_id_t,
            args: *const trace_arg_t,
            num_args: libc::size_t,
        );

        pub fn trace_context_write_flow_step_event_record(
            context: *const trace_context_t,
            event_time: trace_ticks_t,
            thread_ref: *const trace_thread_ref_t,
            category_ref: *const trace_string_ref_t,
            name_ref: *const trace_string_ref_t,
            flow_id: trace_flow_id_t,
            args: *const trace_arg_t,
            num_args: libc::size_t,
        );

        pub fn trace_context_write_flow_end_event_record(
            context: *const trace_context_t,
            event_time: trace_ticks_t,
            thread_ref: *const trace_thread_ref_t,
            category_ref: *const trace_string_ref_t,
            name_ref: *const trace_string_ref_t,
            flow_id: trace_flow_id_t,
            args: *const trace_arg_t,
            num_args: libc::size_t,
        );

        pub fn trace_context_write_initialization_record(
            context: *const trace_context_t,
            ticks_per_second: u64,
        );

        pub fn trace_context_write_string_record(
            context: *const trace_context_t,
            index: trace_string_index_t,
            string: *const libc::c_char,
            length: libc::size_t,
        );

        pub fn trace_context_write_thread_record(
            context: *const trace_context_t,
            index: trace_thread_index_t,
            procss_koid: zx_koid_t,
            thread_koid: zx_koid_t,
        );

        pub fn trace_context_write_blob_record(
            context: *const trace_context_t,
            type_: trace_blob_type_t,
            name_ref: *const trace_string_ref_t,
            data: *const libc::c_void,
            size: libc::size_t,
        );

        pub fn trace_context_alloc_record(
            context: *const trace_context_t,
            num_bytes: libc::size_t,
        ) -> *mut libc::c_void;

        // From trace-engine/instrumentation.h

        pub fn trace_generate_nonce() -> u64;

        pub fn trace_state() -> trace_state_t;

        pub fn trace_is_category_enabled(category_literal: *const libc::c_char) -> bool;

        pub fn trace_acquire_context() -> *const trace_context_t;

        pub fn trace_acquire_context_for_category(
            category_literal: *const libc::c_char,
            out_ref: *mut trace_string_ref_t,
        ) -> *const trace_context_t;

        pub fn trace_acquire_context_for_category_cached(
            category_literal: *const libc::c_char,
            trace_site: *const u64,
            out_ref: *mut trace_string_ref_t,
        ) -> *const trace_context_t;

        pub fn trace_release_context(context: *const trace_context_t);

        pub fn trace_acquire_prolonged_context() -> *const trace_prolonged_context_t;

        pub fn trace_release_prolonged_context(context: *const trace_prolonged_context_t);

        pub fn trace_register_observer(event: zx_handle_t) -> zx_status_t;

        pub fn trace_unregister_observer(event: zx_handle_t) -> zx_status_t;

        pub fn trace_notify_observer_updated(event: zx_handle_t);

        pub fn trace_context_get_buffering_mode(
            context: *const trace_context_t,
        ) -> trace_buffering_mode_t;
    }
}

/// Arguments for `TraceFuture` and `TraceFutureExt`. Use `trace_future_args!` to construct this
/// object.
pub struct TraceFutureArgs<'a> {
    pub category: &'static CStr,
    pub name: &'static CStr,

    /// `TraceFuture::new` and `trace_future_args!` both check if the trace category is enabled. The
    /// trace context is acquired in `trace_future_args!` and is passed to `TraceFuture::new` to
    /// avoid acquiring it twice.
    pub context: Option<TraceCategoryContext>,

    /// The trace arguments to appear in every duration event written by the `TraceFuture`. `args`
    /// should be empty if `context` is `None`.
    pub args: Vec<Arg<'a>>,

    /// The flow id to use in the flow events that connect the duration events together. A flow id
    /// will be constructed with `Id::new()` if not provided.
    pub flow_id: Option<Id>,

    /// Use `trace_future_args!` to construct this object.
    pub _use_trace_future_args: (),
}

#[doc(hidden)]
#[macro_export]
macro_rules! __impl_trace_future_args {
    ($category:expr, $name:expr, $flow_id:expr $(, $key:expr => $val:expr)*) => {{
        {
            static CACHE: $crate::trace_site_t = $crate::trace_site_t::new(0);
            let context = $crate::TraceCategoryContext::acquire_cached($category, &CACHE);
            let args = if context.is_some() {
                vec![$($crate::ArgValue::of($key, $val)),*]
            } else {
                vec![]
            };
            $crate::TraceFutureArgs {
                category: $category,
                name: $name,
                context: context,
                args: args,
                flow_id: $flow_id,
                _use_trace_future_args: (),
            }
        }
    }};
}

/// Macro for constructing `TraceFutureArgs`. The trace arguments won't be constructed if the
/// category is not enabled. If the category becomes enabled while the `TraceFuture` is still alive
/// then the duration events will still be written but without the trace arguments.
///
/// Example:
///
/// ```
/// async move {
///     ....
/// }.trace(trace_future_args!(c"category", c"name", "x" => 5, "y" => 10)).await;
/// ```
#[macro_export]
macro_rules! trace_future_args {
    ($category:expr, $name:expr $(, $key:expr => $val:expr)*) => {
        $crate::__impl_trace_future_args!($category, $name, None $(,$key => $val)*)
    };
    ($category:expr, $name:expr, $flow_id:expr $(, $key:expr => $val:expr)*) => {
        $crate::__impl_trace_future_args!($category, $name, Some($flow_id) $(,$key => $val)*)
    };
}

/// Extension trait for tracing futures.
pub trait TraceFutureExt: Future + Sized {
    /// Wraps a `Future` in a `TraceFuture`.
    ///
    /// Example:
    ///
    /// ```rust
    /// future.trace(trace_future_args!(c"category", c"name")).await;
    /// ```
    ///
    /// Which is equivalent to:
    ///
    /// ```rust
    /// TraceFuture::new(trace_future_args!(c"category", c"name"), future).await;
    /// ```
    fn trace<'a>(self, args: TraceFutureArgs<'a>) -> TraceFuture<'a, Self> {
        TraceFuture::new(args, self)
    }
}

impl<T: Future + Sized> TraceFutureExt for T {}

/// Wraps a `Future` and writes duration events when the future is created, dropped, and every time
/// it's polled. The duration events are connected by flow events.
#[pin_project(PinnedDrop)]
pub struct TraceFuture<'a, Fut: Future> {
    category: &'static CStr,
    name: &'static CStr,
    args: Vec<Arg<'a>>,
    flow_id: Option<Id>,
    #[pin]
    // This future can be large (> 3000 bytes) so we Box it to avoid extra memcpy's when creating
    future: Pin<Box<Fut>>,
}

impl<'a, Fut: Future> TraceFuture<'a, Fut> {
    pub fn new(args: TraceFutureArgs<'a>, future: Fut) -> Self {
        debug_assert!(
            args.context.is_some() || args.args.is_empty(),
            "There should not be any trace arguments when the category is disabled"
        );
        let mut this = Self {
            category: args.category,
            name: args.name,
            args: args.args,
            flow_id: args.flow_id,
            future: Box::pin(future),
        };
        if let Some(context) = args.context {
            this.trace_create(context);
        }
        this
    }

    #[cold]
    fn trace_create(&mut self, context: TraceCategoryContext) {
        let name_ref = context.register_string_literal(self.name);
        let flow_id = self.flow_id.get_or_insert_with(Id::new);
        let duration_start = zx::BootTicks::get();
        context.write_flow_begin(zx::BootTicks::get(), name_ref, *flow_id, &[]);
        self.args.push(ArgValue::of("state", "created"));
        context.write_duration(name_ref, duration_start, &self.args);
        self.args.pop();
    }

    #[cold]
    fn trace_poll(
        self: Pin<&mut Self>,
        context: TraceCategoryContext,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<Fut::Output> {
        let this = self.project();
        let name_ref = context.register_string_literal(this.name);
        let flow_id = this.flow_id.get_or_insert_with(Id::new);
        let duration_start = zx::BootTicks::get();
        context.write_flow_step(zx::BootTicks::get(), name_ref, *flow_id, &[]);
        let result = this.future.poll(cx);
        let result_str: &'static str = if result.is_pending() { "pending" } else { "ready" };
        this.args.push(ArgValue::of("state", result_str));
        context.write_duration(name_ref, duration_start, &this.args);
        this.args.pop();
        result
    }

    #[cold]
    fn trace_drop(self: Pin<&mut Self>, context: TraceCategoryContext) {
        let this = self.project();
        let name_ref = context.register_string_literal(this.name);
        let flow_id = this.flow_id.get_or_insert_with(Id::new);
        let duration_start = zx::BootTicks::get();
        context.write_flow_end(zx::BootTicks::get(), name_ref, *flow_id, &[]);
        this.args.push(ArgValue::of("state", "dropped"));
        context.write_duration(name_ref, duration_start, &this.args);
        this.args.pop();
    }
}

impl<Fut: Future> Future for TraceFuture<'_, Fut> {
    type Output = Fut::Output;
    fn poll(self: Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> Poll<Fut::Output> {
        if let Some(context) = TraceCategoryContext::acquire(self.as_ref().get_ref().category) {
            self.trace_poll(context, cx)
        } else {
            self.project().future.poll(cx)
        }
    }
}

#[pin_project::pinned_drop]
impl<Fut: Future> PinnedDrop for TraceFuture<'_, Fut> {
    fn drop(self: Pin<&mut Self>) {
        if let Some(context) = TraceCategoryContext::acquire(self.as_ref().get_ref().category) {
            self.trace_drop(context);
        }
    }
}

#[cfg(test)]
mod test {
    use super::{trim_to_last_char_boundary, Id};

    #[test]
    fn trim_to_last_char_boundary_trims_to_last_character_boundary() {
        assert_eq!(b"x", trim_to_last_char_boundary("x", 5));
        assert_eq!(b"x", trim_to_last_char_boundary("x", 1));
        assert_eq!(b"", trim_to_last_char_boundary("x", 0));
        assert_eq!(b"xxxxx", trim_to_last_char_boundary("xxxxx", 6));
        assert_eq!(b"xxxxx", trim_to_last_char_boundary("xxxxx", 5));
        assert_eq!(b"xxxx", trim_to_last_char_boundary("xxxxx", 4));

        assert_eq!("💩".as_bytes(), trim_to_last_char_boundary("💩", 5));
        assert_eq!("💩".as_bytes(), trim_to_last_char_boundary("💩", 4));
        assert_eq!(b"", trim_to_last_char_boundary("💩", 3));
    }

    // Here, we're looking to make sure that successive calls to the function generate distinct
    // values. How those values are distinct is not particularly meaningful; the current
    // implementation yields sequential values, but that's not a behavior to rely on.
    #[test]
    fn test_id_new() {
        assert_ne!(Id::new(), Id::new());
    }
}
