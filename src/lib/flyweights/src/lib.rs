// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Types implementing the [flyweight pattern](https://en.wikipedia.org/wiki/Flyweight_pattern)
//! for reusing object allocations.

#![warn(missing_docs)]

use ahash::AHashSet;
use bstr::{BStr, BString};
use serde::de::{Deserializer, Visitor};
use serde::ser::Serializer;
use serde::{Deserialize, Serialize};
use std::borrow::Borrow;
use std::fmt::{Debug, Display, Formatter, Result as FmtResult};
use std::hash::{Hash, Hasher};
use std::ops::Deref;
use std::ptr::NonNull;
use std::sync::{Arc, Mutex};

/// The global string cache for `FlyStr`.
///
/// If a live `FlyStr` contains an `Arc<Box<[u8]>>`, the `Arc<Box<[u8]>>` must also be in this cache
/// and it must have a refcount of >= 2.
static CACHE: std::sync::LazyLock<Mutex<AHashSet<Storage>>> =
    std::sync::LazyLock::new(|| Mutex::new(AHashSet::new()));

/// Wrapper type for stored `Arc`s that lets us query the cache without an owned value. Implementing
/// `Borrow<[u8]> for Arc<Box<[u8]>>` upstream *might* be possible with specialization but this is
/// easy enough.
#[derive(Eq, Hash, PartialEq)]
struct Storage(Arc<Box<[u8]>>);

impl Borrow<[u8]> for Storage {
    #[inline]
    fn borrow(&self) -> &[u8] {
        self.0.as_ref()
    }
}

/// An immutable string type which only stores a single copy of each string allocated. Internally
/// represented as an `Arc` to the backing allocation. Occupies a single pointer width.
///
/// # Small strings
///
/// Very short strings are stored inline in the pointer with bit-tagging, so no allocations are
/// performed.
///
/// # Performance
///
/// It's slower to construct than a regular `String` but trades that for reduced standing memory
/// usage by deduplicating strings. `PartialEq` and `Hash` are implemented on the underlying pointer
/// value rather than the pointed-to data for faster equality comparisons and indexing, which is
/// sound by virtue of the type guaranteeing that only one `FlyStr` pointer value will exist at
/// any time for a given string's contents.
///
/// As with any performance optimization, you should only use this type if you can measure the
/// benefit it provides to your program. Pay careful attention to creating `FlyStr`s in hot paths
/// as it may regress runtime performance.
///
/// # Allocation lifecycle
///
/// Intended for long-running system services with user-provided values, `FlyStr`s are removed from
/// the global cache when the last reference to them is dropped. While this incurs some overhead
/// it is important to prevent the value cache from becoming a denial-of-service vector.
#[derive(Clone, Eq, Hash, PartialEq)]
pub struct FlyStr(RawRepr);

static_assertions::assert_eq_size!(FlyStr, usize);

impl FlyStr {
    /// Create a `FlyStr`, allocating it in the cache if the value is not already cached.
    ///
    /// # Performance
    ///
    /// Creating an instance of this type requires accessing the global cache of strings, which
    /// involves taking a lock. When multiple threads are allocating lots of strings there may be
    /// contention.
    ///
    /// Each string allocated is hashed for lookup in the cache.
    pub fn new(s: impl AsRef<str> + Into<String>) -> Self {
        Self(RawRepr::new_str(s))
    }

    /// Returns the underlying string slice.
    #[inline]
    pub fn as_str(&self) -> &str {
        // SAFETY: Every FlyStr is constructed from valid UTF-8 bytes.
        unsafe { std::str::from_utf8_unchecked(self.0.as_bytes()) }
    }
}

impl Default for FlyStr {
    #[inline]
    fn default() -> Self {
        Self::new("")
    }
}

impl From<&'_ str> for FlyStr {
    #[inline]
    fn from(s: &str) -> Self {
        Self::new(s)
    }
}

impl From<&'_ String> for FlyStr {
    #[inline]
    fn from(s: &String) -> Self {
        Self::new(&**s)
    }
}

impl From<String> for FlyStr {
    #[inline]
    fn from(s: String) -> Self {
        Self::new(s)
    }
}

impl From<Box<str>> for FlyStr {
    #[inline]
    fn from(s: Box<str>) -> Self {
        Self::new(s)
    }
}

impl From<&Box<str>> for FlyStr {
    #[inline]
    fn from(s: &Box<str>) -> Self {
        Self::new(&**s)
    }
}

impl TryFrom<FlyByteStr> for FlyStr {
    type Error = std::str::Utf8Error;

    #[inline]
    fn try_from(b: FlyByteStr) -> Result<FlyStr, Self::Error> {
        // The internals of both FlyStr and FlyByteStr are the same, but it's only sound to return
        // a FlyStr if the RawRepr contains/points to valid UTF-8.
        std::str::from_utf8(b.as_bytes())?;
        Ok(FlyStr(b.0))
    }
}

impl Into<String> for FlyStr {
    #[inline]
    fn into(self) -> String {
        self.as_str().to_owned()
    }
}

impl Into<String> for &'_ FlyStr {
    #[inline]
    fn into(self) -> String {
        self.as_str().to_owned()
    }
}

impl Deref for FlyStr {
    type Target = str;

    #[inline]
    fn deref(&self) -> &Self::Target {
        self.as_str()
    }
}

impl AsRef<str> for FlyStr {
    #[inline]
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl PartialOrd for FlyStr {
    #[inline]
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for FlyStr {
    #[inline]
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.as_str().cmp(other.as_str())
    }
}

impl PartialEq<str> for FlyStr {
    #[inline]
    fn eq(&self, other: &str) -> bool {
        self.as_str() == other
    }
}

impl PartialEq<&'_ str> for FlyStr {
    #[inline]
    fn eq(&self, other: &&str) -> bool {
        self.as_str() == *other
    }
}

