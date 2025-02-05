// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Common utilities used by both directory and file traits.

use crate::node::Node;
use fidl::endpoints::ServerEnd;
use fidl::prelude::*;
use fidl_fuchsia_io as fio;
use futures::StreamExt as _;
use std::sync::Arc;
use zx_status::Status;

pub use vfs_macros::attribute_query;

/// Set of known rights.
const FS_RIGHTS: fio::OpenFlags = fio::OPEN_RIGHTS;

/// Returns true if the rights flags in `flags` do not exceed those in `parent_flags`.
pub fn stricter_or_same_rights(parent_flags: fio::OpenFlags, flags: fio::OpenFlags) -> bool {
    let parent_rights = parent_flags & FS_RIGHTS;
    let rights = flags & FS_RIGHTS;
    return !rights.intersects(!parent_rights);
}

/// Common logic for rights processing during cloning a node, shared by both file and directory
/// implementations.
pub fn inherit_rights_for_clone(
    parent_flags: fio::OpenFlags,
    mut flags: fio::OpenFlags,
) -> Result<fio::OpenFlags, Status> {
    if flags.intersects(fio::OpenFlags::CLONE_SAME_RIGHTS) && flags.intersects(FS_RIGHTS) {
        return Err(Status::INVALID_ARGS);
    }

    // We preserve OPEN_FLAG_APPEND as this is what is the most convenient for the POSIX emulation.
    //
    // OPEN_FLAG_NODE_REFERENCE is enforced, according to our current FS permissions design.
    flags |= parent_flags & (fio::OpenFlags::APPEND | fio::OpenFlags::NODE_REFERENCE);

    // If CLONE_FLAG_SAME_RIGHTS is requested, cloned connection will inherit the same rights
    // as those from the originating connection.  We have ensured that no FS_RIGHTS flags are set
    // above.
    if flags.intersects(fio::OpenFlags::CLONE_SAME_RIGHTS) {
        flags &= !fio::OpenFlags::CLONE_SAME_RIGHTS;
        flags |= parent_flags & FS_RIGHTS;
    }

    if !stricter_or_same_rights(parent_flags, flags) {
        return Err(Status::ACCESS_DENIED);
    }

    // Ignore the POSIX flags for clone.
    flags &= !(fio::OpenFlags::POSIX_WRITABLE | fio::OpenFlags::POSIX_EXECUTABLE);

    Ok(flags)
}

/// A helper method to send OnOpen event on the handle owned by the `server_end` in case `flags`
/// contains `OPEN_FLAG_STATUS`.
///
/// If the send operation fails for any reason, the error is ignored.  This helper is used during
/// an Open() or a Clone() FIDL methods, and these methods have no means to propagate errors to the
/// caller.  OnOpen event is the only way to do that, so there is nowhere to report errors in
/// OnOpen dispatch.  `server_end` will be closed, so there will be some kind of indication of the
/// issue.
///
/// # Panics
/// If `status` is `Status::OK`.  In this case `OnOpen` may need to contain a description of the
/// object, and server_end should not be dropped.
pub fn send_on_open_with_error(
    describe: bool,
    server_end: ServerEnd<fio::NodeMarker>,
    status: Status,
) {
    if status == Status::OK {
        panic!("send_on_open_with_error() should not be used to respond with Status::OK");
    }

    if !describe {
        // There is no reasonable way to report this error.  Assuming the `server_end` has just
        // disconnected or failed in some other way why we are trying to send OnOpen.
        let _ = server_end.close_with_epitaph(status);
        return;
    }

    let (_, control_handle) = server_end.into_stream_and_control_handle();
    // Same as above, ignore the error.
    let _ = control_handle.send_on_open_(status.into_raw(), None);
    control_handle.shutdown_with_epitaph(status);
}

/// Trait to be used as a supertrait when an object should allow dynamic casting to an Any.
///
/// Separate trait since [`into_any`] requires Self to be Sized, which cannot be satisfied in a
/// trait without preventing it from being object safe (thus disallowing dynamic dispatch).
/// Since we provide a generic implementation, the size of each concrete type is known.
pub trait IntoAny: std::any::Any {
    /// Cast the given object into a `dyn std::any::Any`.
    fn into_any(self: Arc<Self>) -> Arc<dyn std::any::Any + Send + Sync + 'static>;
}

