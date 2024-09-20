// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::io::{Error, Write};

use crate::compiler::util::{emit_doc_string, snake_to_camel};
use crate::compiler::wire::emit_type;
use crate::compiler::Compiler;
use crate::ir::CompIdent;

pub fn emit_union<W: Write>(
    compiler: &mut Compiler<'_>,
    out: &mut W,
    ident: &CompIdent,
) -> Result<(), Error> {
    let u = &compiler.library.union_declarations[ident];

    let name = &u.name.type_name();

    let (access_params, access_args) =
        if u.members.is_empty() { ("", "") } else { ("<'buf>", "<'_>") };

    // Write required wire type

    emit_doc_string(out, u.attributes.doc_string())?;
    writeln!(
        out,
        r#"
        #[repr(transparent)]
        pub struct Wire{name}<'buf> {{
            raw: ::fidl::RawWireUnion<'buf>,
        }}

        pub enum Wire{name}Ref{access_params} {{
        "#,
    )?;

    for member in &u.members {
        let member_name = snake_to_camel(&member.name);

        write!(out, "{member_name}(&'buf ")?;
        emit_type(compiler, out, &member.ty)?;
        writeln!(out, "),")?;
    }

    if !u.is_strict {
        writeln!(out, "Unknown(u64),")?;
    }

    writeln!(
        out,
        r#"
        }}

        pub enum Wire{name}Mut{access_params} {{
        "#,
    )?;

    for member in &u.members {
        let member_name = snake_to_camel(&member.name);

        write!(out, "{member_name}(&'buf mut ")?;
        emit_type(compiler, out, &member.ty)?;
        writeln!(out, "),")?;
    }

    if !u.is_strict {
        writeln!(out, "Unknown(u64),")?;
    }

    writeln!(
        out,
        r#"
        }}

        impl Wire{name}<'_> {{
            pub fn as_ref(&self) -> Wire{name}Ref{access_args} {{
                match self.raw.ordinal() {{
        "#,
    )?;

    for member in &u.members {
        let member_name = snake_to_camel(&member.name);
        let ordinal = member.ordinal;

        write!(
            out,
            r#"
            {ordinal} => Wire{name}Ref::{member_name}(
                unsafe {{ self.raw.get().deref_unchecked() }}
            ),
            "#,
        )?;
    }

    if u.is_strict {
        writeln!(out, "_ => unsafe {{ ::core::hint::unreachable_unchecked() }},",)?;
    } else {
        writeln!(out, "unknown => Wire{name}Ref::Unknown(unknown),")?;
    }

    writeln!(
        out,
        r#"
                }}
            }}

            pub fn as_mut(&mut self) -> Wire{name}Mut{access_args} {{
                match self.raw.ordinal() {{
        "#,
    )?;

    for member in &u.members {
        let member_name = snake_to_camel(&member.name);
        let ordinal = member.ordinal;

        write!(
            out,
            r#"
            {ordinal} => Wire{name}Mut::{member_name}(
                unsafe {{ self.raw.get_mut().deref_mut_unchecked() }}
            ),
            "#,
        )?;
    }

    if u.is_strict {
        writeln!(out, "_ => unsafe {{ ::core::hint::unreachable_unchecked() }},",)?;
    } else {
        writeln!(out, "unknown => Wire{name}Mut::Unknown(unknown),")?;
    }

    writeln!(
        out,
        r#"
                }}
            }}
        }}

        unsafe impl<'buf> ::fidl::Decode<'buf> for Wire{name}<'buf> {{
            fn decode(
                mut slot: ::fidl::Slot<'_, Self>,
                decoder: &mut ::fidl::decode::Decoder<'buf>,
            ) -> Result<(), ::fidl::decode::Error> {{
                ::fidl::munge!(let Self {{ mut raw }} = slot.as_mut());
                match ::fidl::RawWireUnion::encoded_ordinal(raw.as_mut()) {{
        "#,
    )?;

    for member in &u.members {
        let ord = member.ordinal;

        writeln!(out, "{ord} => ::fidl::RawWireUnion::decode_as::<")?;
        emit_type(compiler, out, &member.ty)?;
        writeln!(out, ">(raw, decoder)?,")?;
    }

    if u.is_strict {
        writeln!(
            out,
            r#"
            ord => return Err(fidl::decode::Error::InvalidUnionOrdinal(
                ord as usize
            )),
            "#,
        )?;
    } else {
        writeln!(out, "_ => ::fidl::RawWireUnion::decode_unknown(raw, decoder)?,",)?;
    }

    writeln!(
        out,
        r#"
                }}

                Ok(())
            }}
        }}

        impl<'buf> ::core::fmt::Debug for Wire{name}<'buf> {{
            fn fmt(
                &self,
                f: &mut ::core::fmt::Formatter<'_>,
            ) -> ::core::fmt::Result {{
                match self.raw.ordinal() {{
        "#,
    )?;

    for member in &u.members {
        let ord = member.ordinal;

        writeln!(out, "{ord} => unsafe {{ self.raw.get().deref_unchecked::<")?;
        emit_type(compiler, out, &member.ty)?;
        writeln!(out, ">().fmt(f) }},")?;
    }

    writeln!(
        out,
        r#"
                    _ => unsafe {{ ::core::hint::unreachable_unchecked() }},
                }}
            }}
        }}
        "#,
    )?;

    // Write optional wire type

    writeln!(
        out,
        r#"
        #[repr(transparent)]
        pub struct WireOptional{name}<'buf> {{
            raw: ::fidl::RawWireUnion<'buf>,
        }}

        impl<'buf> WireOptional{name}<'buf> {{
            pub fn is_some(&self) -> bool {{
                self.raw.is_some()
            }}

            pub fn is_none(&self) -> bool {{
                self.raw.is_none()
            }}

            pub fn as_ref(&self) -> Option<&Wire{name}<'buf>> {{
                if self.is_some() {{
                    Some(unsafe {{ &*(self as *const Self).cast() }})
                }} else {{
                    None
                }}
            }}

            pub fn as_mut(&mut self) -> Option<&mut Wire{name}<'buf>> {{
                if self.is_some() {{
                    Some(unsafe {{ &mut *(self as *mut Self).cast() }})
                }} else {{
                    None
                }}
            }}

            pub fn take(&mut self) -> Option<Wire{name}<'buf>> {{
                if self.is_some() {{
                    Some(Wire{name} {{
                        raw: ::core::mem::replace(
                            &mut self.raw,
                            ::fidl::RawWireUnion::null(),
                        )
                    }})
                }} else {{
                    None
                }}
            }}
        }}

        impl<'buf> Default for WireOptional{name}<'buf> {{
            fn default() -> Self {{
                Self {{
                    raw: ::fidl::RawWireUnion::null(),
                }}
            }}
        }}

        unsafe impl<'buf> ::fidl::Decode<'buf> for WireOptional{name}<'buf> {{
            fn decode(
                mut slot: ::fidl::Slot<'_, Self>,
                decoder: &mut ::fidl::decode::Decoder<'buf>,
            ) -> Result<(), ::fidl::decode::Error> {{
                ::fidl::munge!(let Self {{ mut raw }} = slot.as_mut());
                match ::fidl::RawWireUnion::encoded_ordinal(raw.as_mut()) {{
        "#,
    )?;

    for member in &u.members {
        let ord = member.ordinal;

        writeln!(out, "{ord} => ::fidl::RawWireUnion::decode_as::<")?;
        emit_type(compiler, out, &member.ty)?;
        writeln!(out, ">(raw, decoder)?,")?;
    }

    writeln!(
        out,
        r#"
                    0 => ::fidl::RawWireUnion::decode_absent(raw)?,
                    _ => ::fidl::RawWireUnion::decode_unknown(
                        raw,
                        decoder,
                    )?,
                }}

                Ok(())
            }}
        }}
        "#,
    )?;

    if compiler.config.emit_debug_impls {
        writeln!(
            out,
            r#"
            impl<'buf> ::core::fmt::Debug for WireOptional{name}<'buf> {{
                fn fmt(
                    &self,
                    f: &mut ::core::fmt::Formatter<'_>,
                ) -> ::core::fmt::Result {{
                    self.as_ref().fmt(f)
                }}
            }}
            "#
        )?;
    }

    Ok(())
}