impl PartialEq<String> for FlyStr {
    #[inline]
    fn eq(&self, other: &String) -> bool {
        self.as_str() == &**other
    }
}

impl PartialEq<FlyByteStr> for FlyStr {
    #[inline]
    fn eq(&self, other: &FlyByteStr) -> bool {
        self.0 == other.0
    }
}

impl PartialEq<&'_ FlyByteStr> for FlyStr {
    #[inline]
    fn eq(&self, other: &&FlyByteStr) -> bool {
        self.0 == other.0
    }
}

impl PartialOrd<str> for FlyStr {
    #[inline]
    fn partial_cmp(&self, other: &str) -> Option<std::cmp::Ordering> {
        self.as_str().partial_cmp(other)
    }
}

impl PartialOrd<&str> for FlyStr {
    #[inline]
    fn partial_cmp(&self, other: &&str) -> Option<std::cmp::Ordering> {
        self.as_str().partial_cmp(*other)
    }
}

impl PartialOrd<FlyByteStr> for FlyStr {
    #[inline]
    fn partial_cmp(&self, other: &FlyByteStr) -> Option<std::cmp::Ordering> {
        BStr::new(self.as_str()).partial_cmp(other.as_bstr())
    }
}

impl PartialOrd<&'_ FlyByteStr> for FlyStr {
    #[inline]
    fn partial_cmp(&self, other: &&FlyByteStr) -> Option<std::cmp::Ordering> {
        BStr::new(self.as_str()).partial_cmp(other.as_bstr())
    }
}

impl Debug for FlyStr {
    fn fmt(&self, f: &mut Formatter<'_>) -> FmtResult {
        Debug::fmt(self.as_str(), f)
    }
}

impl Display for FlyStr {
    fn fmt(&self, f: &mut Formatter<'_>) -> FmtResult {
        Display::fmt(self.as_str(), f)
    }
}

impl Serialize for FlyStr {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'d> Deserialize<'d> for FlyStr {
    fn deserialize<D: Deserializer<'d>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_str(FlyStrVisitor)
    }
}

struct FlyStrVisitor;

impl Visitor<'_> for FlyStrVisitor {
    type Value = FlyStr;
    fn expecting(&self, formatter: &mut Formatter<'_>) -> FmtResult {
        formatter.write_str("a string")
    }

    fn visit_borrowed_str<'de, E>(self, v: &'de str) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        Ok(v.into())
    }

    fn visit_str<E>(self, v: &str) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        Ok(v.into())
    }

    fn visit_string<E>(self, v: String) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        Ok(v.into())
    }
}

macro_rules! new_raw_repr {
    ($borrowed_bytes:expr, $owned_bytes:expr) => {
        if $borrowed_bytes.len() <= MAX_INLINE_SIZE {
            RawRepr::new_inline($borrowed_bytes)
        } else {
            let mut cache = CACHE.lock().unwrap();
            if let Some(existing) = cache.get($borrowed_bytes) {
                RawRepr::from_storage(existing)
            } else {
                let (ret, for_cache) = RawRepr::new_for_storage($owned_bytes);
                cache.insert(for_cache);
                ret
            }
        }
    };
}

/// An immutable bytestring type which only stores a single copy of each string allocated.
/// Internally represented as an `Arc` to the backing allocation. Occupies a single pointer width.
///
/// # Small strings
///
/// Very short strings are stored inline in the pointer with bit-tagging, so no allocations are
/// performed.
///
/// # Performance
///
/// It's slower to construct than a regular `BString` but trades that for reduced standing memory
/// usage by deduplicating strings. `PartialEq` and `Hash` are implemented on the underlying pointer
/// value rather than the pointed-to data for faster equality comparisons and indexing, which is
/// sound by virtue of the type guaranteeing that only one `FlyByteStr` pointer value will exist at
/// any time for a given string's contents.
///
/// As with any performance optimization, you should only use this type if you can measure the
/// benefit it provides to your program. Pay careful attention to creating `FlyByteStr`s in hot
/// paths as it may regress runtime performance.
///
/// # Allocation lifecycle
///
/// Intended for long-running system services with user-provided values, `FlyByteStr`s are removed
/// from the global cache when the last reference to them is dropped. While this incurs some
/// overhead it is important to prevent the value cache from becoming a denial-of-service vector.
#[derive(Clone, Eq, Hash, PartialEq)]
pub struct FlyByteStr(RawRepr);

static_assertions::assert_eq_size!(FlyByteStr, usize);

impl FlyByteStr {
    /// Create a `FlyByteStr`, allocating it in the cache if the value is not already cached.
    ///
    /// # Performance
    ///
    /// Creating an instance of this type requires accessing the global cache of strings, which
    /// involves taking a lock. When multiple threads are allocating lots of strings there may be
    /// contention.
    ///
    /// Each string allocated is hashed for lookup in the cache.
    pub fn new(s: impl AsRef<[u8]> + Into<Vec<u8>>) -> Self {
        Self(RawRepr::new(s))
    }

    /// Returns the underlying bytestring slice.
    #[inline]
    pub fn as_bstr(&self) -> &BStr {
        BStr::new(self.0.as_bytes())
    }

    /// Returns the underlying byte slice.
    #[inline]
    pub fn as_bytes(&self) -> &[u8] {
        self.0.as_bytes()
    }
}

impl Default for FlyByteStr {
    #[inline]
    fn default() -> Self {
        Self::new(b"")
    }
}

impl From<&'_ [u8]> for FlyByteStr {
    #[inline]
    fn from(s: &[u8]) -> Self {
        Self::new(s)
    }
}

