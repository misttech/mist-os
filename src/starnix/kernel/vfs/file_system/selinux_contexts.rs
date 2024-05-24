// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

///! SELinux security context structures for filesystems.
use crate::vfs::{FsStr, FsString};
use starnix_uapi::{error, errors::Errno};
use std::{collections::HashMap, ops::Deref};

/// SELinux security context structures for managing the incompatible
/// `context=[security-context]` and `defcontext=[security-context]` mount options.
mod def {
    use crate::vfs::{FsStr, FsString};

    #[derive(Clone, Debug, PartialEq)]
    enum ContextType {
        /// Tag for a `context=[security-context]` mount option.
        Context,
        /// Tag for a `defcontext=[security-context]` mount option.
        DefContext,
    }

    /// Unified type for exactly one of the incompatible `context=[security-context]` and
    /// `defcontext=[security-context]` mount options.
    #[derive(Clone, Debug, PartialEq)]
    pub struct Context {
        /// Tag to distinguish `context=[security-context]` and `defcontext=[security-context]`.
        context_type: ContextType,
        /// Security context string.
        context: FsString,
    }

    impl Context {
        /// Construct a `context=[security-context]` mount option value.
        pub(super) fn new_context(context: FsString) -> Self {
            Self { context_type: ContextType::Context, context }
        }

        /// Construct a `defcontext=[security-context]` mount option value.
        pub(super) fn new_defcontext(context: FsString) -> Self {
            Self { context_type: ContextType::DefContext, context }
        }

        /// The a `context=[security-context]` mount option value or `None`.
        pub fn context(&self) -> Option<&FsStr> {
            match self.context_type {
                ContextType::Context => Some(self.context.as_ref()),
                _ => None,
            }
        }

        /// The a `defcontext=[security-context]` mount option value or `None`.
        pub fn defcontext(&self) -> Option<&FsStr> {
            match self.context_type {
                ContextType::DefContext => Some(self.context.as_ref()),
                _ => None,
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[::fuchsia::test]
        fn test_context() {
            let context_str = b"user_u:unconfined_r:unconfined_t:s0";
            let context = Context::new_context(context_str.into());
            assert_eq!(Some(context_str.into()), context.context());
            assert_eq!(None, context.defcontext());
        }

        #[::fuchsia::test]
        fn test_defcontext() {
            let context_str = b"user_u:unconfined_r:unconfined_t:s0";
            let context = Context::new_defcontext(context_str.into());
            assert_eq!(Some(context_str.into()), context.defcontext());
            assert_eq!(None, context.context());
        }
    }
}

/// SELinux security context-related filesystem mount options. These options are documented in the
/// `context=context, fscontext=context, defcontext=context, and rootcontext=context` section of
/// the `mount(8)` manpage.
#[derive(Clone, Debug, PartialEq)]
pub struct SeLinuxContexts {
    /// The value of either:
    ///
    /// 1. The `context=[security-context]` mount option, or
    /// 1. The `defcontext=[security-context]` mount option.
    ///
    /// The former is used as the immutable security context for all directories in the system,
    /// and, additionally, as the filesystem (superblock) security context only if
    /// `fscontext=[security-context]` is not set. The latter defines the default security
    /// context used for unlabeled files and directories in this filesystem. These two options
    /// cannot both be set on a filesystem.
    context: Option<def::Context>,
    /// The value of the `fscontext=[security-context]` mount option. This option is used to
    /// label the filesystem (superblock) itself.
    fscontext: Option<FsString>,
    /// The value of the `rootcontext=[security-context]` mount option. This option is used to
    /// (re)label the inode located at the filesystem mountpoint.
    rootcontext: Option<FsString>,
}

impl SeLinuxContexts {
    pub fn from_defcontext(defcontext: FsString) -> Self {
        Self {
            context: Some(def::Context::new_defcontext(defcontext)),
            fscontext: None,
            rootcontext: None,
        }
    }

