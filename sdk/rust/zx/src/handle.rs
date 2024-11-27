// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Type-safe bindings for Zircon handles.
//!
use crate::{
    object_get_info_single, object_get_property, object_set_property, ok, sys, MonotonicInstant,
    Name, ObjectQuery, Port, Property, PropertyQuery, Rights, Signals, Status, Topic,
    WaitAsyncOpts,
};
use std::marker::PhantomData;
use std::mem::{self, ManuallyDrop};

#[derive(
    Debug,
    Copy,
    Clone,
    Eq,
    PartialEq,
    Ord,
    PartialOrd,
    Hash,
    zerocopy::FromBytes,
    zerocopy::KnownLayout,
    zerocopy::Immutable,
)]
#[repr(transparent)]
pub struct Koid(sys::zx_koid_t);

impl Koid {
    pub fn from_raw(raw: sys::zx_koid_t) -> Koid {
        Koid(raw)
    }

    pub fn raw_koid(&self) -> sys::zx_koid_t {
        self.0
    }
}

/// An object representing a Zircon
/// [handle](https://fuchsia.dev/fuchsia-src/concepts/objects/handles).
///
/// Internally, it is represented as a 32-bit integer, but this wrapper enforces
/// strict ownership semantics. The `Drop` implementation closes the handle.
///
/// This type represents the most general reference to a kernel object, and can
/// be interconverted to and from more specific types. Those conversions are not
/// enforced in the type system; attempting to use them will result in errors
/// returned by the kernel. These conversions don't change the underlying
/// representation, but do change the type and thus what operations are available.
#[derive(Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
#[repr(transparent)]
pub struct Handle(sys::zx_handle_t);

impl AsHandleRef for Handle {
    fn as_handle_ref(&self) -> HandleRef<'_> {
        Unowned { inner: ManuallyDrop::new(Handle(self.0)), marker: PhantomData }
    }
}

impl HandleBased for Handle {}

impl Drop for Handle {
    fn drop(&mut self) {
        if self.0 != sys::ZX_HANDLE_INVALID {
            unsafe { sys::zx_handle_close(self.0) };
        }
    }
}

impl Handle {
    /// Initialize a handle backed by ZX_HANDLE_INVALID, the only safe non-handle.
    #[inline(always)]
    pub const fn invalid() -> Handle {
        Handle(sys::ZX_HANDLE_INVALID)
    }

    /// If a raw handle is obtained from some other source, this method converts
    /// it into a type-safe owned handle.
    ///
    /// # Safety
    ///
    /// `raw` must either be a valid handle (i.e. not dangling), or
    /// `ZX_HANDLE_INVALID`. If `raw` is a valid handle, then either:
    /// - `raw` may be closed manually and the returned `Handle` must not be
    ///   dropped.
    /// - Or `raw` must not be closed until the returned `Handle` is dropped, at
    ///   which time it will close `raw`.
    pub const unsafe fn from_raw(raw: sys::zx_handle_t) -> Handle {
        Handle(raw)
    }

    pub fn is_invalid(&self) -> bool {
        self.0 == sys::ZX_HANDLE_INVALID
    }

    pub fn replace(self, rights: Rights) -> Result<Handle, Status> {
        let handle = self.0;
        let mut out = 0;
        let status = unsafe { sys::zx_handle_replace(handle, rights.bits(), &mut out) };
        // zx_handle_replace always invalidates |handle| so we can't run our drop handler.
        std::mem::forget(self);
        ok(status).map(|()| Handle(out))
    }
}

struct NameProperty();
unsafe impl PropertyQuery for NameProperty {
    const PROPERTY: Property = Property::NAME;
    // SAFETY: this type is correctly sized and the kernel guarantees that it will be
    // null-terminated like the type requires.
    type PropTy = Name;
}

/// A borrowed value of type `T`.
///
/// This is primarily used for working with borrowed values of `HandleBased` types.
#[derive(Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
#[repr(transparent)]
pub struct Unowned<'a, T> {
    inner: ManuallyDrop<T>,
    marker: PhantomData<&'a T>,
}

impl<'a, T> ::std::ops::Deref for Unowned<'a, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &*self.inner
    }
}

