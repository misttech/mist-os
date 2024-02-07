// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

pub mod security_context;

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
    }
}

macro_rules! class_permission_enum {
    ($(#[$meta:meta])* $name:ident {
        $($(#[$variant_meta:meta])* $variant:ident),*,
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
    }
}

class_permission_enum! {
    /// A well-known "process" class permission in SELinux policy that has a particular meaning in
    /// policy enforcement hooks.
    #[derive(Clone, Debug, Eq, Hash, PartialEq)]
    ProcessPermission {
        /// Permission to fork the current running process.
        Fork,
        /// Permission to transition to a different security domain.
        Transition,
        /// Permission to get scheduling policy currently applied to a process.
        GetSched,
        /// Permission to set scheduling policy for a process.
        SetSched,
        /// Permission to get the process group ID.
        GetPgid,
        /// Permission to set the process group ID.
        SetPgid,
    }
}

impl ProcessPermission {
    pub fn name(&self) -> &'static str {
        match self {
            ProcessPermission::Fork => "fork",
            ProcessPermission::Transition => "transition",
            ProcessPermission::GetSched => "getsched",
            ProcessPermission::SetSched => "setsched",
            ProcessPermission::GetPgid => "getpgid",
            ProcessPermission::SetPgid => "setpgid",
        }
    }
}

impl From<Permission> for AbstractPermission {
    fn from(permission: Permission) -> Self {
        Self::System(permission)
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