impl<T: 'static + Send + Sync> IntoAny for T {
    fn into_any(self: Arc<Self>) -> Arc<dyn std::any::Any + Send + Sync + 'static> {
        self as Arc<dyn std::any::Any + Send + Sync + 'static>
    }
}

/// Returns equivalent POSIX mode/permission bits based on the specified rights.
/// Note that these only set the user bits.
// TODO(https://fxbug.dev/324112547): Remove this function or make it only visible to this crate.
pub fn rights_to_posix_mode_bits(readable: bool, writable: bool, executable: bool) -> u32 {
    return (if readable { libc::S_IRUSR } else { 0 }
        | if writable { libc::S_IWUSR } else { 0 }
        | if executable { libc::S_IXUSR } else { 0 })
    .into();
}

pub async fn extended_attributes_sender(
    iterator: ServerEnd<fio::ExtendedAttributeIteratorMarker>,
    attributes: Vec<Vec<u8>>,
) {
    let mut stream = iterator.into_stream();

    let mut chunks = attributes.chunks(fio::MAX_LIST_ATTRIBUTES_CHUNK as usize).peekable();

    while let Some(Ok(fio::ExtendedAttributeIteratorRequest::GetNext { responder })) =
        stream.next().await
    {
        let (chunk, last) = match chunks.next() {
            Some(chunk) => (chunk, chunks.peek().is_none()),
            None => (&[][..], true),
        };
        responder.send(Ok((chunk, last))).unwrap_or_else(|error| {
            log::error!(error:?; "list extended attributes failed to send a chunk");
        });
        if last {
            break;
        }
    }
}

pub fn encode_extended_attribute_value(
    value: Vec<u8>,
) -> Result<fio::ExtendedAttributeValue, Status> {
    let size = value.len() as u64;
    if size > fio::MAX_INLINE_ATTRIBUTE_VALUE {
        #[cfg(target_os = "fuchsia")]
        {
            let vmo = fidl::Vmo::create(size)?;
            vmo.write(&value, 0)?;
            Ok(fio::ExtendedAttributeValue::Buffer(vmo))
        }
        #[cfg(not(target_os = "fuchsia"))]
        Err(Status::NOT_SUPPORTED)
    } else {
        Ok(fio::ExtendedAttributeValue::Bytes(value))
    }
}

pub fn decode_extended_attribute_value(
    value: fio::ExtendedAttributeValue,
) -> Result<Vec<u8>, Status> {
    match value {
        fio::ExtendedAttributeValue::Bytes(val) => Ok(val),
        #[cfg(target_os = "fuchsia")]
        fio::ExtendedAttributeValue::Buffer(vmo) => {
            let length = vmo.get_content_size()?;
            vmo.read_to_vec(0, length)
        }
        #[cfg(not(target_os = "fuchsia"))]
        fio::ExtendedAttributeValue::Buffer(_) => Err(Status::NOT_SUPPORTED),
        fio::ExtendedAttributeValue::__SourceBreaking { .. } => Err(Status::NOT_SUPPORTED),
    }
}

/// Helper for building [`fio::NodeAttributes2`]` given `requested` attributes. Code will only run
/// for `requested` attributes.
///
/// Example:
///
///   attributes!(
///       requested,
///       Mutable { creation_time: 123, modification_time: 456 },
///       Immutable { content_size: 789 }
///   );
///
#[macro_export]
macro_rules! attributes {
    ($requested:expr,
     Mutable {$($mut_a:ident: $mut_v:expr),* $(,)?},
     Immutable {$($immut_a:ident: $immut_v:expr),* $(,)?}) => (
        {
            use $crate::common::attribute_query;
            fio::NodeAttributes2 {
                mutable_attributes: fio::MutableNodeAttributes {
                    $($mut_a: if $requested.contains(attribute_query!($mut_a)) {
                        Option::from($mut_v)
                    } else {
                        None
                    }),*,
                    ..Default::default()
                },
                immutable_attributes: fio::ImmutableNodeAttributes {
                    $($immut_a: if $requested.contains(attribute_query!($immut_a)) {
                        Option::from($immut_v)
                    } else {
                        None
                    }),*,
                    ..Default::default()
                }
            }
        }
    )
}