impl<T: HandleBased> Clone for Unowned<'_, T> {
    fn clone(&self) -> Self {
        unsafe { Self::from_raw_handle(self.inner.as_handle_ref().raw_handle()) }
    }
}

pub type HandleRef<'a> = Unowned<'a, Handle>;

impl<'a, T: HandleBased> Unowned<'a, T> {
    /// Create a `HandleRef` from a raw handle. Use this method when you are given a raw handle but
    /// should not take ownership of it. Examples include process-global handles like the root
    /// VMAR. This method should be called with an explicitly provided lifetime that must not
    /// outlive the lifetime during which the handle is owned by the current process. It is unsafe
    /// because most of the time, it is better to use a `Handle` to prevent leaking resources.
    ///
    /// # Safety
    ///
    /// `handle` must be a valid handle (i.e. not dangling), or
    /// `ZX_HANDLE_INVALID`. If `handle` is a valid handle, then it must not be
    /// closed for the lifetime `'a`.
    pub unsafe fn from_raw_handle(handle: sys::zx_handle_t) -> Self {
        Unowned { inner: ManuallyDrop::new(T::from(Handle::from_raw(handle))), marker: PhantomData }
    }

    pub fn raw_handle(&self) -> sys::zx_handle_t {
        self.inner.raw_handle()
    }

    pub fn duplicate(&self, rights: Rights) -> Result<T, Status> {
        let mut out = 0;
        let status =
            unsafe { sys::zx_handle_duplicate(self.raw_handle(), rights.bits(), &mut out) };
        ok(status).map(|()| T::from(Handle(out)))
    }

    pub fn signal(&self, clear_mask: Signals, set_mask: Signals) -> Result<(), Status> {
        let status =
            unsafe { sys::zx_object_signal(self.raw_handle(), clear_mask.bits(), set_mask.bits()) };
        ok(status)
    }

    pub fn wait(&self, signals: Signals, deadline: MonotonicInstant) -> Result<Signals, Status> {
        let mut pending = Signals::empty().bits();
        let status = unsafe {
            sys::zx_object_wait_one(
                self.raw_handle(),
                signals.bits(),
                deadline.into_nanos(),
                &mut pending,
            )
        };
        ok(status).map(|()| Signals::from_bits_truncate(pending))
    }

    pub fn wait_async(
        &self,
        port: &Port,
        key: u64,
        signals: Signals,
        options: WaitAsyncOpts,
    ) -> Result<(), Status> {
        let status = unsafe {
            sys::zx_object_wait_async(
                self.raw_handle(),
                port.raw_handle(),
                key,
                signals.bits(),
                options.bits(),
            )
        };
        ok(status)
    }
}

impl<'a> Unowned<'a, Handle> {
    /// Convert this HandleRef to one of a specific type.
    pub fn cast<T: HandleBased>(self) -> Unowned<'a, T> {
        // SAFETY: this function's guarantees are upheld by the self input.
        unsafe { Unowned::from_raw_handle(self.raw_handle()) }
    }
}

/// A trait to get a reference to the underlying handle of an object.
pub trait AsHandleRef {
    /// Get a reference to the handle. One important use of such a reference is
    /// for `object_wait_many`.
    fn as_handle_ref(&self) -> HandleRef<'_>;

    /// Interpret the reference as a raw handle (an integer type). Two distinct
    /// handles will have different raw values (so it can perhaps be used as a
    /// key in a data structure).
    fn raw_handle(&self) -> sys::zx_handle_t {
        self.as_handle_ref().inner.0
    }

    /// Set and clear userspace-accessible signal bits on an object. Wraps the
    /// [zx_object_signal](https://fuchsia.dev/fuchsia-src/reference/syscalls/object_signal.md)
    /// syscall.
    fn signal_handle(&self, clear_mask: Signals, set_mask: Signals) -> Result<(), Status> {
        self.as_handle_ref().signal(clear_mask, set_mask)
    }

    /// Waits on a handle. Wraps the
    /// [zx_object_wait_one](https://fuchsia.dev/fuchsia-src/reference/syscalls/object_wait_one.md)
    /// syscall.
    fn wait_handle(&self, signals: Signals, deadline: MonotonicInstant) -> Result<Signals, Status> {
        self.as_handle_ref().wait(signals, deadline)
    }

