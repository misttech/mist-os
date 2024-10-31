// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::io::{Error, Write};

use crate::compiler::util::{emit_doc_string, snake_to_camel};
use crate::compiler::wire::emit_type;
use crate::compiler::Compiler;
use crate::ir::CompIdent;

// TODO: wire unions need a drop impl

pub fn emit_union<W: Write>(
    compiler: &mut Compiler<'_>,
    out: &mut W,
    ident: &CompIdent,
) -> Result<(), Error> {
    let u = &compiler.schema.union_declarations[ident];

    let name = &u.name.type_name();
    let is_static = u.shape.max_out_of_line == 0;
    let mut has_only_static_members = true;
    for member in &u.members {
        if member.ty.shape.max_out_of_line != 0 {
            has_only_static_members = false;
            break;
        }
    }

    let (params, phantom, decode_param, decode_where, decode_unknown, decode_as) = if is_static {
        (
            "",
            "()",
            "",
            "___D: ::fidl_next::decoder::InternalHandleDecoder",
            "decode_unknown_static",
            "decode_as_static",
        )
    } else {
        (
            "<'buf>",
            "&'buf mut [::fidl_next::Chunk]",
            "'buf, ",
            "___D: ::fidl_next::Decoder<'buf>",
            "decode_unknown",
            "decode_as",
        )
    };
    let (access_params, access_args) = if u.members.is_empty() {
        ("", "")
    } else if has_only_static_members {
        ("<'union>", "<'_>")
    } else {
        ("<'union, 'buf>", "<'_, 'buf>")
    };

    // Write required wire type

    emit_doc_string(out, u.attributes.doc_string())?;
    writeln!(
        out,
        r#"
        #[repr(transparent)]
        pub struct Wire{name}{params} {{
            raw: ::fidl_next::RawWireUnion,
            _phantom: ::core::marker::PhantomData<{phantom}>,
        }}

        pub enum Wire{name}Ref{access_params} {{
        "#,
    )?;

    for member in &u.members {
        let member_name = snake_to_camel(&member.name);

        write!(out, "{member_name}(&'union ")?;
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

        write!(out, "{member_name}(&'union mut ")?;
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

        impl{params} Wire{name}{params} {{
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
        "#
    )?;

    if is_static {
        writeln!(
            out,
            r#"
            impl Clone for Wire{name} {{
                fn clone(&self) -> Self {{
                    match self.raw.ordinal() {{
            "#,
        )?;

        for member in &u.members {
            let ordinal = member.ordinal;

            write!(out, "{ordinal} => Self {{ raw: unsafe {{ self.raw.clone_unchecked::<",)?;

            emit_type(compiler, out, &member.ty)?;

            write!(out, ">() }}, _phantom: ::core::marker::PhantomData }},",)?;
        }

        if u.is_strict {
            writeln!(out, "_ => unsafe {{ ::core::hint::unreachable_unchecked() }},",)?;
        } else {
            writeln!(
                out,
                r#"
                _ => Self {{
                    raw: unsafe {{ self.raw.clone_unchecked::<()>() }},
                    _phantom: ::core::marker::PhantomData,
                }},
                "#,
            )?;
        }

        writeln!(
            out,
            r#"
                    }}
                }}
            }}
            "#
        )?;
    }

    writeln!(
        out,
        r#"
        unsafe impl<{decode_param}___D: ?Sized> ::fidl_next::Decode<___D> for Wire{name}{params}
        where
            {decode_where},
        "#,
    )?;

    for member in &u.members {
        emit_type(compiler, out, &member.ty)?;
        writeln!(out, ": ::fidl_next::Decode<___D>,")?;
    }

    writeln!(
        out,
        r#"
        {{
            fn decode(
                mut slot: ::fidl_next::Slot<'_, Self>,
                decoder: &mut ___D,
            ) -> Result<(), ::fidl_next::DecodeError> {{
                ::fidl_next::munge!(let Self {{ mut raw, _phantom: _ }} = slot.as_mut());
                match ::fidl_next::RawWireUnion::encoded_ordinal(raw.as_mut()) {{
        "#,
    )?;

    for member in &u.members {
        let ord = member.ordinal;

        writeln!(out, "{ord} => ::fidl_next::RawWireUnion::{decode_as}::<___D, ")?;
        emit_type(compiler, out, &member.ty)?;
        writeln!(out, ">(raw, decoder)?,")?;
    }

    if u.is_strict {
        writeln!(
            out,
            r#"
            ord => return Err(::fidl_next::DecodeError::InvalidUnionOrdinal(
                ord as usize
            )),
            "#,
        )?;
    } else {
        // TODO: if static, decode_unknown_static
        writeln!(out, "_ => ::fidl_next::RawWireUnion::{decode_unknown}(raw, decoder)?,",)?;
    }

    writeln!(
        out,
        r#"
                }}

                Ok(())
            }}
        }}

        impl{params} ::core::fmt::Debug for Wire{name}{params} {{
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
        pub struct WireOptional{name}{params} {{
            raw: ::fidl_next::RawWireUnion,
            _phantom: ::core::marker::PhantomData<{phantom}>,
        }}

        impl{params} WireOptional{name}{params} {{
            pub fn is_some(&self) -> bool {{
                self.raw.is_some()
            }}

            pub fn is_none(&self) -> bool {{
                self.raw.is_none()
            }}

            pub fn as_ref(&self) -> Option<&Wire{name}{params}> {{
                if self.is_some() {{
                    Some(unsafe {{ &*(self as *const Self).cast() }})
                }} else {{
                    None
                }}
            }}

            pub fn as_mut(&mut self) -> Option<&mut Wire{name}{params}> {{
                if self.is_some() {{
                    Some(unsafe {{ &mut *(self as *mut Self).cast() }})
                }} else {{
                    None
                }}
            }}

            pub fn take(&mut self) -> Option<Wire{name}{params}> {{
                if self.is_some() {{
                    Some(Wire{name} {{
                        raw: ::core::mem::replace(
                            &mut self.raw,
                            ::fidl_next::RawWireUnion::null(),
                        ),
                        _phantom: ::core::marker::PhantomData,
                    }})
                }} else {{
                    None
                }}
            }}
        }}

        impl{params} Default for WireOptional{name}{params} {{
            fn default() -> Self {{
                Self {{
                    raw: ::fidl_next::RawWireUnion::null(),
                    _phantom: ::core::marker::PhantomData,
                }}
            }}
        }}
        "#,
    )?;

    if is_static {
        writeln!(
            out,
            r#"
            impl Clone for WireOptional{name} {{
                fn clone(&self) -> Self {{
                    if self.is_none() {{
                        return WireOptional{name} {{
                            raw: ::fidl_next::RawWireUnion::null(),
                            _phantom: ::core::marker::PhantomData,
                        }};
                    }}

                    match self.raw.ordinal() {{
            "#,
        )?;

        for member in &u.members {
            let ordinal = member.ordinal;

            write!(out, "{ordinal} => Self {{ raw: unsafe {{ self.raw.clone_unchecked::<",)?;

            emit_type(compiler, out, &member.ty)?;

            write!(out, ">() }}, _phantom: ::core::marker::PhantomData }},",)?;
        }

        if u.is_strict {
            writeln!(out, "_ => unsafe {{ ::core::hint::unreachable_unchecked() }},",)?;
        } else {
            writeln!(
                out,
                r#"
                _ => Self {{
                    raw: unsafe {{ self.raw.clone_unchecked::<()>() }},
                    _phantom: ::core::marker::PhantomData,
                }},
                "#,
            )?;
        }

        writeln!(
            out,
            r#"
                    }}
                }}
            }}
            "#,
        )?;
    }

    writeln!(
        out,
        r#"
        unsafe impl<{decode_param}___D: ?Sized> ::fidl_next::Decode<___D> for WireOptional{name}{params}
        where
            {decode_where},
        "#,
    )?;

    for member in &u.members {
        emit_type(compiler, out, &member.ty)?;
        writeln!(out, ": ::fidl_next::Decode<___D>,")?;
    }

    writeln!(
        out,
        r#"
        {{
            fn decode(
                mut slot: ::fidl_next::Slot<'_, Self>,
                decoder: &mut ___D,
            ) -> Result<(), ::fidl_next::DecodeError> {{
                ::fidl_next::munge!(let Self {{ mut raw, _phantom: _ }} = slot.as_mut());
                match ::fidl_next::RawWireUnion::encoded_ordinal(raw.as_mut()) {{
        "#,
    )?;

    for member in &u.members {
        let ord = member.ordinal;

        writeln!(out, "{ord} => ::fidl_next::RawWireUnion::{decode_as}::<___D, ")?;
        emit_type(compiler, out, &member.ty)?;
        writeln!(out, ">(raw, decoder)?,")?;
    }

    writeln!(
        out,
        r#"
                    0 => ::fidl_next::RawWireUnion::decode_absent(raw)?,
                    _ => ::fidl_next::RawWireUnion::{decode_unknown}(
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
            impl{params} ::core::fmt::Debug for WireOptional{name}{params} {{
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
