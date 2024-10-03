// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// Implements the HandleBased traits for a Handle newtype struct
macro_rules! impl_handle_based {
    ($type_name:path) => {
        impl AsHandleRef for $type_name {
            fn as_handle_ref(&self) -> HandleRef<'_> {
                self.0.as_handle_ref()
            }
        }

        impl From<Handle> for $type_name {
            fn from(handle: Handle) -> Self {
                $type_name(handle)
            }
        }

        impl From<$type_name> for Handle {
            fn from(x: $type_name) -> Handle {
                x.0
            }
        }

        impl HandleBased for $type_name {}
    };
}

/// Convenience macro for creating get/set property functions on an object.
///
/// This is for use when the underlying property type is a simple raw type.
/// It creates an empty 'tag' struct to implement the relevant PropertyQuery*
/// traits against. One, or both, of a getter and setter may be defined
/// depending upon what the property supports. Example usage is
/// unsafe_handle_propertyes!(ObjectType[get_foo_prop,set_foo_prop:FooPropTag,FOO,u32;]);
/// unsafe_handle_properties!(object: Foo,
///     props: [
///         {query_ty: FOO_BAR, tag: FooBarTag, prop_ty: usize, get:get_bar},
///         {query_ty: FOO_BAX, tag: FooBazTag, prop_ty: u32, set:set_baz},
///     ]
/// );
/// And will create
/// Foo::get_bar(&self) -> Result<usize, Status>
/// Foo::set_baz(&self, val: &u32) -> Result<(), Status>
/// Using Property::FOO as the underlying property.
///
///  # Safety
///
/// This macro will implement unsafe traits on your behalf and any combination
/// of query_ty and prop_ty must respect the Safety requirements detailed on the
/// PropertyQuery trait.
macro_rules! unsafe_handle_properties {
    (
        object: $object_ty:ty,
        props: [$( {
            query_ty: $query_ty:ident,
            tag: $query_tag:ident,
            prop_ty: $prop_ty:ty
            $(,get: $get:ident)*
            $(,set: $set:ident)*
            $(,)*
        }),*$(,)*]
    ) => {
        $(
            struct $query_tag {}
            unsafe impl PropertyQuery for $query_tag {
                const PROPERTY: Property = Property::$query_ty;
                type PropTy = $prop_ty;
            }

            $(
                impl $object_ty {
                    pub fn $get(&self) -> Result<$prop_ty, Status> {
                        object_get_property::<$query_tag>(self.as_handle_ref())
                    }
                }
            )*

            $(
                impl $object_ty {
                    pub fn $set(&self, val: &$prop_ty) -> Result<(), Status> {
                        object_set_property::<$query_tag>(self.as_handle_ref(), val)
                    }
                }
            )*
        )*
    }
}

// Creates associated constants of TypeName of the form
// `pub const NAME: TypeName = TypeName(path::to::value);`
// and provides a private `assoc_const_name` method and a `Debug` implementation
// for the type based on `$name`.
// If multiple names match, the first will be used in `name` and `Debug`.
#[macro_export]
macro_rules! assoc_values {
    ($typename:ident, [$($(#[$attr:meta])* $name:ident = $value:path;)*]) => {
        #[allow(non_upper_case_globals)]
        impl $typename {
            $(
                $(#[$attr])*
                pub const $name: $typename = $typename($value);
            )*

            fn assoc_const_name(&self) -> Option<&'static str> {
                match self.0 {
                    $(
                        $(#[$attr])*
                        $value => Some(stringify!($name)),
                    )*
                    _ => None,
                }
            }
        }

        impl ::std::fmt::Debug for $typename {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                f.write_str(concat!(stringify!($typename), "("))?;
                match self.assoc_const_name() {
                    Some(name) => f.write_str(&name)?,
                    None => ::std::fmt::Debug::fmt(&self.0, f)?,
                }
                f.write_str(")")
            }
        }
    }
}