    /// Causes packet delivery on the given port when the object changes state and matches signals.
    /// [zx_object_wait_async](https://fuchsia.dev/fuchsia-src/reference/syscalls/object_wait_async.md)
    /// syscall.
    fn wait_async_handle(
        &self,
        port: &Port,
        key: u64,
        signals: Signals,
        options: WaitAsyncOpts,
    ) -> Result<(), Status> {
        self.as_handle_ref().wait_async(port, key, signals, options)
    }

    /// Get the [Property::NAME] property for this object.
    ///
    /// Wraps a call to the
    /// [zx_object_get_property](https://fuchsia.dev/fuchsia-src/reference/syscalls/object_get_property.md)
    /// syscall for the ZX_PROP_NAME property.
    fn get_name(&self) -> Result<Name, Status> {
        object_get_property::<NameProperty>(self.as_handle_ref())
    }

    /// Set the [Property::NAME] property for this object.
    ///
    /// The name's length must be less than [sys::ZX_MAX_NAME_LEN], i.e.
    /// name.[to_bytes_with_nul()](CStr::to_bytes_with_nul()).len() <= [sys::ZX_MAX_NAME_LEN], or
    /// Err([Status::INVALID_ARGS]) will be returned.
    ///
    /// Wraps a call to the
    /// [zx_object_get_property](https://fuchsia.dev/fuchsia-src/reference/syscalls/object_get_property.md)
    /// syscall for the ZX_PROP_NAME property.
    fn set_name(&self, name: &Name) -> Result<(), Status> {
        object_set_property::<NameProperty>(self.as_handle_ref(), &name)
    }

    /// Wraps the
    /// [zx_object_get_info](https://fuchsia.dev/fuchsia-src/reference/syscalls/object_get_info.md)
    /// syscall for the ZX_INFO_HANDLE_BASIC topic.
    fn basic_info(&self) -> Result<HandleBasicInfo, Status> {
        Ok(HandleBasicInfo::from(object_get_info_single::<HandleBasicInfoQuery>(
            self.as_handle_ref(),
        )?))
    }

    /// Wraps the
    /// [zx_object_get_info](https://fuchsia.dev/fuchsia-src/reference/syscalls/object_get_info.md)
    /// syscall for the ZX_INFO_HANDLE_COUNT topic.
    fn count_info(&self) -> Result<HandleCountInfo, Status> {
        Ok(HandleCountInfo::from(object_get_info_single::<HandleCountInfoQuery>(
            self.as_handle_ref(),
        )?))
    }

    /// Returns the koid (kernel object ID) for this handle.
    fn get_koid(&self) -> Result<Koid, Status> {
        self.basic_info().map(|info| info.koid)
    }
}

impl<'a, T: HandleBased> AsHandleRef for Unowned<'a, T> {
    fn as_handle_ref(&self) -> HandleRef<'_> {
        Unowned { inner: ManuallyDrop::new(Handle(self.raw_handle())), marker: PhantomData }
    }
}

impl<T: AsHandleRef> AsHandleRef for &T {
    fn as_handle_ref(&self) -> HandleRef<'_> {
        (*self).as_handle_ref()
    }
}

/// A trait implemented by all handle-based types.
///
/// Note: it is reasonable for user-defined objects wrapping a handle to implement
/// this trait. For example, a specific interface in some protocol might be
/// represented as a newtype of `Channel`, and implement the `as_handle_ref`
/// method and the `From<Handle>` trait to facilitate conversion from and to the
/// interface.
pub trait HandleBased: AsHandleRef + From<Handle> + Into<Handle> {
    /// Duplicate a handle, possibly reducing the rights available. Wraps the
    /// [zx_handle_duplicate](https://fuchsia.dev/fuchsia-src/reference/syscalls/handle_duplicate.md)
    /// syscall.
    fn duplicate_handle(&self, rights: Rights) -> Result<Self, Status> {
        self.as_handle_ref().duplicate(rights).map(|handle| Self::from(handle))
    }

    /// Create a replacement for a handle, possibly reducing the rights available. This invalidates
    /// the original handle. Wraps the
    /// [zx_handle_replace](https://fuchsia.dev/fuchsia-src/reference/syscalls/handle_replace.md)
    /// syscall.
    fn replace_handle(self, rights: Rights) -> Result<Self, Status> {
        <Self as Into<Handle>>::into(self).replace(rights).map(|handle| Self::from(handle))
    }

