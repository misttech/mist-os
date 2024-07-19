// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

/// A class that may appear in SELinux policy or an access vector cache query.
#[derive(Clone, Debug, PartialEq)]
pub enum AbstractObjectClass {
    Unspecified,
    /// A well-known class used in the SELinux system, such as `process` or `file`.
    System(ObjectClass),
    /// A custom class that only has meaning in policies that define class with the given string
    /// name.
    Custom(String),
}

impl Default for AbstractObjectClass {
    fn default() -> Self {
        Self::Unspecified
    }
}

macro_rules! enumerable_enum {
    ($(#[$meta:meta])* $name:ident {
        $($(#[$variant_meta:meta])* $variant:ident),*,
    }) => {
        $(#[$meta])*
        pub enum $name {
            $($(#[$variant_meta])* $variant),*
        }

        impl $name {
            pub fn all_variants() -> Vec<Self> {
                vec![
                    $($name::$variant),*
                ]
            }
        }
    }
}

enumerable_enum! {
    /// A well-known class in SELinux policy that has a particular meaning in policy enforcement
    /// hooks.
    #[derive(Clone, Debug, Eq, Hash, PartialEq)]
    ObjectClass {
        /// The SELinux "process" object class.
        Process,
        /// The SELinux "file" object class.
        File,
        /// The SELinux "blk_file" object class.
        Block,
        /// The SELinux "chr_file" object class.
        Character,
        /// The SELinux "lnk_file" object class.
        Link,
        /// The SELinux "fifo_file" object class.
        Fifo,
        /// The SELinux "sock_file" object class.
        Socket,
    }
}

impl ObjectClass {
    pub fn name(&self) -> &'static str {
        match self {
            Self::Process => "process",
            Self::File => "file",
            Self::Block => "blk_file",
            Self::Character => "chr_file",
            Self::Link => "lnk_file",
            Self::Fifo => "fifo_file",
            Self::Socket => "sock_file",
        }
    }
}

impl From<ObjectClass> for AbstractObjectClass {
    fn from(object_class: ObjectClass) -> Self {
        Self::System(object_class)
    }
}

impl From<String> for AbstractObjectClass {
    fn from(name: String) -> Self {
        Self::Custom(name)
    }
}

enumerable_enum! {
    /// A well-known file-like class in SELinux policy that has a particular meaning in policy
    /// enforcement hooks.
    #[derive(Clone, Debug, Eq, Hash, PartialEq)]
    FileClass {
        /// The SELinux "file" object class.
        File,
        /// The SELinux "blk_file" object class.
        Block,
        /// The SELinux "chr_file" object class.
        Character,
        /// The SELinux "lnk_file" object class.
        Link,
        /// The SELinux "fifo_file" object class.
        Fifo,
        /// The SELinux "sock_file" object class.
        Socket,
    }
}

impl From<FileClass> for ObjectClass {
    fn from(file_class: FileClass) -> Self {
        match file_class {
            FileClass::File => Self::File,
            FileClass::Block => Self::Block,
            FileClass::Character => Self::Character,
            FileClass::Link => Self::Link,
            FileClass::Fifo => Self::Fifo,
            FileClass::Socket => Self::Socket,
        }
    }
}

/// A permission that may appear in SELinux policy or an access vector cache query.
#[derive(Clone, Debug, PartialEq)]
pub enum AbstractPermission {
    /// A permission that is interpreted directly by the system. These are kernel objects such as
    /// a "process", "file", etc.
    System(Permission),
    /// A permission with an arbitrary string identifier.
    Custom { class: AbstractObjectClass, permission: String },
}

impl AbstractPermission {
    pub fn new_custom(class: AbstractObjectClass, permission: String) -> Self {
        Self::Custom { class, permission }
    }
}

impl From<Permission> for AbstractPermission {
    fn from(permission: Permission) -> Self {
        Self::System(permission)
    }
}

pub trait ClassPermission {
    fn class(&self) -> ObjectClass;
}

macro_rules! permission_enum {
    ($(#[$meta:meta])* $name:ident {
        $($(#[$variant_meta:meta])* $variant:ident($inner:ident)),*,
    }) => {
        $(#[$meta])*
        pub enum $name {
            $($(#[$variant_meta])* $variant($inner)),*
        }

        $(impl From<$inner> for $name {
            fn from(v: $inner) -> Self {
                Self::$variant(v)
            }
        })*

        impl ClassPermission for $name {
            fn class(&self) -> ObjectClass {
                match self {
                    $($name::$variant(_) => ObjectClass::$variant),*
                }
            }
        }

        impl $name {
            pub fn name(&self) -> &'static str {
                match self {
                    $($name::$variant(v) => v.name()),*
                }
            }

            pub fn all_variants() -> Vec<Self> {
                let mut all_variants = vec![];
                $(all_variants.extend($inner::all_variants().into_iter().map($name::from));)*
                all_variants
            }
        }
    }
}

permission_enum! {
    /// A well-known `(class, permission)` pair in SELinux policy that has a particular meaning in
    /// policy enforcement hooks.
    #[derive(Clone, Debug, Eq, Hash, PartialEq)]
    Permission {
        /// Permissions for the well-known SELinux "process" object class.
        Process(ProcessPermission),
        /// Permissions for the well-known SELinux "file" object class.
        File(FilePermission),
    }
}

macro_rules! class_permission_enum {
    ($(#[$meta:meta])* $name:ident {
        $($(#[$variant_meta:meta])* $variant:ident ($variant_name:literal)),*,
    }) => {
        enumerable_enum! {
            $(#[$meta])* $name {
                $($(#[$variant_meta])* $variant),*,
            }
        }

        impl ClassPermission for $name {
            fn class(&self) -> ObjectClass {
                Permission::from(self.clone()).class()
            }
        }

        impl $name {
            fn name(&self) -> &'static str {
                match self {
                    $($name::$variant => $variant_name),*
                }
            }
        }
    }
}

class_permission_enum! {
    /// A well-known "process" class permission in SELinux policy that has a particular meaning in
    /// policy enforcement hooks.
    #[derive(Clone, Debug, Eq, Hash, PartialEq)]
    ProcessPermission {
        /// Permission to fork the current running process.
        Fork("fork"),
        /// Permission to transition to a different security domain.
        Transition("transition"),
        /// Permission to get scheduling policy currently applied to a process.
        GetSched("getsched"),
        /// Permission to set scheduling policy for a process.
        SetSched("setsched"),
        /// Permission to get the process group ID.
        GetPgid("getpgid"),
        /// Permission to set the process group ID.
        SetPgid("setpgid"),
        /// Permission to send a signal other than SIGKILL, SIGSTOP, or SIGCHLD to a process.
        Signal("signal"),
        /// Permission to send SIGKILL to a process.
        SigKill("sigkill"),
        /// Permission to send SIGSTOP to a process.
        SigStop("sigstop"),
        /// Permission to send SIGCHLD to a process.
        SigChld("sigchld"),
        /// Permission to trace a process.
        Ptrace("ptrace"),
        /// Permission to get the session ID.
        GetSession("getsession"),
        /// Permission to set the calling task's current Security Context.
        /// The "dyntransition" permission separately limits which Contexts "setcurrent" may be used to transition to.
        SetCurrent("setcurrent"),
        /// Permission to set the Security Context used by `exec()`.
        SetExec("setexec"),
        /// Permission to set the Security Context used when creating filesystem objects.
        SetFsCreate("setfscreate"),
        /// Permission to set the Security Context used when creating kernel keyrings.
        SetKeyCreate("setkeycreate"),
        /// Permission to set the Security Context used when creating new labeled sockets.
        SetSockCreate("setsockcreate"),
        /// Permission to get the resource limits on a process.
        GetRlimit("getrlimit"),
        /// Permission to set the resource limits on a process.
        SetRlimit("setrlimit"),
    }
}

class_permission_enum! {
    /// A well-known "file" class permission in SELinux policy that has a particular meaning in
    /// policy enforcement hooks.
    #[derive(Clone, Debug, Eq, Hash, PartialEq)]
    FilePermission {
        /// Permission to create a file.
        Create("create"),
        /// Permission to open a file.
        Open("open"),
        /// Permission to use a file as an entry point to the calling domain without performing a
        /// transition.
        ExecuteNoTrans("execute_no_trans"),
        /// Permission to use a file as an entry point into the new domain on transition.
        Entrypoint("entrypoint"),
    }
}

/// Initial Security Identifier (SID) values defined by the SELinux Reference Policy.
/// Where the SELinux Reference Policy retains definitions for some deprecated initial SIDs, this
/// enum omits deprecated entries for clarity.
#[repr(u64)]
enum ReferenceInitialSid {
    Kernel = 1,
    _Security = 2,
    Unlabeled = 3,
    _Fs = 4,
    _File = 5,
    _AnySocket = 6,
    _Port = 7,
    _Netif = 8,
    _Netmsg = 9,
    _Node = 10,
    _Sysctl = 15,
    _Devnull = 25,

    FirstUnused,
}

/// Lowest Security Identifier value guaranteed not to be used by this
/// implementation to refer to an initial Security Context.
pub const FIRST_UNUSED_SID: u32 = ReferenceInitialSid::FirstUnused as u32;

macro_rules! initial_sid_enum {
    ($(#[$meta:meta])* $name:ident {
        $($(#[$variant_meta:meta])* $variant:ident ($variant_name: literal)),*,
    }) => {
        $(#[$meta])*
        pub enum $name {
            $($(#[$variant_meta])* $variant = ReferenceInitialSid::$variant as isize),*
        }

        impl $name {
            pub fn all_variants() -> Vec<Self> {
                vec![
                    $($name::$variant),*
                ]
            }

            pub fn name(&self) -> &'static str {
                match self {
                    $($name::$variant => $variant_name),*
                }
            }
        }
    }
}

initial_sid_enum! {
/// Initial Security Identifier (SID) values actually used by this implementation.
/// These must be present in the policy, for it to be valid.
#[derive(Copy, Clone, Debug, Eq, Hash, PartialEq)]
    InitialSid {
        Kernel("kernel"),
        Unlabeled("unlabeled"),
    }
}

/// A borrowed byte slice that contains no `NUL` characters by truncating the input slice at the
/// first `NUL` (if any) upon construction.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct NullessByteStr<'a>(&'a [u8]);

impl<'a> NullessByteStr<'a> {
    /// Returns a non-null-terminated representation of the security context string.
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

impl<'a, S: AsRef<[u8]> + ?Sized> From<&'a S> for NullessByteStr<'a> {
    /// Any `AsRef<[u8]>` can be processed into a [`NullessByteStr`]. The [`NullessByteStr`] will
    /// retain everything up to (but not including) a null character, or else the complete byte
    /// string.
    fn from(s: &'a S) -> Self {
        let value = s.as_ref();
        match value.iter().position(|c| *c == 0) {
            Some(end) => Self(&value[..end]),
            None => Self(value),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn object_class_permissions() {
        assert_eq!(AbstractObjectClass::Unspecified, AbstractObjectClass::default());
        assert_eq!(
            AbstractObjectClass::Custom(String::from("my_class")),
            String::from("my_class").into()
        );
        for variant in ProcessPermission::all_variants().into_iter() {
            assert_eq!(ObjectClass::Process, variant.class());
            assert_eq!("process", variant.class().name());
            let permission: Permission = variant.clone().into();
            assert_eq!(Permission::Process(variant.clone()), permission);
            assert_eq!(
                AbstractPermission::System(Permission::Process(variant.clone())),
                permission.into()
            );
            assert_eq!(AbstractObjectClass::System(ObjectClass::Process), variant.class().into());
        }
    }

    #[test]
    fn nulless_byte_str_equivalence() {
        let unterminated: NullessByteStr<'_> = b"u:object_r:test_valid_t:s0".into();
        let nul_terminated: NullessByteStr<'_> = b"u:object_r:test_valid_t:s0\0".into();
        let nul_containing: NullessByteStr<'_> =
            b"u:object_r:test_valid_t:s0\0IGNORE THIS\0!\0".into();

        for context in [nul_terminated, nul_containing] {
            assert_eq!(unterminated, context);
            assert_eq!(unterminated.as_bytes(), context.as_bytes());
        }
    }
}