impl From<&'_ BStr> for FlyByteStr {
    #[inline]
    fn from(s: &BStr) -> Self {
        let bytes: &[u8] = s.as_ref();
        Self(new_raw_repr!(bytes, bytes.to_vec().into_boxed_slice()))
    }
}

impl From<&'_ str> for FlyByteStr {
    #[inline]
    fn from(s: &str) -> Self {
        Self::new(s)
    }
}

impl From<&'_ Vec<u8>> for FlyByteStr {
    #[inline]
    fn from(s: &Vec<u8>) -> Self {
        Self(new_raw_repr!(&s[..], s.clone().into_boxed_slice()))
    }
}

impl From<&'_ String> for FlyByteStr {
    #[inline]
    fn from(s: &String) -> Self {
        Self::new(&**s)
    }
}

impl From<Vec<u8>> for FlyByteStr {
    #[inline]
    fn from(s: Vec<u8>) -> Self {
        Self::new(s)
    }
}

impl From<String> for FlyByteStr {
    #[inline]
    fn from(s: String) -> Self {
        Self::new(s)
    }
}

impl From<Box<[u8]>> for FlyByteStr {
    #[inline]
    fn from(s: Box<[u8]>) -> Self {
        Self::new(s)
    }
}

impl From<Box<str>> for FlyByteStr {
    #[inline]
    fn from(s: Box<str>) -> Self {
        Self(new_raw_repr!(s.as_bytes(), s.into_boxed_bytes()))
    }
}

impl From<&'_ Box<[u8]>> for FlyByteStr {
    #[inline]
    fn from(s: &'_ Box<[u8]>) -> Self {
        Self(new_raw_repr!(&**s, s.clone()))
    }
}

impl From<&Box<str>> for FlyByteStr {
    #[inline]
    fn from(s: &Box<str>) -> Self {
        Self::new(&**s)
    }
}

impl Into<BString> for FlyByteStr {
    #[inline]
    fn into(self) -> BString {
        self.as_bstr().to_owned()
    }
}

impl Into<Vec<u8>> for FlyByteStr {
    #[inline]
    fn into(self) -> Vec<u8> {
        self.as_bytes().to_owned()
    }
}

impl From<FlyStr> for FlyByteStr {
    #[inline]
    fn from(s: FlyStr) -> FlyByteStr {
        Self(s.0)
    }
}

impl TryInto<String> for FlyByteStr {
    type Error = std::string::FromUtf8Error;

    #[inline]
    fn try_into(self) -> Result<String, Self::Error> {
        String::from_utf8(self.into())
    }
}

impl Deref for FlyByteStr {
    type Target = BStr;

    #[inline]
    fn deref(&self) -> &Self::Target {
        self.as_bstr()
    }
}

impl AsRef<BStr> for FlyByteStr {
    #[inline]
    fn as_ref(&self) -> &BStr {
        self.as_bstr()
    }
}

impl PartialOrd for FlyByteStr {
    #[inline]
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for FlyByteStr {
    #[inline]
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.as_bstr().cmp(other.as_bstr())
    }
}

impl PartialEq<[u8]> for FlyByteStr {
    #[inline]
    fn eq(&self, other: &[u8]) -> bool {
        self.as_bytes() == other
    }
}

impl PartialEq<BStr> for FlyByteStr {
    #[inline]
    fn eq(&self, other: &BStr) -> bool {
        self.as_bytes() == other
    }
}

impl PartialEq<str> for FlyByteStr {
    #[inline]
    fn eq(&self, other: &str) -> bool {
        self.as_bytes() == other.as_bytes()
    }
}

impl PartialEq<&'_ [u8]> for FlyByteStr {
    #[inline]
    fn eq(&self, other: &&[u8]) -> bool {
        self.as_bytes() == *other
    }
}

impl PartialEq<&'_ BStr> for FlyByteStr {
    #[inline]
    fn eq(&self, other: &&BStr) -> bool {
        self.as_bstr() == *other
    }
}

impl PartialEq<&'_ str> for FlyByteStr {
    #[inline]
    fn eq(&self, other: &&str) -> bool {
        self.as_bytes() == other.as_bytes()
    }
}

impl PartialEq<String> for FlyByteStr {
    #[inline]
    fn eq(&self, other: &String) -> bool {
        self.as_bytes() == other.as_bytes()
    }
}

impl PartialEq<FlyStr> for FlyByteStr {
    #[inline]
    fn eq(&self, other: &FlyStr) -> bool {
        self.0 == other.0
    }
}

impl PartialEq<&'_ FlyStr> for FlyByteStr {
    #[inline]
    fn eq(&self, other: &&FlyStr) -> bool {
        self.0 == other.0
    }
}

impl PartialOrd<str> for FlyByteStr {
    #[inline]
    fn partial_cmp(&self, other: &str) -> Option<std::cmp::Ordering> {
        self.as_bstr().partial_cmp(other)
    }
}

impl PartialOrd<&str> for FlyByteStr {
    #[inline]
    fn partial_cmp(&self, other: &&str) -> Option<std::cmp::Ordering> {
        self.as_bstr().partial_cmp(other)
    }
}

impl PartialOrd<FlyStr> for FlyByteStr {
    #[inline]
    fn partial_cmp(&self, other: &FlyStr) -> Option<std::cmp::Ordering> {
        self.as_bstr().partial_cmp(other.as_str())
    }
}

impl Debug for FlyByteStr {
    fn fmt(&self, f: &mut Formatter<'_>) -> FmtResult {
        Debug::fmt(self.as_bstr(), f)
    }
}

impl Display for FlyByteStr {
    fn fmt(&self, f: &mut Formatter<'_>) -> FmtResult {
        Display::fmt(self.as_bstr(), f)
    }
}