    /// Attempts to construct a [`SeLinuxContexts`] instance. Returns `Ok(None)` if no security
    /// context options are set. Returns `Err(...)` if incompatible mount options should prevent
    /// mount from succeeding.
    pub(super) fn try_from_mount_options(
        options: &HashMap<FsString, FsString>,
    ) -> Result<Option<Self>, Errno> {
        let context_options = options
            .into_iter()
            .filter(|(key, _value)| {
                ***key == b"context"
                    || ***key == b"fscontext"
                    || ***key == b"defcontext"
                    || ***key == b"rootcontext"
            })
            .collect::<HashMap<_, _>>();

        // The `defcontext` and `context` options cannot be used together.
        match (
            context_options.get(&&FsString::from(b"defcontext")),
            context_options.get(&&FsString::from(b"context")),
        ) {
            (Some(_), Some(_)) => return error!(EINVAL),
            _ => {}
        }

        let get_option_value = |key: &[u8]| -> Option<FsString> {
            context_options.get(&&FsString::from(key)).map(Deref::deref).map(Clone::clone)
        };

        let context = if let Some(context) = get_option_value(b"context") {
            Some(def::Context::new_context(context))
        } else if let Some(defcontext) = get_option_value(b"defcontext") {
            Some(def::Context::new_defcontext(defcontext))
        } else {
            None
        };
        let fscontext = get_option_value(b"fscontext");
        let rootcontext = get_option_value(b"rootcontext");

        Ok(if context.is_some() || fscontext.is_some() || rootcontext.is_some() {
            Some(Self { context, fscontext, rootcontext })
        } else {
            None
        })
    }

    /// Returns the value of the `context=[security-context]` mount option, if set.
    pub fn context(&self) -> Option<&FsStr> {
        self.context.as_ref().and_then(def::Context::context)
    }

    /// Returns the value of the `defcontext=[security-context]` mount option, if set.
    pub fn defcontext(&self) -> Option<&FsStr> {
        self.context.as_ref().and_then(def::Context::defcontext)
    }

    /// Returns the value of the `fscontext=[security-context]` mount option, if set.
    pub fn fscontext(&self) -> Option<&FsStr> {
        self.fscontext.as_ref().map(AsRef::as_ref)
    }

    /// Returns the value of the `rootcontext=[security-context]` mount option, if set.
    pub fn rootcontext(&self) -> Option<&FsStr> {
        self.rootcontext.as_ref().map(AsRef::as_ref)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use maplit::hashmap;

    #[::fuchsia::test]
    fn test_context_defcontext() {
        assert_eq!(
            error!(EINVAL),
            SeLinuxContexts::try_from_mount_options(&hashmap! {
                b"context".into() => b"user_u:unconfined_r:unconfined_t:s0".into(),
                b"defcontext".into() => b"system_u:object_r:system_t:s0".into(),
            })
        );
    }

    #[::fuchsia::test]
    fn test_context_fscontext_rootcontext() {
        let contexts = SeLinuxContexts::try_from_mount_options(&hashmap! {
            b"context".into() => b"user_u:unconfined_r:unconfined_t:s0".into(),
            b"fscontext".into() => b"fs_u:object_r:fs_t:s0".into(),
            b"rootcontext".into() => b"fsroot_u:object_r:fsroot_t:s0".into(),
        })
        .expect("context, fscontext, rootcontext compatible")
        .expect("options contain filesystem security contexts");
        assert_eq!(Some(b"user_u:unconfined_r:unconfined_t:s0".into()), contexts.context());
        assert_eq!(None, contexts.defcontext());
        assert_eq!(Some(b"fs_u:object_r:fs_t:s0".into()), contexts.fscontext());
        assert_eq!(Some(b"fsroot_u:object_r:fsroot_t:s0".into()), contexts.rootcontext());
    }

    #[::fuchsia::test]
    fn test_defcontext_fscontext_rootcontext() {
        let contexts = SeLinuxContexts::try_from_mount_options(&hashmap! {
            b"defcontext".into() => b"user_u:unconfined_r:unconfined_t:s0".into(),
            b"fscontext".into() => b"fs_u:object_r:fs_t:s0".into(),
            b"rootcontext".into() => b"fsroot_u:object_r:fsroot_t:s0".into(),
        })
        .expect("defcontext, fscontext, rootcontext compatible")
        .expect("options contain filesystem security contexts");
        assert_eq!(None, contexts.context());
        assert_eq!(Some(b"user_u:unconfined_r:unconfined_t:s0".into()), contexts.defcontext());
        assert_eq!(Some(b"fs_u:object_r:fs_t:s0".into()), contexts.fscontext());
        assert_eq!(Some(b"fsroot_u:object_r:fsroot_t:s0".into()), contexts.rootcontext());
    }
}