    /// Converts the value into its inner handle.
    ///
    /// This is a convenience function which simply forwards to the `Into` trait.
    fn into_handle(self) -> Handle {
        self.into()
    }

    /// Converts the handle into it's raw representation.
    ///
    /// The caller takes ownership over the raw handle, and must close or transfer it to avoid a handle leak.
    fn into_raw(self) -> sys::zx_handle_t {
        let h = self.into_handle();
        let r = h.0;
        mem::forget(h);
        r
    }

    /// Creates an instance of this type from a handle.
    ///
    /// This is a convenience function which simply forwards to the `From` trait.
    fn from_handle(handle: Handle) -> Self {
        Self::from(handle)
    }

    /// Creates an instance of another handle-based type from this value's inner handle.
    fn into_handle_based<H: HandleBased>(self) -> H {
        H::from_handle(self.into_handle())
    }

    /// Creates an instance of this type from the inner handle of another
    /// handle-based type.
    fn from_handle_based<H: HandleBased>(h: H) -> Self {
        Self::from_handle(h.into_handle())
    }

    fn is_invalid_handle(&self) -> bool {
        self.as_handle_ref().is_invalid()
    }
}

/// A trait implemented by all handles for objects which have a peer.
pub trait Peered: HandleBased {
    /// Set and clear userspace-accessible signal bits on the object's peer. Wraps the
    /// [zx_object_signal_peer][osp] syscall.
    ///
    /// [osp]: https://fuchsia.dev/fuchsia-src/reference/syscalls/object_signal_peer.md
    fn signal_peer(&self, clear_mask: Signals, set_mask: Signals) -> Result<(), Status> {
        let handle = self.raw_handle();
        let status =
            unsafe { sys::zx_object_signal_peer(handle, clear_mask.bits(), set_mask.bits()) };
        ok(status)
    }

    /// Returns true if the handle has received the `PEER_CLOSED` signal.
    ///
    /// # Errors
    ///
    /// See https://fuchsia.dev/reference/syscalls/object_wait_one?hl=en#errors for a full list of
    /// errors. Note that `Status::TIMED_OUT` errors are converted to `Ok(false)` and all other
    /// errors are propagated.
    fn is_closed(&self) -> Result<bool, Status> {
        match self.wait_handle(Signals::OBJECT_PEER_CLOSED, MonotonicInstant::INFINITE_PAST) {
            Ok(signals) => Ok(signals.contains(Signals::OBJECT_PEER_CLOSED)),
            Err(Status::TIMED_OUT) => Ok(false),
            Err(e) => Err(e),
        }
    }
}

/// Zircon object types.
#[derive(Copy, Clone, Ord, PartialOrd, Eq, PartialEq, Hash)]
#[repr(transparent)]
pub struct ObjectType(sys::zx_obj_type_t);

assoc_values!(ObjectType, [
    NONE            = sys::ZX_OBJ_TYPE_NONE;
    PROCESS         = sys::ZX_OBJ_TYPE_PROCESS;
    THREAD          = sys::ZX_OBJ_TYPE_THREAD;
    VMO             = sys::ZX_OBJ_TYPE_VMO;
    CHANNEL         = sys::ZX_OBJ_TYPE_CHANNEL;
    EVENT           = sys::ZX_OBJ_TYPE_EVENT;
    PORT            = sys::ZX_OBJ_TYPE_PORT;
    INTERRUPT       = sys::ZX_OBJ_TYPE_INTERRUPT;
    PCI_DEVICE      = sys::ZX_OBJ_TYPE_PCI_DEVICE;
    DEBUGLOG        = sys::ZX_OBJ_TYPE_DEBUGLOG;
    SOCKET          = sys::ZX_OBJ_TYPE_SOCKET;
    RESOURCE        = sys::ZX_OBJ_TYPE_RESOURCE;
    EVENTPAIR       = sys::ZX_OBJ_TYPE_EVENTPAIR;
    JOB             = sys::ZX_OBJ_TYPE_JOB;
    VMAR            = sys::ZX_OBJ_TYPE_VMAR;
    FIFO            = sys::ZX_OBJ_TYPE_FIFO;
    GUEST           = sys::ZX_OBJ_TYPE_GUEST;
    VCPU            = sys::ZX_OBJ_TYPE_VCPU;
    TIMER           = sys::ZX_OBJ_TYPE_TIMER;
    IOMMU           = sys::ZX_OBJ_TYPE_IOMMU;
    BTI             = sys::ZX_OBJ_TYPE_BTI;
    PROFILE         = sys::ZX_OBJ_TYPE_PROFILE;
    PMT             = sys::ZX_OBJ_TYPE_PMT;
    SUSPEND_TOKEN   = sys::ZX_OBJ_TYPE_SUSPEND_TOKEN;
    PAGER           = sys::ZX_OBJ_TYPE_PAGER;
    EXCEPTION       = sys::ZX_OBJ_TYPE_EXCEPTION;
    CLOCK           = sys::ZX_OBJ_TYPE_CLOCK;
    STREAM          = sys::ZX_OBJ_TYPE_STREAM;
    MSI             = sys::ZX_OBJ_TYPE_MSI;
    IOB             = sys::ZX_OBJ_TYPE_IOB;
]);