/// Helper for building [`fio::NodeAttributes2`]` given immutable attributes in `requested`
/// Code will only run for `requested` attributes. Mutable attributes in `requested` are ignored.
///
/// Example:
///
///   immutable_attributes!(
///       requested,
///       Immutable { content_size: 789 }
///   );
///
#[macro_export]
macro_rules! immutable_attributes {
    ($requested:expr,
     Immutable {$($immut_a:ident: $immut_v:expr),* $(,)?}) => (
        {
            use $crate::common::attribute_query;
            fio::NodeAttributes2 {
                mutable_attributes: Default::default(),
                immutable_attributes: fio::ImmutableNodeAttributes {
                    $($immut_a: if $requested.contains(attribute_query!($immut_a)) {
                        Option::from($immut_v)
                    } else {
                        None
                    }),*,
                    ..Default::default()
                },
            }
        }
    )
}

/// Represents if and how objects should be created with an open request.
#[derive(PartialEq, Eq)]
pub enum CreationMode {
    Never,
    AllowExisting,
    Always,
    UnnamedTemporary,
    UnlinkableUnnamedTemporary,
}

/// Used to translate fuchsia.io/Node.SetAttr calls (io1) to fuchsia.io/Node.UpdateAttributes (io2).
pub(crate) fn io1_to_io2_attrs(
    flags: fio::NodeAttributeFlags,
    attrs: fio::NodeAttributes,
) -> fio::MutableNodeAttributes {
    fio::MutableNodeAttributes {
        creation_time: flags
            .contains(fio::NodeAttributeFlags::CREATION_TIME)
            .then_some(attrs.creation_time),
        modification_time: flags
            .contains(fio::NodeAttributeFlags::MODIFICATION_TIME)
            .then_some(attrs.modification_time),
        ..Default::default()
    }
}

/// The set of attributes that must be queried to fulfill an io1 GetAttrs request.
const ALL_IO1_ATTRIBUTES: fio::NodeAttributesQuery = fio::NodeAttributesQuery::PROTOCOLS
    .union(fio::NodeAttributesQuery::ABILITIES)
    .union(fio::NodeAttributesQuery::ID)
    .union(fio::NodeAttributesQuery::CONTENT_SIZE)
    .union(fio::NodeAttributesQuery::STORAGE_SIZE)
    .union(fio::NodeAttributesQuery::LINK_COUNT)
    .union(fio::NodeAttributesQuery::CREATION_TIME)
    .union(fio::NodeAttributesQuery::MODIFICATION_TIME);

/// Default set of attributes to send to an io1 GetAttr request upon failure.
const DEFAULT_IO1_ATTRIBUTES: fio::NodeAttributes = fio::NodeAttributes {
    mode: 0,
    id: fio::INO_UNKNOWN,
    content_size: 0,
    storage_size: 0,
    link_count: 0,
    creation_time: 0,
    modification_time: 0,
};

const DEFAULT_LINK_COUNT: u64 = 1;

/// Approximate a set of POSIX mode bits based on a node's protocols and abilities. This follows the
/// C++ VFS implementation, and is only used for io1 GetAttrs calls where the filesystem doesn't
/// support POSIX mode bits. Returns 0 if the mode bits could not be approximated.
const fn approximate_posix_mode(
    protocols: Option<fio::NodeProtocolKinds>,
    abilities: fio::Abilities,
) -> u32 {
    let Some(protocols) = protocols else {
        return 0;
    };
    match protocols {
        fio::NodeProtocolKinds::DIRECTORY => {
            let mut mode = libc::S_IFDIR;
            if abilities.contains(fio::Abilities::ENUMERATE) {
                mode |= libc::S_IRUSR;
            }
            if abilities.contains(fio::Abilities::MODIFY_DIRECTORY) {
                mode |= libc::S_IWUSR;
            }
            if abilities.contains(fio::Abilities::TRAVERSE) {
                mode |= libc::S_IXUSR;
            }
            mode
        }
        fio::NodeProtocolKinds::FILE => {
            let mut mode = libc::S_IFREG;
            if abilities.contains(fio::Abilities::READ_BYTES) {
                mode |= libc::S_IRUSR;
            }
            if abilities.contains(fio::Abilities::WRITE_BYTES) {
                mode |= libc::S_IWUSR;
            }
            if abilities.contains(fio::Abilities::EXECUTE) {
                mode |= libc::S_IXUSR;
            }
            mode
        }
        fio::NodeProtocolKinds::CONNECTOR => fio::MODE_TYPE_SERVICE | libc::S_IRUSR | libc::S_IWUSR,
        #[cfg(fuchsia_api_level_at_least = "HEAD")]
        fio::NodeProtocolKinds::SYMLINK => libc::S_IFLNK | libc::S_IRUSR,
        _ => 0,
    }
}

