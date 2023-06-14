// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::{
        common::io2_conversions,
        directory::{entry::EntryInfo, DirectoryOptions},
        file::FileOptions,
        node::NodeOptions,
        symlink::SymlinkOptions,
    },
    fidl_fuchsia_io as fio, fuchsia_zircon as zx,
};

/// Extends fio::ConnectionProtocols and fio::OpenFlags
pub trait ProtocolsExt: Sync + 'static {
    /// True if the directory protocol is allowed.
    fn is_dir_allowed(&self) -> bool;

    /// True if the file protocol is allowed.
    fn is_file_allowed(&self) -> bool;

    /// True if the symlink protocol is allowed.
    fn is_symlink_allowed(&self) -> bool;

    /// True if any node protocol is allowed.
    fn is_any_node_protocol_allowed(&self) -> bool;

    /// The open mode for the connection.
    fn open_mode(&self) -> fio::OpenMode;

    /// The rights for the connection.  If None, it means the connection is not for a node based
    /// protocol.  If the connection is supposed to use the same rights as the parent connection,
    /// the rights should have been populated.
    fn rights(&self) -> Option<fio::Operations>;

    /// Convert to directory options.  Returns an error if the request does not permit a directory.
    fn to_directory_options(&self) -> Result<DirectoryOptions, zx::Status>;

    /// Convert to file options.  Returns an error if the request does not permit a file.
    fn to_file_options(&self) -> Result<FileOptions, zx::Status>;

    /// Convert to symlink options.  Returns an error if the request does not permit a symlink.
    fn to_symlink_options(&self) -> Result<SymlinkOptions, zx::Status>;

    /// Convert to node options.  Returns an error if the request does not permit a node.
    fn to_node_options(&self, entry_info: &EntryInfo) -> Result<NodeOptions, zx::Status>;

    /// True if REPRESENTATION is desired.
    fn get_representation(&self) -> bool;

    /// True if the file should be in append mode.
    fn is_append(&self) -> bool;

    /// True if the file should be truncated.
    fn is_truncate(&self) -> bool;

    /// The attributes requested in the REPRESENTATION event.
    fn attributes(&self) -> fio::NodeAttributesQuery;

    /// Attributes to set if an object is created.
    fn create_attributes(&self) -> Option<&fio::MutableNodeAttributes>;

    /// If creating an object, Whether to create a directory.
    fn create_directory(&self) -> bool;

    /// True if the protocol should be a limited node connection.
    fn is_node(&self) -> bool;
}

impl ProtocolsExt for fio::ConnectionProtocols {
    fn is_dir_allowed(&self) -> bool {
        matches!(
            self,
            fio::ConnectionProtocols::Node(fio::NodeOptions {
                protocols: Some(fio::NodeProtocols { directory: Some(_), .. }),
                ..
            })
        ) || self.is_any_node_protocol_allowed()
    }

    fn is_file_allowed(&self) -> bool {
        matches!(
            self,
            fio::ConnectionProtocols::Node(fio::NodeOptions {
                protocols: Some(fio::NodeProtocols { file: Some(_), .. }),
                ..
            })
        ) || self.is_any_node_protocol_allowed()
    }

    fn is_symlink_allowed(&self) -> bool {
        matches!(
            self,
            fio::ConnectionProtocols::Node(fio::NodeOptions {
                protocols: Some(fio::NodeProtocols { symlink: Some(_), .. }),
                ..
            })
        ) || self.is_any_node_protocol_allowed()
    }

    fn is_any_node_protocol_allowed(&self) -> bool {
        matches!(self, fio::ConnectionProtocols::Node(fio::NodeOptions { protocols: None, .. }))
    }

    fn open_mode(&self) -> fio::OpenMode {
        match self {
            fio::ConnectionProtocols::Node(fio::NodeOptions { mode: Some(mode), .. }) => *mode,
            _ => fio::OpenMode::OpenExisting,
        }
    }

    fn rights(&self) -> Option<fio::Operations> {
        match self {
            fio::ConnectionProtocols::Node(fio::NodeOptions { rights, .. }) => *rights,
            _ => None,
        }
    }

