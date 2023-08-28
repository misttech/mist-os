// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use darling::{FromDeriveInput, FromMeta};
use proc_macro::TokenStream;
use quote::quote;
use syn::{parse_macro_input, DeriveInput};

/// Attributes for the [Capability] derive macro.
#[derive(FromDeriveInput, Default)]
#[darling(default, attributes(capability))]
struct CapabilityAttributes {
    /// If set, generate a [TryClone] implementation.
    try_clone: Option<TryClone>,

    /// If set, generate a [Convert] implementation.
    convert: Option<Convert>,
}

/// Generated implementation to use for [TryClone].
#[derive(FromMeta, Debug)]
#[darling(rename_all = "snake_case")]
enum TryClone {
    /// The type is clonable using [std::clone::Clone].
    ///
    /// [TryClone.try_clone] will always return an `Ok` value.
    Clone,
    /// The type is not clonable.
    ///
    /// [TryClone.try_clone] will always return an `Err` value.
    Err,
}

/// Generated implementation to use for [Convert].
#[derive(FromMeta, Debug)]
#[darling(rename_all = "snake_case")]
enum Convert {
    /// The type can only be converted to itself.
    ///
    /// [Convert.try_into_capability] will return an error for any [TypeId] that
    /// does not match the `Self` type.
    ToSelfOnly,
}

/// Derive macro that implements [Capability] and its super traits.
///
/// # Example
///
/// ```
/// #[derive(Capability, Debug)]
/// struct MyCapability;
/// ```
///
/// # `TryFrom<AnyCapability>`
///
/// The derived Capability implements `TryFrom<AnyCapability>`, and by extension,
/// `TryInto<T> for AnyCapability`, for convenient downcasting and conversion to a concrete type.
///
/// ```
/// let cap = MyCapability {};
/// let any: AnyCapability = Box::new(cap);
/// let downcast: MyCapability = any.try_into()?;
/// ```
///
/// The capability can be converted to a different type, provided that its
/// [Convert.try_into_capability] implementation allows it:
///
/// ```
/// let cap = MyCapability {};
/// let any: AnyCapability = Box::new(cap);
/// let some_other: SomeOtherCapability = any.try_into()?;
/// ```
///
/// # `TryClone`
///
/// All capabilities must implement `TryClone`. If your capability implements the standard Rust
/// `Clone` trait, use the `try_clone = "clone"` attribute to generate a `TryClone` implementation
/// that delegates to `Clone`:
///
/// ```
/// #[derive(Capability, Clone, Debug)]
/// #[capability(try_clone = "clone")]
/// struct MyCloneableCapability;
///
/// let cap = MyCloneableCapability {};
/// assert!(cap.try_clone().is_ok());  // TryClone always succeeds because it uses Clone.
/// ```
///
/// This works regardless if `Clone` is derived or explicitly implemented.
///
/// If your type is not cloneable, use `try_clone = "err"` to generate an implementation
/// that always returns an error.
///
/// ```
/// #[derive(Capability, Debug)]
/// #[capability(try_clone = "err")]
/// struct MyCapability;
///
/// let cap = MyCapability {};
/// assert!(cap.try_clone().is_err());  // TryClone always fails.
/// ```
///
/// Omit the `try_clone` attribute if you want to supply your own `TryClone` implementation.
///
/// # `Convert`
///
/// All capabilities must implement the `Convert`. This trait converts between capability types,
/// used in the `TryFrom<AnyCapability>` implementation.
///
/// Use the `convert = "to_self_only"` attribute to generate a minimal `Convert` implementation
/// that can convert the value into its own type:
///
/// ```
/// #[derive(Capability, Debug)]
/// #[capability(convert = "to_self_only")]
/// struct MyCapability;
///
/// let cap = MyCapability {};
/// let converted: Box<dyn Any> = cap.try_into_capability(TypeId::of::<MyCapability>()).unwrap();
/// let cap: MyCapability = *converted.downcast::<MyCapability>().unwrap();
/// ```
///
/// Omit the `convert` attribute if you want to supply your own `Convert` implementation.
#[proc_macro_derive(Capability, attributes(capability))]
pub fn derive_capability(input: TokenStream) -> TokenStream {
    let input: DeriveInput = parse_macro_input!(input);
    let attrs = CapabilityAttributes::from_derive_input(&input).expect("invalid options");
    let name = &input.ident;

    let (impl_generics, ty_generics, where_clause) = input.generics.split_for_impl();

    let try_clone = match attrs.try_clone {
        Some(TryClone::Clone) => quote! {
            impl #impl_generics ::sandbox::TryClone for #name #ty_generics #where_clause {
                #[inline]
                fn try_clone(&self) -> Result<Self, ()> {
                    Ok(self.clone())
                }
            }
        },
        Some(TryClone::Err) => quote! {
            impl #impl_generics ::sandbox::TryClone for #name #ty_generics #where_clause {
                #[inline]
                fn try_clone(&self) -> Result<Self, ()> {
                    Err(())
                }
            }
        },
        None => quote! {},
    };

    let convert = match attrs.convert {
        Some(Convert::ToSelfOnly) => quote! {
            impl #impl_generics ::sandbox::Convert for #name #ty_generics #where_clause {
                #[inline]
                fn try_into_capability(self, type_id: ::std::any::TypeId) -> Result<Box<dyn ::std::any::Any>, ()> {
                    if type_id == ::std::any::TypeId::of::<Self>() {
                        return Ok(Box::new(self) as Box<dyn ::std::any::Any>);
                    }
                    Err(())
                }
            }
        },
        None => quote! {},
    };

    TokenStream::from(quote! {
        impl #impl_generics ::sandbox::Capability for #name #ty_generics #where_clause {}

        impl #impl_generics TryFrom<::sandbox::AnyCapability> for #name #ty_generics #where_clause {
            type Error = ();

            fn try_from(value: ::sandbox::AnyCapability) -> Result<Self, Self::Error> {
                if value.as_any().is::<Self>() {
                    return Ok(*value.into_any().downcast::<Self>().unwrap());
                }
                if let Ok(converted) = <::sandbox::AnyCapability as ::sandbox::Convert>::try_into_capability(value, std::any::TypeId::of::<Self>()) {
                    return Ok(*converted.downcast::<Self>().unwrap());
                }
                return Err(());
            }
        }

        #try_clone

        #convert
    })
}
