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
    }
}

/// Initial Security Identifier (SID) values defined by the SELinux Reference Policy.
/// The presence and ordering of all values, including ones unused by the implementation,
/// ensures that the numeric values match those output by userspace policy tooling.
#[repr(u64)]
enum ReferenceInitialSid {
    Kernel = 1,
    _Security,
    Unlabeled,
    _Fs,
    _File,
    _FileLabels,
    _Init,
    _AnySocket,
    _Port,
    _Netif,
    _Netmsg,
    _Node,
    _IgmpPacket,
    _IcmpSocket,
    _TcpSocket,
    _SysctlModprobe,
    _Sysctl,
    _SysctlFs,
    _SysctlKernel,
    _SysctlNet,
    _SysctlNetUnix,
    _SysctlVm,
    _SysctlDev,
    _Kmod,
    _Policy,
    _ScmpPacket,
    _Devnull,

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
}