    fn to_directory_options(&self) -> Result<DirectoryOptions, zx::Status> {
        if !self.is_dir_allowed() {
            if self.is_file_allowed() && !self.is_symlink_allowed() {
                return Err(zx::Status::NOT_FILE);
            } else {
                return Err(zx::Status::WRONG_TYPE);
            }
        }
        let optional_rights = match self {
            fio::ConnectionProtocols::Node(fio::NodeOptions {
                protocols:
                    Some(fio::NodeProtocols {
                        directory:
                            Some(fio::DirectoryProtocolOptions {
                                optional_rights: Some(rights), ..
                            }),
                        ..
                    }),
                ..
            }) => *rights,
            _ => fio::Operations::empty(),
        };
        // If is_dir_allowed() returned true, there must be rights.
        Ok(DirectoryOptions { rights: self.rights().unwrap() | optional_rights })
    }

    fn to_file_options(&self) -> Result<FileOptions, zx::Status> {
        if !self.is_file_allowed() {
            if self.is_dir_allowed() && !self.is_symlink_allowed() {
                return Err(zx::Status::NOT_DIR);
            } else {
                return Err(zx::Status::WRONG_TYPE);
            }
        }
        Ok(FileOptions {
            // If is_file_allowed() returned true, there must be rights.
            rights: self.rights().unwrap(),
            is_append: self.is_append(),
        })
    }

    fn to_symlink_options(&self) -> Result<SymlinkOptions, zx::Status> {
        if !self.is_symlink_allowed() {
            return Err(zx::Status::WRONG_TYPE);
        }
        // If is_symlink_allowed() returned true, there must be rights.
        if !self.rights().unwrap().contains(fio::Operations::GET_ATTRIBUTES) {
            return Err(zx::Status::INVALID_ARGS);
        }
        Ok(SymlinkOptions)
    }

    fn to_node_options(&self, entry_info: &EntryInfo) -> Result<NodeOptions, zx::Status> {
        let must_be_directory = match self {
            fio::ConnectionProtocols::Node(fio::NodeOptions {
                protocols: Some(fio::NodeProtocols { node: Some(flags), .. }),
                ..
            }) => flags.contains(fio::NodeProtocolFlags::MUST_BE_DIRECTORY),
            _ => false,
        };
        if must_be_directory && entry_info.type_() != fio::DirentType::Directory {
            Err(zx::Status::NOT_DIR)
        } else {
            Ok(NodeOptions { rights: self.rights().unwrap() & fio::Operations::GET_ATTRIBUTES })
        }
    }

    fn get_representation(&self) -> bool {
        matches!(self, fio::ConnectionProtocols::Node(
            fio::NodeOptions {
                    flags: Some(flags),
                ..
            }
        ) if flags.contains(fio::NodeFlags::GET_REPRESENTATION))
    }

    fn is_append(&self) -> bool {
        matches!(
            self,
            fio::ConnectionProtocols::Node(fio::NodeOptions {
                protocols: Some(fio::NodeProtocols { file: Some(flags), .. }),
                ..
            }) if flags.contains(fio::FileProtocolFlags::APPEND)
        )
    }

    fn is_truncate(&self) -> bool {
        matches!(
            self,
            fio::ConnectionProtocols::Node(fio::NodeOptions {
                protocols: Some(fio::NodeProtocols { file: Some(flags), .. }),
                ..
            }) if flags.contains(fio::FileProtocolFlags::TRUNCATE)
        )
    }

    fn attributes(&self) -> fio::NodeAttributesQuery {
        match self {
            fio::ConnectionProtocols::Node(fio::NodeOptions {
                attributes: Some(query), ..
            }) => *query,
            _ => fio::NodeAttributesQuery::empty(),
        }
    }

    fn create_attributes(&self) -> Option<&fio::MutableNodeAttributes> {
        match self {
            fio::ConnectionProtocols::Node(fio::NodeOptions { create_attributes: a, .. }) => {
                a.as_ref()
            }
            _ => None,
        }
    }

    fn create_directory(&self) -> bool {
        matches!(
            self,
            fio::ConnectionProtocols::Node(fio::NodeOptions {
                protocols: Some(fio::NodeProtocols { directory: Some(_), .. }),
                ..
            })
        )
    }

    fn is_node(&self) -> bool {
        matches!(
            self,
            fio::ConnectionProtocols::Node(fio::NodeOptions {
                protocols: Some(fio::NodeProtocols { node: Some(_), .. }),
                ..
            })
        )
    }
}

impl ProtocolsExt for fio::OpenFlags {
    fn is_dir_allowed(&self) -> bool {
        !self.contains(fio::OpenFlags::NOT_DIRECTORY)
    }

    fn is_file_allowed(&self) -> bool {
        !self.contains(fio::OpenFlags::DIRECTORY)
    }