impl ObjectType {
    /// Creates an `ObjectType` from the underlying zircon type.
    pub const fn from_raw(raw: sys::zx_obj_type_t) -> Self {
        Self(raw)
    }

    /// Converts `ObjectType` into the underlying zircon type.
    pub const fn into_raw(self) -> sys::zx_obj_type_t {
        self.0
    }
}

/// Basic information about a handle.
///
/// Wrapper for data returned from [Handle::basic_info()].
#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub struct HandleBasicInfo {
    pub koid: Koid,
    pub rights: Rights,
    pub object_type: ObjectType,
    pub related_koid: Koid,
}

impl Default for HandleBasicInfo {
    fn default() -> Self {
        Self::from(sys::zx_info_handle_basic_t::default())
    }
}

impl From<sys::zx_info_handle_basic_t> for HandleBasicInfo {
    fn from(info: sys::zx_info_handle_basic_t) -> Self {
        let sys::zx_info_handle_basic_t { koid, rights, type_, related_koid, .. } = info;

        // Note lossy conversion of Rights and HandleProperty here if either of those types are out
        // of date or incomplete.
        HandleBasicInfo {
            koid: Koid(koid),
            rights: Rights::from_bits_truncate(rights),
            object_type: ObjectType(type_),
            related_koid: Koid(related_koid),
        }
    }
}

// zx_info_handle_basic_t is able to be safely replaced with a byte representation and is a PoD
// type.
struct HandleBasicInfoQuery;
unsafe impl ObjectQuery for HandleBasicInfoQuery {
    const TOPIC: Topic = Topic::HANDLE_BASIC;
    type InfoTy = sys::zx_info_handle_basic_t;
}

sys::zx_info_handle_count_t!(HandleCountInfo);

impl From<sys::zx_info_handle_count_t> for HandleCountInfo {
    fn from(sys::zx_info_handle_count_t { handle_count }: sys::zx_info_handle_count_t) -> Self {
        HandleCountInfo { handle_count }
    }
}

// zx_info_handle_count_t is able to be safely replaced with a byte representation and is a PoD
// type.
struct HandleCountInfoQuery;
unsafe impl ObjectQuery for HandleCountInfoQuery {
    const TOPIC: Topic = Topic::HANDLE_COUNT;
    type InfoTy = sys::zx_info_handle_count_t;
}

/// Handle operation.
#[derive(Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub enum HandleOp<'a> {
    Move(Handle),
    Duplicate(HandleRef<'a>),
}

/// Operation to perform on handles during write. ABI-compatible with `zx_handle_disposition_t`.
#[derive(Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
#[repr(C)]
pub struct HandleDisposition<'a> {
    // Must be either ZX_HANDLE_OP_MOVE or ZX_HANDLE_OP_DUPLICATE.
    operation: sys::zx_handle_op_t,
    // ZX_HANDLE_OP_MOVE==owned, ZX_HANDLE_OP_DUPLICATE==borrowed.
    handle: sys::zx_handle_t,
    // Preserve a borrowed handle's lifetime. Does not occupy any layout.
    _handle_lifetime: std::marker::PhantomData<&'a ()>,

    pub object_type: ObjectType,
    pub rights: Rights,
    pub result: Status,
}