impl Serialize for FlyByteStr {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_bytes(self.as_bytes())
    }
}

impl<'d> Deserialize<'d> for FlyByteStr {
    fn deserialize<D: Deserializer<'d>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_bytes(FlyByteStrVisitor)
    }
}

struct FlyByteStrVisitor;

impl<'de> Visitor<'de> for FlyByteStrVisitor {
    type Value = FlyByteStr;
    fn expecting(&self, formatter: &mut Formatter<'_>) -> FmtResult {
        formatter.write_str("a string, a bytestring, or a sequence of bytes")
    }

    fn visit_str<E>(self, v: &str) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        Ok(FlyByteStr::from(v))
    }

    fn visit_string<E>(self, v: String) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        Ok(FlyByteStr::from(v))
    }

    fn visit_bytes<E>(self, v: &[u8]) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        Ok(FlyByteStr::from(v))
    }

    fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
    where
        A: serde::de::SeqAccess<'de>,
    {
        let mut bytes = vec![];
        while let Some(b) = seq.next_element::<u8>()? {
            bytes.push(b);
        }
        Ok(FlyByteStr::from(bytes))
    }
}

#[repr(C)] // Guarantee predictable field ordering.
union RawRepr {
    /// Strings longer than MAX_INLINE_SIZE are allocated as Arc<Box<[u8]>> which have a thin
    /// pointer representation. This means that `heap` variants of `Storage` will always have the
    /// pointer contents aligned and this variant will never have its least significant bit set.
    ///
    /// We store a `NonNull` so we can have guaranteed pointer layout.
    heap: NonNull<Box<[u8]>>,

    /// Strings shorter than or equal in length to MAX_INLINE_SIZE are stored in this union variant.
    /// The first byte is reserved for the size of the inline string, and the remaining bytes are
    /// used for the string itself. The first byte has its least significant bit set to 1 to
    /// distinguish inline strings from heap-allocated ones, and the size is stored in the remaining
    /// 7 bits.
    inline: InlineRepr,
}

// The inline variant should not cause us to occupy more space than the heap variant alone.
static_assertions::assert_eq_size!(Arc<Box<[u8]>>, RawRepr);

// Alignment of the Arc pointers must be >1 in order to have space for the mask bit at the bottom.
static_assertions::const_assert!(std::mem::align_of::<Box<[u8]>>() > 1);

// The short string optimization makes little-endian layout assumptions with the first byte being
// the least significant.
static_assertions::assert_type_eq_all!(byteorder::NativeEndian, byteorder::LittleEndian);

/// An enum with an actual discriminant that allows us to limit the reach of unsafe code in the
/// implementation without affecting the stored size of `RawRepr`.
enum SafeRepr<'a> {
    Heap(NonNull<Box<[u8]>>),
    Inline(&'a InlineRepr),
}

// SAFETY: FlyStr can be dropped from any thread.
unsafe impl Send for RawRepr {}
// SAFETY: FlyStr has an immutable public API.
unsafe impl Sync for RawRepr {}

impl RawRepr {
    fn new_str(s: impl AsRef<str> + Into<String>) -> Self {
        let borrowed = s.as_ref();
        new_raw_repr!(borrowed.as_bytes(), {
            let s: String = s.into();
            s.into_bytes().into_boxed_slice()
        })
    }

    fn new(s: impl AsRef<[u8]> + Into<Vec<u8>>) -> Self {
        let borrowed = s.as_ref();
        new_raw_repr!(borrowed, {
            let v: Vec<u8> = s.into();
            v.into_boxed_slice()
        })
    }

    #[inline]
    fn new_inline(s: &[u8]) -> Self {
        assert!(s.len() <= MAX_INLINE_SIZE);
        let new = Self { inline: InlineRepr::new(s) };
        assert!(new.is_inline(), "least significant bit must be 1 for inline strings");
        new
    }

    #[inline]
    fn from_storage(storage: &Storage) -> Self {
        Self { heap: nonnull_from_arc(Arc::clone(&storage.0)) }
    }

    #[inline]
    fn new_for_storage(bytes: Box<[u8]>) -> (Self, Storage) {
        assert!(bytes.len() > MAX_INLINE_SIZE);
        let new_storage = Arc::new(bytes);
        let for_cache = Storage(Arc::clone(&new_storage));
        let new = Self { heap: nonnull_from_arc(new_storage) };
        assert!(!new.is_inline(), "least significant bit must be 0 for heap strings");
        (new, for_cache)
    }

    #[inline]
    fn is_inline(&self) -> bool {
        // SAFETY: it is always OK to interpret a pointer as byte array as long as we don't expect
        // to retain provenance.
        (unsafe { self.inline.masked_len } & 1) == 1
    }

    #[inline]
    fn project(&self) -> SafeRepr<'_> {
        if self.is_inline() {
            // SAFETY: Just checked that this is the inline variant.
            SafeRepr::Inline(unsafe { &self.inline })
        } else {
            // SAFETY: Just checked that this is the heap variant.
            SafeRepr::Heap(unsafe { self.heap })
        }
    }

    #[inline]
    fn as_bytes(&self) -> &[u8] {
        match self.project() {
            // SAFETY: FlyStr owns the `Arc` stored as a NonNull, it is live as long as `FlyStr`.
            SafeRepr::Heap(ptr) => unsafe { &**ptr.as_ref() },
            SafeRepr::Inline(i) => i.as_bytes(),
        }
    }
}