    fn is_symlink_allowed(&self) -> bool {
        !self.contains(fio::OpenFlags::DIRECTORY)
    }

    fn is_any_node_protocol_allowed(&self) -> bool {
        !self.intersects(fio::OpenFlags::DIRECTORY | fio::OpenFlags::NOT_DIRECTORY)
    }

    fn open_mode(&self) -> fio::OpenMode {
        if self.contains(fio::OpenFlags::CREATE) {
            if self.contains(fio::OpenFlags::CREATE_IF_ABSENT) {
                fio::OpenMode::AlwaysCreate
            } else {
                fio::OpenMode::MaybeCreate
            }
        } else {
            fio::OpenMode::OpenExisting
        }
    }

    fn rights(&self) -> Option<fio::Operations> {
        if self.contains(fio::OpenFlags::CLONE_SAME_RIGHTS) {
            None
        } else {
            let mut rights = fio::Operations::GET_ATTRIBUTES | fio::Operations::CONNECT;
            if self.contains(fio::OpenFlags::RIGHT_READABLE) {
                rights |= fio::Operations::READ_BYTES | fio::R_STAR_DIR;
            }
            if self.contains(fio::OpenFlags::RIGHT_WRITABLE) {
                rights |= fio::Operations::WRITE_BYTES | fio::W_STAR_DIR;
            }
            if self.contains(fio::OpenFlags::RIGHT_EXECUTABLE) {
                rights |= fio::Operations::EXECUTE | fio::X_STAR_DIR;
            }
            Some(rights)
        }
    }

    /// Checks flags provided for a new directory connection.  Returns directory options (cleaning
    /// up some ambiguities) or an error, in case new new connection flags are not permitting the
    /// connection to be opened.
    ///
    /// Changing this function can be dangerous!  Flags operations may have security implications.
    fn to_directory_options(&self) -> Result<DirectoryOptions, zx::Status> {
        assert!(!self.intersects(fio::OpenFlags::NODE_REFERENCE));

        let mut flags = *self;

        if flags.intersects(fio::OpenFlags::DIRECTORY) {
            flags &= !fio::OpenFlags::DIRECTORY;
        }

        if flags.intersects(fio::OpenFlags::NOT_DIRECTORY) {
            return Err(zx::Status::NOT_FILE);
        }

        // Parent connection must check the POSIX flags in `check_child_connection_flags`, so if any
        // are still present, we expand their respective rights and remove any remaining flags.
        if flags.intersects(fio::OpenFlags::POSIX_EXECUTABLE) {
            flags |= fio::OpenFlags::RIGHT_EXECUTABLE;
        }
        if flags.intersects(fio::OpenFlags::POSIX_WRITABLE) {
            flags |= fio::OpenFlags::RIGHT_WRITABLE;
        }
        flags &= !(fio::OpenFlags::POSIX_WRITABLE | fio::OpenFlags::POSIX_EXECUTABLE);

        let allowed_flags = fio::OpenFlags::DESCRIBE
            | fio::OpenFlags::CREATE
            | fio::OpenFlags::CREATE_IF_ABSENT
            | fio::OpenFlags::DIRECTORY
            | fio::OpenFlags::RIGHT_READABLE
            | fio::OpenFlags::RIGHT_WRITABLE
            | fio::OpenFlags::RIGHT_EXECUTABLE;

        let prohibited_flags = fio::OpenFlags::APPEND | fio::OpenFlags::TRUNCATE;

        if flags.intersects(prohibited_flags) {
            return Err(zx::Status::INVALID_ARGS);
        }

        if flags.intersects(!allowed_flags) {
            return Err(zx::Status::NOT_SUPPORTED);
        }

        Ok(DirectoryOptions { rights: io2_conversions::io1_to_io2(flags) })
    }