static_assertions::assert_eq_size!(HandleDisposition<'_>, sys::zx_handle_disposition_t);
static_assertions::const_assert_eq!(
    std::mem::offset_of!(HandleDisposition<'_>, operation),
    std::mem::offset_of!(sys::zx_handle_disposition_t, operation)
);
static_assertions::const_assert_eq!(
    std::mem::offset_of!(HandleDisposition<'_>, handle),
    std::mem::offset_of!(sys::zx_handle_disposition_t, handle)
);
static_assertions::const_assert_eq!(
    std::mem::offset_of!(HandleDisposition<'_>, object_type),
    std::mem::offset_of!(sys::zx_handle_disposition_t, type_)
);
static_assertions::const_assert_eq!(
    std::mem::offset_of!(HandleDisposition<'_>, rights),
    std::mem::offset_of!(sys::zx_handle_disposition_t, rights)
);
static_assertions::const_assert_eq!(
    std::mem::offset_of!(HandleDisposition<'_>, result),
    std::mem::offset_of!(sys::zx_handle_disposition_t, result)
);

impl<'a> HandleDisposition<'a> {
    #[inline]
    pub fn new(
        handle_op: HandleOp<'a>,
        object_type: ObjectType,
        rights: Rights,
        status: Status,
    ) -> Self {
        let (operation, handle) = match handle_op {
            HandleOp::Move(h) => (sys::ZX_HANDLE_OP_MOVE, h.into_raw()),
            HandleOp::Duplicate(h) => (sys::ZX_HANDLE_OP_DUPLICATE, h.raw_handle()),
        };

        Self {
            operation,
            handle,
            _handle_lifetime: std::marker::PhantomData,
            object_type,
            rights: rights,
            result: status,
        }
    }

    pub fn raw_handle(&self) -> sys::zx_handle_t {
        self.handle
    }

    pub fn is_move(&self) -> bool {
        self.operation == sys::ZX_HANDLE_OP_MOVE
    }

    pub fn is_duplicate(&self) -> bool {
        self.operation == sys::ZX_HANDLE_OP_DUPLICATE
    }

    pub fn take_op(&mut self) -> HandleOp<'a> {
        match self.operation {
            sys::ZX_HANDLE_OP_MOVE => {
                // SAFETY: this is guaranteed to be a valid handle number by a combination of this
                // type's public API and the kernel's guarantees.
                HandleOp::Move(unsafe {
                    Handle::from_raw(std::mem::replace(&mut self.handle, sys::ZX_HANDLE_INVALID))
                })
            }
            sys::ZX_HANDLE_OP_DUPLICATE => {
                // SAFETY: this is guaranteed to be a valid handle number by a combination of this
                // type's public API and the kernel's guarantees.
                HandleOp::Duplicate(unsafe { Unowned::from_raw_handle(self.handle) })
            }
            _ => unreachable!(),
        }
    }

    pub fn into_raw(mut self) -> sys::zx_handle_disposition_t {
        match self.take_op() {
            HandleOp::Move(mut handle) => sys::zx_handle_disposition_t {
                operation: sys::ZX_HANDLE_OP_MOVE,
                handle: std::mem::replace(&mut handle, Handle::invalid()).into_raw(),
                type_: self.object_type.0,
                rights: self.rights.bits(),
                result: self.result.into_raw(),
            },
            HandleOp::Duplicate(handle_ref) => sys::zx_handle_disposition_t {
                operation: sys::ZX_HANDLE_OP_DUPLICATE,
                handle: handle_ref.raw_handle(),
                type_: self.object_type.0,
                rights: self.rights.bits(),
                result: self.result.into_raw(),
            },
        }
    }
}

impl<'a> Drop for HandleDisposition<'a> {
    fn drop(&mut self) {
        // Ensure we clean up owned handle variants.
        if self.operation == sys::ZX_HANDLE_OP_MOVE {
            unsafe { drop(Handle::from_raw(self.handle)) };
        }
    }
}

/// Information on handles that were read.
///
/// ABI-compatible with zx_handle_info_t.
#[derive(Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
#[repr(C)]
pub struct HandleInfo {
    pub handle: Handle,
    pub object_type: ObjectType,
    pub rights: Rights,

    // Necessary for ABI compatibility with zx_handle_info_t.
    pub(crate) _unused: u32,
}

static_assertions::assert_eq_size!(HandleInfo, sys::zx_handle_info_t);
static_assertions::const_assert_eq!(
    std::mem::offset_of!(HandleInfo, handle),
    std::mem::offset_of!(sys::zx_handle_info_t, handle)
);
static_assertions::const_assert_eq!(
    std::mem::offset_of!(HandleInfo, object_type),
    std::mem::offset_of!(sys::zx_handle_info_t, ty)
);
static_assertions::const_assert_eq!(
    std::mem::offset_of!(HandleInfo, rights),
    std::mem::offset_of!(sys::zx_handle_info_t, rights)
);
static_assertions::const_assert_eq!(
    std::mem::offset_of!(HandleInfo, _unused),
    std::mem::offset_of!(sys::zx_handle_info_t, unused)
);

impl HandleInfo {
    /// Make a new `HandleInfo`.
    pub const fn new(handle: Handle, object_type: ObjectType, rights: Rights) -> Self {
        Self { handle, object_type, rights, _unused: 0 }
    }

    /// # Safety
    ///
    /// See [`Handle::from_raw`] for requirements about the validity and closing
    /// of `raw.handle`.
    ///
    /// Note that while `raw.ty` _should_ correspond to the type of the handle,
    /// that this is not required for safety.
    pub const unsafe fn from_raw(raw: sys::zx_handle_info_t) -> HandleInfo {
        HandleInfo::new(
            // SAFETY: invariants to not double-close are upheld by the caller.
            unsafe { Handle::from_raw(raw.handle) },
            ObjectType(raw.ty),
            Rights::from_bits_retain(raw.rights),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // The unit tests are built with a different crate name, but fuchsia_runtime returns a "real"
    // zx::Vmar that we need to use.
    use zx::{
        AsHandleRef, Channel, Handle, HandleBased, HandleDisposition, HandleInfo, HandleOp, Name,
        ObjectType, Rights, Vmo,
    };
    use zx_sys as sys;

    #[test]
    fn into_raw() {
        let vmo = Vmo::create(1).unwrap();
        let h = vmo.into_raw();
        let vmo2 = Vmo::from(unsafe { Handle::from_raw(h) });
        assert!(vmo2.write(b"1", 0).is_ok());
    }

    /// Test duplication by means of a VMO
    #[test]
    fn duplicate() {
        let hello_length: usize = 5;

        // Create a VMO and write some data to it.
        let vmo = Vmo::create(hello_length as u64).unwrap();
        assert!(vmo.write(b"hello", 0).is_ok());

        // Replace, reducing rights to read.
        let readonly_vmo = vmo.duplicate_handle(Rights::READ).unwrap();
        // Make sure we can read but not write.
        let mut read_vec = vec![0; hello_length];
        assert!(readonly_vmo.read(&mut read_vec, 0).is_ok());
        assert_eq!(read_vec, b"hello");
        assert_eq!(readonly_vmo.write(b"", 0), Err(Status::ACCESS_DENIED));

        // Write new data to the original handle, and read it from the new handle
        assert!(vmo.write(b"bye", 0).is_ok());
        assert!(readonly_vmo.read(&mut read_vec, 0).is_ok());
        assert_eq!(read_vec, b"byelo");
    }

    // Test replace by means of a VMO
    #[test]
    fn replace() {
        let hello_length: usize = 5;

        // Create a VMO and write some data to it.
        let vmo = Vmo::create(hello_length as u64).unwrap();
        assert!(vmo.write(b"hello", 0).is_ok());

        // Replace, reducing rights to read.
        let readonly_vmo = vmo.replace_handle(Rights::READ).unwrap();
        // Make sure we can read but not write.
        let mut read_vec = vec![0; hello_length];
        assert!(readonly_vmo.read(&mut read_vec, 0).is_ok());
        assert_eq!(read_vec, b"hello");
        assert_eq!(readonly_vmo.write(b"", 0), Err(Status::ACCESS_DENIED));
    }

    #[test]
    fn set_get_name() {
        // We need some concrete object to exercise the AsHandleRef<'_> set/get_name functions.
        let vmo = Vmo::create(1).unwrap();
        let short_name = Name::new("v").unwrap();
        assert!(vmo.set_name(&short_name).is_ok());
        assert_eq!(vmo.get_name().unwrap(), short_name);
    }

    #[test]
    fn set_get_max_len_name() {
        let vmo = Vmo::create(1).unwrap();
        let max_len_name = Name::new("a_great_maximum_length_vmo_name").unwrap(); // 31 bytes
        assert!(vmo.set_name(&max_len_name).is_ok());
        assert_eq!(vmo.get_name().unwrap(), max_len_name);
    }

    #[test]
    fn basic_info_channel() {
        let (side1, side2) = Channel::create();
        let info1 = side1.basic_info().expect("side1 basic_info failed");
        let info2 = side2.basic_info().expect("side2 basic_info failed");

        assert_eq!(info1.koid, info2.related_koid);
        assert_eq!(info2.koid, info1.related_koid);

        for info in &[info1, info2] {
            assert!(info.koid.raw_koid() >= sys::ZX_KOID_FIRST);
            assert_eq!(info.object_type, ObjectType::CHANNEL);
            assert!(info.rights.contains(Rights::READ | Rights::WRITE | Rights::WAIT));
        }

        let side1_repl = side1.replace_handle(Rights::READ).expect("side1 replace_handle failed");
        let info1_repl = side1_repl.basic_info().expect("side1_repl basic_info failed");
        assert_eq!(info1_repl.koid, info1.koid);
        assert_eq!(info1_repl.rights, Rights::READ);
    }

    #[test]
    fn basic_info_vmar() {
        // VMARs aren't waitable.
        let root_vmar = fuchsia_runtime::vmar_root_self();
        let info = root_vmar.basic_info().expect("vmar basic_info failed");
        assert_eq!(info.object_type, ObjectType::VMAR);
        assert!(!info.rights.contains(Rights::WAIT));
    }

    #[test]
    fn count_info() {
        let vmo0 = Vmo::create(1).unwrap();
        let count_info = vmo0.count_info().expect("vmo0 count_info failed");
        assert_eq!(count_info.handle_count, 1);

        let vmo1 = vmo0.duplicate_handle(Rights::SAME_RIGHTS).expect("vmo duplicate_handle failed");
        let count_info = vmo1.count_info().expect("vmo1 count_info failed");
        assert_eq!(count_info.handle_count, 2);
    }

    #[test]
    fn raw_handle_disposition() {
        const RAW_HANDLE: sys::zx_handle_t = 1;
        let hd = HandleDisposition::new(
            HandleOp::Move(unsafe { Handle::from_raw(RAW_HANDLE) }),
            ObjectType::VMO,
            Rights::EXECUTE,
            Status::OK,
        );
        let raw_hd = hd.into_raw();
        assert_eq!(raw_hd.operation, sys::ZX_HANDLE_OP_MOVE);
        assert_eq!(raw_hd.handle, RAW_HANDLE);
        assert_eq!(raw_hd.rights, sys::ZX_RIGHT_EXECUTE);
        assert_eq!(raw_hd.type_, sys::ZX_OBJ_TYPE_VMO);
        assert_eq!(raw_hd.result, sys::ZX_OK);
    }

    #[test]
    fn handle_info_from_raw() {
        const RAW_HANDLE: sys::zx_handle_t = 1;
        let raw_hi = sys::zx_handle_info_t {
            handle: RAW_HANDLE,
            ty: sys::ZX_OBJ_TYPE_VMO,
            rights: sys::ZX_RIGHT_EXECUTE,
            unused: 128,
        };
        let hi = unsafe { HandleInfo::from_raw(raw_hi) };
        assert_eq!(hi.handle.into_raw(), RAW_HANDLE);
        assert_eq!(hi.object_type, ObjectType::VMO);
        assert_eq!(hi.rights, Rights::EXECUTE);
    }

    #[test]
    fn basic_peer_closed() {
        let (lhs, rhs) = crate::EventPair::create();
        assert!(!lhs.is_closed().unwrap());
        assert!(!rhs.is_closed().unwrap());
        drop(rhs);
        assert!(lhs.is_closed().unwrap());
    }
}