/// Used to translate fuchsia.io/Node.GetAttributes calls (io2) to fuchsia.io/Node.GetAttrs (io1).
/// We don't return a Result since the fuchsia.io/Node.GetAttrs method doesn't use FIDL errors, and
/// thus requires we return a status code and set of default attributes for the failure case.
pub async fn io2_to_io1_attrs<T: Node>(
    node: &T,
    rights: fio::Rights,
) -> (Status, fio::NodeAttributes) {
    if !rights.contains(fio::Rights::GET_ATTRIBUTES) {
        return (Status::BAD_HANDLE, DEFAULT_IO1_ATTRIBUTES);
    }

    let attributes = node.get_attributes(ALL_IO1_ATTRIBUTES).await;
    let Ok(fio::NodeAttributes2 {
        mutable_attributes: mut_attrs,
        immutable_attributes: immut_attrs,
    }) = attributes
    else {
        return (attributes.unwrap_err(), DEFAULT_IO1_ATTRIBUTES);
    };

    (
        Status::OK,
        fio::NodeAttributes {
            // If the node has POSIX mode bits, use those directly, otherwise synthesize a set based
            // on the node's protocols/abilities if available.
            mode: mut_attrs.mode.unwrap_or_else(|| {
                approximate_posix_mode(
                    immut_attrs.protocols,
                    immut_attrs.abilities.unwrap_or_default(),
                )
            }),
            id: immut_attrs.id.unwrap_or(fio::INO_UNKNOWN),
            content_size: immut_attrs.content_size.unwrap_or_default(),
            storage_size: immut_attrs.storage_size.unwrap_or_default(),
            link_count: immut_attrs.link_count.unwrap_or(DEFAULT_LINK_COUNT),
            creation_time: mut_attrs.creation_time.unwrap_or_default(),
            modification_time: mut_attrs.modification_time.unwrap_or_default(),
        },
    )
}

pub fn mutable_node_attributes_to_query(
    attributes: &fio::MutableNodeAttributes,
) -> fio::NodeAttributesQuery {
    let mut query = fio::NodeAttributesQuery::empty();

    if attributes.creation_time.is_some() {
        query |= fio::NodeAttributesQuery::CREATION_TIME;
    }
    if attributes.modification_time.is_some() {
        query |= fio::NodeAttributesQuery::MODIFICATION_TIME;
    }
    if attributes.access_time.is_some() {
        query |= fio::NodeAttributesQuery::ACCESS_TIME;
    }
    if attributes.mode.is_some() {
        query |= fio::NodeAttributesQuery::MODE;
    }
    if attributes.uid.is_some() {
        query |= fio::NodeAttributesQuery::UID;
    }
    if attributes.gid.is_some() {
        query |= fio::NodeAttributesQuery::GID;
    }
    if attributes.rdev.is_some() {
        query |= fio::NodeAttributesQuery::RDEV;
    }
    query
}

#[cfg(test)]
mod tests {
    use super::inherit_rights_for_clone;

    use fidl_fuchsia_io as fio;

    // TODO This should be converted into a function as soon as backtrace support is in place.
    // The only reason this is a macro is to generate error messages that point to the test
    // function source location in their top stack frame.
    macro_rules! irfc_ok {
        ($parent_flags:expr, $flags:expr, $expected_new_flags:expr $(,)*) => {{
            let res = inherit_rights_for_clone($parent_flags, $flags);
            match res {
                Ok(new_flags) => assert_eq!(
                    $expected_new_flags, new_flags,
                    "`inherit_rights_for_clone` returned unexpected set of flags.\n\
                     Expected: {:X}\n\
                     Actual: {:X}",
                    $expected_new_flags, new_flags
                ),
                Err(status) => panic!("`inherit_rights_for_clone` failed.  Status: {status}"),
            }
        }};
    }

    #[test]
    fn node_reference_is_inherited() {
        irfc_ok!(
            fio::OpenFlags::NODE_REFERENCE,
            fio::OpenFlags::empty(),
            fio::OpenFlags::NODE_REFERENCE
        );
    }
}