impl PartialEq for RawRepr {
    #[inline]
    fn eq(&self, other: &Self) -> bool {
        // SAFETY: it is always OK to interpret a pointer as a byte array as long as we don't expect
        // to retain provenance.
        let lhs = unsafe { &self.inline };
        // SAFETY: it is always OK to interpret a pointer as a byte array as long as we don't expect
        // to retain provenance.
        let rhs = unsafe { &other.inline };
        lhs.eq(rhs)
    }
}
impl Eq for RawRepr {}

impl Hash for RawRepr {
    fn hash<H: Hasher>(&self, h: &mut H) {
        // SAFETY: it is always OK to interpret a pointer as a byte array as long as we don't expect
        // to retain provenance.
        let this = unsafe { &self.inline };
        this.hash(h);
    }
}

impl Clone for RawRepr {
    fn clone(&self) -> Self {
        match self.project() {
            SafeRepr::Heap(ptr) => {
                // SAFETY: We own this Arc, we know it's live because we are. The pointer came from
                // Arc::into_raw.
                let clone = unsafe { Arc::from_raw(ptr.as_ptr() as *const Box<[u8]>) };

                // Increment the count since we're not taking ownership of `self`.
                // SAFETY: This pointer came from `Arc::into_raw` and is still live.
                unsafe { Arc::increment_strong_count(ptr.as_ptr()) };

                Self { heap: nonnull_from_arc(clone) }
            }
            SafeRepr::Inline(&inline) => Self { inline },
        }
    }
}

impl Drop for RawRepr {
    fn drop(&mut self) {
        if !self.is_inline() {
            // Lock the cache before checking the count to ensure consistency.
            let mut cache = CACHE.lock().unwrap();

            // SAFETY: We checked above that this is the heap repr and this pointer was created from
            // an Arc in RawRepr::new.
            let heap = unsafe { Arc::from_raw(self.heap.as_ptr()) };

            // Check whether we're the last reference outside the cache, if so remove from cache.
            if Arc::strong_count(&heap) == 2 {
                assert!(cache.remove(&**heap), "cache must have a reference if refcount is 2");
            }
        }
    }
}

#[inline]
fn nonnull_from_arc(a: Arc<Box<[u8]>>) -> NonNull<Box<[u8]>> {
    let raw: *const Box<[u8]> = Arc::into_raw(a);
    // SAFETY: Arcs can't be null.
    unsafe { NonNull::new_unchecked(raw as *mut Box<[u8]>) }
}

#[derive(Clone, Copy, Hash, PartialEq)]
#[repr(C)] // Preserve field ordering.
struct InlineRepr {
    /// The first byte, which corresponds to the LSB of a pointer in the other variant.
    ///
    /// When the first bit is `1` the rest of this byte stores the length of the inline string.
    masked_len: u8,
    /// Inline string contents.
    contents: [u8; MAX_INLINE_SIZE],
}

/// We can store small strings up to 1 byte less than the size of the pointer to the heap-allocated
/// string.
const MAX_INLINE_SIZE: usize = std::mem::size_of::<NonNull<Box<[u8]>>>() - 1;

// Guard rail to make sure we never end up with an incorrect inline size encoding. Ensure that
// MAX_INLINE_SIZE is always smaller than the maximum size we can represent in a byte with the LSB
// reserved.
static_assertions::const_assert!((std::u8::MAX >> 1) as usize >= MAX_INLINE_SIZE);

impl InlineRepr {
    #[inline]
    fn new(s: &[u8]) -> Self {
        assert!(s.len() <= MAX_INLINE_SIZE);

        // Set the first byte to the length of the inline string with LSB masked to 1.
        let masked_len = ((s.len() as u8) << 1) | 1;

        let mut contents = [0u8; MAX_INLINE_SIZE];
        contents[..s.len()].copy_from_slice(s);

        Self { masked_len, contents }
    }