    fn to_file_options(&self) -> Result<FileOptions, zx::Status> {
        assert!(!self.intersects(fio::OpenFlags::NODE_REFERENCE));

        if self.contains(fio::OpenFlags::DIRECTORY) {
            return Err(zx::Status::NOT_DIR);
        }

        // Verify allowed operations/flags this node supports.
        let flags_without_rights = self.difference(
            fio::OpenFlags::RIGHT_READABLE
                | fio::OpenFlags::RIGHT_WRITABLE
                | fio::OpenFlags::RIGHT_EXECUTABLE,
        );
        const ALLOWED_FLAGS: fio::OpenFlags = fio::OpenFlags::NODE_REFERENCE
            .union(fio::OpenFlags::DESCRIBE)
            .union(fio::OpenFlags::CREATE)
            .union(fio::OpenFlags::CREATE_IF_ABSENT)
            .union(fio::OpenFlags::APPEND)
            .union(fio::OpenFlags::TRUNCATE)
            .union(fio::OpenFlags::POSIX_WRITABLE)
            .union(fio::OpenFlags::POSIX_EXECUTABLE)
            .union(fio::OpenFlags::NOT_DIRECTORY);
        if flags_without_rights.intersects(!ALLOWED_FLAGS) {
            return Err(zx::Status::NOT_SUPPORTED);
        }

        // Disallow invalid flag combinations.
        let mut prohibited_flags = fio::OpenFlags::empty();
        if !self.intersects(fio::OpenFlags::RIGHT_WRITABLE) {
            prohibited_flags |= fio::OpenFlags::APPEND | fio::OpenFlags::TRUNCATE
        }
        if self.intersects(fio::OpenFlags::NODE_REFERENCE) {
            prohibited_flags |= !fio::OPEN_FLAGS_ALLOWED_WITH_NODE_REFERENCE;
        }
        if self.intersects(prohibited_flags) {
            return Err(zx::Status::INVALID_ARGS);
        }

        Ok(FileOptions {
            rights: {
                let mut rights = fio::Operations::GET_ATTRIBUTES;
                if self.contains(fio::OpenFlags::RIGHT_READABLE) {
                    rights |= fio::Operations::READ_BYTES;
                }
                if self.contains(fio::OpenFlags::RIGHT_WRITABLE) {
                    rights |= fio::Operations::WRITE_BYTES | fio::Operations::UPDATE_ATTRIBUTES;
                }
                if self.contains(fio::OpenFlags::RIGHT_EXECUTABLE) {
                    rights |= fio::Operations::EXECUTE;
                }
                rights
            },
            is_append: self.contains(fio::OpenFlags::APPEND),
        })
    }

    fn to_symlink_options(&self) -> Result<SymlinkOptions, zx::Status> {
        // TODO(fxbug.dev/123390): Support NODE_REFERENCE.

        if self.intersects(fio::OpenFlags::DIRECTORY) {
            return Err(zx::Status::NOT_DIR);
        }

        // We allow write and executable access because the client might not know this is a symbolic
        // link and they want to open the target of the link with write or executable rights.
        let optional = fio::OpenFlags::NOT_DIRECTORY
            | fio::OpenFlags::DESCRIBE
            | fio::OpenFlags::RIGHT_WRITABLE
            | fio::OpenFlags::RIGHT_EXECUTABLE;

        if *self & !optional != fio::OpenFlags::RIGHT_READABLE {
            return Err(zx::Status::INVALID_ARGS);
        }

        Ok(SymlinkOptions)
    }

    fn to_node_options(&self, entry_info: &EntryInfo) -> Result<NodeOptions, zx::Status> {
        // Strictly, we shouldn't allow rights to be specified with NODE_REFERENCE, but there's a
        // CTS pkgdir test that asserts these flags work and fixing that is painful so we preserve
        // old behaviour (which permitted these flags).
        let allowed_rights =
            fio::OPEN_RIGHTS | fio::OpenFlags::POSIX_WRITABLE | fio::OpenFlags::POSIX_EXECUTABLE;
        if self.intersects(!(fio::OPEN_FLAGS_ALLOWED_WITH_NODE_REFERENCE | allowed_rights)) {
            Err(zx::Status::INVALID_ARGS)
        } else if self.contains(fio::OpenFlags::DIRECTORY)
            && entry_info.type_() != fio::DirentType::Directory
        {
            Err(zx::Status::NOT_DIR)
        } else {
            Ok(NodeOptions { rights: fio::Operations::GET_ATTRIBUTES })
        }
    }

    fn get_representation(&self) -> bool {
        false
    }

    fn is_append(&self) -> bool {
        self.contains(fio::OpenFlags::APPEND)
    }

    fn is_truncate(&self) -> bool {
        self.contains(fio::OpenFlags::TRUNCATE)
    }

    fn attributes(&self) -> fio::NodeAttributesQuery {
        fio::NodeAttributesQuery::empty()
    }

    fn create_attributes(&self) -> Option<&fio::MutableNodeAttributes> {
        None
    }

    fn create_directory(&self) -> bool {
        self.contains(fio::OpenFlags::DIRECTORY)
    }

    fn is_node(&self) -> bool {
        self.contains(fio::OpenFlags::NODE_REFERENCE)
    }
}