    #[inline]
    fn as_bytes(&self) -> &[u8] {
        let len = self.masked_len >> 1;
        &self.contents[..len as usize]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use static_assertions::{const_assert, const_assert_eq};
    use std::collections::BTreeSet;
    use test_case::test_case;

    // These tests all manipulate the process-global cache in the parent module. On target devices
    // we run each test case in its own process, so the test cases can't pollute each other. On
    // host, we run tests with a process for each suite (which is the Rust upstream default), and
    // we need to manually isolate the tests from each other.
    #[cfg(not(target_os = "fuchsia"))]
    use serial_test::serial;

    fn reset_global_cache() {
        // We still want subsequent tests to be able to run if one in the same process panics.
        match CACHE.lock() {
            Ok(mut c) => *c = AHashSet::new(),
            Err(e) => *e.into_inner() = AHashSet::new(),
        }
    }
    fn num_strings_in_global_cache() -> usize {
        CACHE.lock().unwrap().len()
    }

    impl RawRepr {
        fn refcount(&self) -> Option<usize> {
            match self.project() {
                SafeRepr::Heap(ptr) => {
                    let tmp = unsafe { Arc::from_raw(ptr.as_ptr() as *const Box<[u8]>) };
                    // tmp isn't taking ownership
                    unsafe { Arc::increment_strong_count(ptr.as_ptr()) };
                    // don't count tmp itself
                    let count = Arc::strong_count(&tmp) - 1;
                    Some(count)
                }
                SafeRepr::Inline(_) => None,
            }
        }
    }

    const SHORT_STRING: &str = "hello";
    const_assert!(SHORT_STRING.len() < MAX_INLINE_SIZE);

    const MAX_LEN_SHORT_STRING: &str = "hello!!";
    const_assert_eq!(MAX_LEN_SHORT_STRING.len(), MAX_INLINE_SIZE);

    const MIN_LEN_LONG_STRING: &str = "hello!!!";
    const_assert_eq!(MIN_LEN_LONG_STRING.len(), MAX_INLINE_SIZE + 1);

    const LONG_STRING: &str = "hello, world!!!!!!!!!!!!!!!!!!!!";
    const_assert!(LONG_STRING.len() > MAX_INLINE_SIZE);

    const SHORT_NON_UTF8: &[u8] = b"\xF0\x28\x8C\x28";
    const_assert!(SHORT_NON_UTF8.len() < MAX_INLINE_SIZE);

    const LONG_NON_UTF8: &[u8] = b"\xF0\x28\x8C\x28\xF0\x28\x8C\x28";
    const_assert!(LONG_NON_UTF8.len() > MAX_INLINE_SIZE);

    #[test_case("" ; "empty string")]
    #[test_case(SHORT_STRING ; "short strings")]
    #[test_case(MAX_LEN_SHORT_STRING ; "max len short strings")]
    #[test_case(MIN_LEN_LONG_STRING ; "barely long strings")]
    #[test_case(LONG_STRING ; "long strings")]
    #[cfg_attr(not(target_os = "fuchsia"), serial)]
    fn string_formatting_is_equivalent_to_str(original: &str) {
        reset_global_cache();

        let cached = FlyStr::new(original);
        assert_eq!(format!("{original}"), format!("{cached}"));
        assert_eq!(format!("{original:?}"), format!("{cached:?}"));

        let cached = FlyByteStr::new(original);
        assert_eq!(format!("{original}"), format!("{cached}"));
        assert_eq!(format!("{original:?}"), format!("{cached:?}"));
    }

    #[test_case("" ; "empty string")]
    #[test_case(SHORT_STRING ; "short strings")]
    #[test_case(MAX_LEN_SHORT_STRING ; "max len short strings")]
    #[test_case(MIN_LEN_LONG_STRING ; "barely long strings")]
    #[test_case(LONG_STRING ; "long strings")]
    #[cfg_attr(not(target_os = "fuchsia"), serial)]
    fn string_equality_works(contents: &str) {
        reset_global_cache();

        let cached = FlyStr::new(contents);
        let bytes_cached = FlyByteStr::new(contents);
        assert_eq!(cached, cached.clone(), "must be equal to itself");
        assert_eq!(cached, contents, "must be equal to the original");
        assert_eq!(cached, contents.to_owned(), "must be equal to an owned copy of the original");
        assert_eq!(cached, bytes_cached);

        // test inequality too
        assert_ne!(cached, "goodbye");
        assert_ne!(bytes_cached, "goodbye");
    }

    #[test_case("", SHORT_STRING ; "empty and short string")]
    #[test_case(SHORT_STRING, MAX_LEN_SHORT_STRING ; "two short strings")]
    #[test_case(MAX_LEN_SHORT_STRING, MIN_LEN_LONG_STRING ; "short and long strings")]
    #[test_case(MIN_LEN_LONG_STRING, LONG_STRING ; "barely long and long strings")]
    #[cfg_attr(not(target_os = "fuchsia"), serial)]
    fn string_comparison_works(lesser_contents: &str, greater_contents: &str) {
        reset_global_cache();

        let lesser = FlyStr::new(lesser_contents);
        let lesser_bytes = FlyByteStr::from(lesser_contents);
        let greater = FlyStr::new(greater_contents);
        let greater_bytes = FlyByteStr::from(greater_contents);

        // lesser as method receiver
        assert!(lesser < greater);
        assert!(lesser < greater_bytes);
        assert!(lesser_bytes < greater);
        assert!(lesser_bytes < greater_bytes);
        assert!(lesser <= greater);
        assert!(lesser <= greater_bytes);
        assert!(lesser_bytes <= greater);
        assert!(lesser_bytes <= greater_bytes);

        // greater as method receiver
        assert!(greater > lesser);
        assert!(greater > lesser_bytes);
        assert!(greater_bytes > lesser);
        assert!(greater >= lesser);
        assert!(greater >= lesser_bytes);
        assert!(greater_bytes >= lesser);
        assert!(greater_bytes >= lesser_bytes);
    }

    #[test_case("" ; "empty string")]
    #[test_case(SHORT_STRING ; "short strings")]
    #[test_case(MAX_LEN_SHORT_STRING ; "max len short strings")]
    #[cfg_attr(not(target_os = "fuchsia"), serial)]
    fn no_allocations_for_short_strings(contents: &str) {
        reset_global_cache();
        assert_eq!(num_strings_in_global_cache(), 0);

        let original = FlyStr::new(contents);
        assert_eq!(num_strings_in_global_cache(), 0);
        assert_eq!(original.0.refcount(), None);

        let cloned = original.clone();
        assert_eq!(num_strings_in_global_cache(), 0);
        assert_eq!(cloned.0.refcount(), None);

        let deduped = FlyStr::new(contents);
        assert_eq!(num_strings_in_global_cache(), 0);
        assert_eq!(deduped.0.refcount(), None);
    }

    #[test_case("" ; "empty string")]
    #[test_case(SHORT_STRING ; "short strings")]
    #[test_case(MAX_LEN_SHORT_STRING ; "max len short strings")]
    #[cfg_attr(not(target_os = "fuchsia"), serial)]
    fn no_allocations_for_short_bytestrings(contents: &str) {
        reset_global_cache();
        assert_eq!(num_strings_in_global_cache(), 0);

        let original = FlyByteStr::new(contents);
        assert_eq!(num_strings_in_global_cache(), 0);
        assert_eq!(original.0.refcount(), None);

        let cloned = original.clone();
        assert_eq!(num_strings_in_global_cache(), 0);
        assert_eq!(cloned.0.refcount(), None);

        let deduped = FlyByteStr::new(contents);
        assert_eq!(num_strings_in_global_cache(), 0);
        assert_eq!(deduped.0.refcount(), None);
    }

    #[test_case(MIN_LEN_LONG_STRING ; "barely long strings")]
    #[test_case(LONG_STRING ; "long strings")]
    #[cfg_attr(not(target_os = "fuchsia"), serial)]
    fn only_one_copy_allocated_for_long_strings(contents: &str) {
        reset_global_cache();

        assert_eq!(num_strings_in_global_cache(), 0);

        let original = FlyStr::new(contents);
        assert_eq!(num_strings_in_global_cache(), 1, "only one string allocated");
        assert_eq!(original.0.refcount(), Some(2), "one copy on stack, one in cache");

        let cloned = original.clone();
        assert_eq!(num_strings_in_global_cache(), 1, "cloning just incremented refcount");
        assert_eq!(cloned.0.refcount(), Some(3), "two copies on stack, one in cache");

        let deduped = FlyStr::new(contents);
        assert_eq!(num_strings_in_global_cache(), 1, "new string was deduped");
        assert_eq!(deduped.0.refcount(), Some(4), "three copies on stack, one in cache");
    }

    #[test_case(MIN_LEN_LONG_STRING ; "barely long strings")]
    #[test_case(LONG_STRING ; "long strings")]
    #[cfg_attr(not(target_os = "fuchsia"), serial)]
    fn only_one_copy_allocated_for_long_bytestrings(contents: &str) {
        reset_global_cache();

        assert_eq!(num_strings_in_global_cache(), 0);

        let original = FlyByteStr::new(contents);
        assert_eq!(num_strings_in_global_cache(), 1, "only one string allocated");
        assert_eq!(original.0.refcount(), Some(2), "one copy on stack, one in cache");

        let cloned = original.clone();
        assert_eq!(num_strings_in_global_cache(), 1, "cloning just incremented refcount");
        assert_eq!(cloned.0.refcount(), Some(3), "two copies on stack, one in cache");

        let deduped = FlyByteStr::new(contents);
        assert_eq!(num_strings_in_global_cache(), 1, "new string was deduped");
        assert_eq!(deduped.0.refcount(), Some(4), "three copies on stack, one in cache");
    }

    #[test]
    fn utf8_and_bytestrings_share_the_cache() {
        reset_global_cache();

        assert_eq!(num_strings_in_global_cache(), 0, "cache is empty");

        let _utf8 = FlyStr::from(MIN_LEN_LONG_STRING);
        assert_eq!(num_strings_in_global_cache(), 1, "string was allocated");

        let _bytes = FlyByteStr::from(MIN_LEN_LONG_STRING);
        assert_eq!(num_strings_in_global_cache(), 1, "bytestring was pulled from cache");
    }

    #[test_case(MIN_LEN_LONG_STRING ; "barely long strings")]
    #[test_case(LONG_STRING ; "long strings")]
    #[cfg_attr(not(target_os = "fuchsia"), serial)]
    fn cached_strings_dropped_when_refs_dropped(contents: &str) {
        reset_global_cache();

        let alloced = FlyStr::new(contents);
        assert_eq!(num_strings_in_global_cache(), 1, "only one string allocated");
        drop(alloced);
        assert_eq!(num_strings_in_global_cache(), 0, "last reference dropped");
    }

    #[test_case(MIN_LEN_LONG_STRING ; "barely long strings")]
    #[test_case(LONG_STRING ; "long strings")]
    #[cfg_attr(not(target_os = "fuchsia"), serial)]
    fn cached_bytestrings_dropped_when_refs_dropped(contents: &str) {
        reset_global_cache();

        let alloced = FlyByteStr::new(contents);
        assert_eq!(num_strings_in_global_cache(), 1, "only one string allocated");
        drop(alloced);
        assert_eq!(num_strings_in_global_cache(), 0, "last reference dropped");
    }

    #[test_case("", SHORT_STRING ; "empty and short string")]
    #[test_case(SHORT_STRING, MAX_LEN_SHORT_STRING ; "two short strings")]
    #[test_case(SHORT_STRING, LONG_STRING ; "short and long strings")]
    #[test_case(LONG_STRING, MAX_LEN_SHORT_STRING ; "long and max-len-short strings")]
    #[test_case(MIN_LEN_LONG_STRING, LONG_STRING ; "barely long and long strings")]
    #[cfg_attr(not(target_os = "fuchsia"), serial)]
    fn equality_and_hashing_with_pointer_value_works_correctly(first: &str, second: &str) {
        reset_global_cache();

        let first = FlyStr::new(first);
        let second = FlyStr::new(second);

        let mut set = AHashSet::new();
        set.insert(first.clone());
        assert!(set.contains(&first));
        assert!(!set.contains(&second));

        // re-insert the same cmstring
        set.insert(first);
        assert_eq!(set.len(), 1, "set did not grow because the same string was inserted as before");

        set.insert(second.clone());
        assert_eq!(set.len(), 2, "inserting a different string must mutate the set");
        assert!(set.contains(&second));

        // re-insert the second cmstring
        set.insert(second);
        assert_eq!(set.len(), 2);
    }

    #[test_case("", SHORT_STRING ; "empty and short string")]
    #[test_case(SHORT_STRING, MAX_LEN_SHORT_STRING ; "two short strings")]
    #[test_case(SHORT_STRING, LONG_STRING ; "short and long strings")]
    #[test_case(LONG_STRING, MAX_LEN_SHORT_STRING ; "long and max-len-short strings")]
    #[test_case(MIN_LEN_LONG_STRING, LONG_STRING ; "barely long and long strings")]
    #[cfg_attr(not(target_os = "fuchsia"), serial)]
    fn byte_equality_and_hashing_with_pointer_value_works_correctly(first: &str, second: &str) {
        reset_global_cache();

        let first = FlyByteStr::new(first);
        let second = FlyByteStr::new(second);

        let mut set = AHashSet::new();
        set.insert(first.clone());
        assert!(set.contains(&first));
        assert!(!set.contains(&second));

        // re-insert the same string
        set.insert(first);
        assert_eq!(set.len(), 1, "set did not grow because the same string was inserted as before");

        set.insert(second.clone());
        assert_eq!(set.len(), 2, "inserting a different string must mutate the set");
        assert!(set.contains(&second));

        // re-insert the second string
        set.insert(second);
        assert_eq!(set.len(), 2);
    }

    #[test_case("", SHORT_STRING ; "empty and short string")]
    #[test_case(SHORT_STRING, MAX_LEN_SHORT_STRING ; "two short strings")]
    #[test_case(SHORT_STRING, LONG_STRING ; "short and long strings")]
    #[test_case(LONG_STRING, MAX_LEN_SHORT_STRING ; "long and max-len-short strings")]
    #[test_case(MIN_LEN_LONG_STRING, LONG_STRING ; "barely long and long strings")]
    #[cfg_attr(not(target_os = "fuchsia"), serial)]
    fn comparison_for_btree_storage_works(first: &str, second: &str) {
        reset_global_cache();

        let first = FlyStr::new(first);
        let second = FlyStr::new(second);

        let mut set = BTreeSet::new();
        set.insert(first.clone());
        assert!(set.contains(&first));
        assert!(!set.contains(&second));

        // re-insert the same cmstring
        set.insert(first);
        assert_eq!(set.len(), 1, "set did not grow because the same string was inserted as before");

        set.insert(second.clone());
        assert_eq!(set.len(), 2, "inserting a different string must mutate the set");
        assert!(set.contains(&second));

        // re-insert the second cmstring
        set.insert(second);
        assert_eq!(set.len(), 2);
    }

    #[test_case("", SHORT_STRING ; "empty and short string")]
    #[test_case(SHORT_STRING, MAX_LEN_SHORT_STRING ; "two short strings")]
    #[test_case(SHORT_STRING, LONG_STRING ; "short and long strings")]
    #[test_case(LONG_STRING, MAX_LEN_SHORT_STRING ; "long and max-len-short strings")]
    #[test_case(MIN_LEN_LONG_STRING, LONG_STRING ; "barely long and long strings")]
    #[cfg_attr(not(target_os = "fuchsia"), serial)]
    fn byte_comparison_for_btree_storage_works(first: &str, second: &str) {
        reset_global_cache();

        let first = FlyByteStr::new(first);
        let second = FlyByteStr::new(second);

        let mut set = BTreeSet::new();
        set.insert(first.clone());
        assert!(set.contains(&first));
        assert!(!set.contains(&second));

        // re-insert the same string
        set.insert(first);
        assert_eq!(set.len(), 1, "set did not grow because the same string was inserted as before");

        set.insert(second.clone());
        assert_eq!(set.len(), 2, "inserting a different string must mutate the set");
        assert!(set.contains(&second));

        // re-insert the second string
        set.insert(second);
        assert_eq!(set.len(), 2);
    }

    #[test_case("" ; "empty string")]
    #[test_case(SHORT_STRING ; "short strings")]
    #[test_case(MAX_LEN_SHORT_STRING ; "max len short strings")]
    #[test_case(MIN_LEN_LONG_STRING ; "min len long strings")]
    #[test_case(LONG_STRING ; "long strings")]
    #[cfg_attr(not(target_os = "fuchsia"), serial)]
    fn serde_works(contents: &str) {
        let s = FlyStr::new(contents);
        let as_json = serde_json::to_string(&s).unwrap();
        assert_eq!(as_json, format!("\"{contents}\""));
        assert_eq!(s, serde_json::from_str::<FlyStr>(&as_json).unwrap());
    }

    #[test_case("" ; "empty string")]
    #[test_case(SHORT_STRING ; "short strings")]
    #[test_case(MAX_LEN_SHORT_STRING ; "max len short strings")]
    #[test_case(MIN_LEN_LONG_STRING ; "min len long strings")]
    #[test_case(LONG_STRING ; "long strings")]
    #[cfg_attr(not(target_os = "fuchsia"), serial)]
    fn serde_works_bytestring(contents: &str) {
        let s = FlyByteStr::new(contents);
        let as_json = serde_json::to_string(&s).unwrap();
        assert_eq!(s, serde_json::from_str::<FlyByteStr>(&as_json).unwrap());
    }

    #[test_case(SHORT_NON_UTF8 ; "short non-utf8 bytestring")]
    #[test_case(LONG_NON_UTF8 ; "long non-utf8 bytestring")]
    #[cfg_attr(not(target_os = "fuchsia"), serial)]
    fn non_utf8_works(contents: &[u8]) {
        let res: Result<FlyStr, _> = FlyByteStr::from(contents).try_into();
        res.unwrap_err();
    }

    #[test_case("" ; "empty string")]
    #[test_case(SHORT_STRING ; "short strings")]
    #[test_case(MAX_LEN_SHORT_STRING ; "max len short strings")]
    #[test_case(MIN_LEN_LONG_STRING ; "min len long strings")]
    #[test_case(LONG_STRING ; "long strings")]
    #[cfg_attr(not(target_os = "fuchsia"), serial)]
    fn flystr_to_flybytestr_and_back(contents: &str) {
        let bytestr = FlyByteStr::from(contents);
        let flystr = FlyStr::try_from(bytestr.clone()).unwrap();
        assert_eq!(bytestr, flystr);
        let bytestr2 = FlyByteStr::from(flystr.clone());
        assert_eq!(bytestr, bytestr2);
    }
}
