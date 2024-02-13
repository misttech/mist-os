// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Schema simplification is intended to take human-readable schemas, either written by hand using
//! the procedural macro or created via the derive macro.
//!
//! This strips documentation comments, non-recursive type aliases, sorts types and fields, reduces
//! enums to their simplest forms, and normalizes constants.
//!
//! These simplified schemas can be compared programatically. Tools like `git diff` can also
//! visualize schema changes when these schemas are written out line-by-line.

use bumpalo::{collections::Vec as BumpVec, Bump};
use std::{
    any::TypeId,
    collections::{hash_map, HashMap},
    ops::ControlFlow,
    ops::ControlFlow::*,
};

use crate::schema::{InlineValue, Type, ValueType};

/// Chain of active type aliases.
///
/// This type is bump allocated. `drop` will not be called.
/// Allocated resources like `Box` and `Vec` will not be freed.
struct AliasChain<'bump> {
    prev: Option<&'bump mut AliasChain<'bump>>,
    id: TypeId,
    recursive: bool,
}

impl<'bump> AliasChain<'bump> {
    /// Find the chain in the chain of aliases.
    fn search(&mut self, id: TypeId) -> Option<&'_ mut AliasChain<'bump>> {
        if self.id == id {
            return Some(self);
        }

        match &mut self.prev {
            Some(prev) => prev.search(id),
            None => None,
        }
    }
}

struct AliasCtx<'bump> {
    /// Chain of all current aliases being processed.
    current: Option<&'bump mut AliasChain<'bump>>,

    /// Collected info about previously processed aliases.
    ///
    /// Does not include aliases currently being processed.
    info: HashMap<TypeId, AliasInfo<'bump>>,
}

struct AliasInfo<'bump> {
    ty: &'bump Type<'bump>,
    recursive: bool,
}

pub struct Ctx<'bump> {
    alias: AliasCtx<'bump>,
}

impl<'bump> Ctx<'bump> {
    pub fn new() -> Self {
        Self { alias: AliasCtx { current: None, info: HashMap::new() } }
    }

    /// Processes a single type, applying edits until it no longer has any edits to apply.
    pub fn process_type(&mut self, bump: &'bump Bump, ty: &mut &'bump Type<'bump>) -> bool {
        let mut updated_type = false;

        // Run on types until the type is not edited or is removed.
        loop {
            let new_ty = if ty.is_leaf() {
                // Leaf types do not require any processing, except normalizing JSON null constants
                // to ValueType::Null.
                if matches!(ty, Type::Constant { value: InlineValue::Null }) {
                    &Type::Type { ty: ValueType::Null }
                } else {
                    break;
                }
            } else {
                let edit_result = self.process_type_inner(bump, ty);

                let Break(new_ty) = edit_result else {
                    // Type was not edited.
                    break;
                };

                new_ty
            };

            *ty = new_ty;
            updated_type = true;
        }

        return updated_type;
    }

    /// Attempts to run a processing step on the given type.
    ///
    /// Returns `ControlFlow::Break` if a step replaced it with a new type, `ControlFlow::Continue`
    /// otherwise.
    fn process_type_inner(
        &mut self,
        bump: &'bump Bump,
        ty: &'bump Type<'bump>,
    ) -> ControlFlow<&'bump Type<'bump>> {
        ProcessAlias.edit(bump, self, ty)?;
        ProcessTuple.edit(bump, self, ty)?;

        // No edits occurred
        Continue(())
    }
}

trait Editor<'bump> {
    fn edit(
        &mut self,
        bump: &'bump Bump,
        ctx: &mut Ctx<'bump>,
        ty: &'bump Type<'bump>,
    ) -> ControlFlow<&'bump Type<'bump>>;
}

/// Functions given to [`rewrite_slice`] to access types from items and handle item removal and
/// slice cancellation.
struct RewriteFns<'bump, I> {
    type_from_item: fn(&mut I) -> &mut &'bump Type<'bump>,
    remove_item_if: fn(&I) -> bool,
    replace_whole_type_with: fn(&I) -> Option<&'bump Type<'bump>>,
}

/// Rewrites types in a slice of type-containing items, optionally removing items or replacing the
/// containing type.
fn rewrite_slice<'bump, I: Copy + 'bump>(
    bump: &'bump Bump,
    ctx: &mut Ctx<'bump>,
    items: &[I],
    fns: RewriteFns<'bump, I>,
) -> ControlFlow<&'bump Type<'bump>, Option<BumpVec<'bump, I>>> {
    // Quick check for replacement from initial types.
    for item in items {
        if let Some(replacement) = (fns.replace_whole_type_with)(item) {
            return Break(replacement);
        }
    }

    // Lazily rewrite items.
    // Does not produce a new vector if the item's types haven't changed.
    let mut new_items: Option<BumpVec<'_, I>> = None;
    for (i, mut item) in items.iter().copied().enumerate() {
        let changed = ctx.process_type(bump, (fns.type_from_item)(&mut item));

        if let Some(replacement) = (fns.replace_whole_type_with)(&item) {
            return Break(replacement);
        }

        let remove_item = (fns.remove_item_if)(&item);

        if !changed && !remove_item {
            if let Some(ref mut new_items) = new_items {
                new_items.push(item);
            }

            continue;
        }

        // Make a copy of the items if it hasn't been copied yet.
        let new_items = new_items.get_or_insert_with(|| {
            // Copy unchanged items up to this point
            let mut vec = BumpVec::with_capacity_in(items.len(), bump);
            vec.extend_from_slice(&items[..i]);
            vec
        });

        if !remove_item {
            new_items.push(item);
        }
    }

    Continue(new_items)
}

/// Gathers information about type aliases.
///
/// Non-recursive type aliases are replaced with the referenced type.
/// Recursive aliases are not rewritten, since the final type information is
/// looked up from context.
struct ProcessAlias;

impl<'bump> Editor<'bump> for ProcessAlias {
    fn edit(
        &mut self,
        bump: &'bump Bump,
        ctx: &mut Ctx<'bump>,
        ty: &'bump Type<'bump>,
    ) -> ControlFlow<&'bump Type<'bump>> {
        let Type::Alias { name, id, ty } = *ty else { return Continue(()) };
        let id = id();
        let mut ty = ty.into();

        // Check the current chain of type aliases being visited.
        // If this type alias is already in the chain, mark it as recursive.
        if let Some(chain) = ctx.alias.current.as_mut().and_then(|chain| chain.search(id)) {
            chain.recursive = true;
            return Continue(());
        }

        // Fully processed type aliases are recorded in the type alias map.
        let info = if let Some(info) = ctx.alias.info.get(&id) {
            info
        } else {
            // Process the type alias' inner type.

            // Add this alias to the current chain so it isn't processed recursively.
            let mut chain =
                bump.alloc(AliasChain { prev: ctx.alias.current.take(), id, recursive: false });
            ctx.alias.current = Some(chain);

            ctx.process_type(bump, &mut ty);

            // Then remove the alias from the chain and add it to the type alias map.
            chain = ctx.alias.current.take().unwrap();
            ctx.alias.current = chain.prev.take();

            let hash_map::Entry::Vacant(entry) = ctx.alias.info.entry(id) else {
                unreachable!(
                    "Alias info was filled out reentrantly for {}{id:?} which is not possible",
                    name()
                );
            };

            entry.insert(AliasInfo { ty, recursive: chain.recursive })
        };

        // Non-recursive type aliases can be replaced with their inner type.
        if info.recursive {
            Continue(())
        } else {
            Break(info.ty)
        }
    }
}

/// Processes tuples by rewriting field types.
///
/// Tuples containing required `Type::Void` fields replaced with `Type::Void`, since it will never
/// match against a value.
struct ProcessTuple;

impl<'bump> Editor<'bump> for ProcessTuple {
    fn edit(
        &mut self,
        bump: &'bump Bump,
        ctx: &mut Ctx<'bump>,
        ty: &'bump Type<'bump>,
    ) -> ControlFlow<&'bump Type<'bump>> {
        let Type::Tuple { mut fields } = *ty else { return Continue(()) };

        let new_fields = rewrite_slice::<&'bump Type<'bump>>(
            bump,
            ctx,
            fields,
            RewriteFns {
                type_from_item: |ty| ty,
                remove_item_if: |_| false,
                replace_whole_type_with: |ty| {
                    // Check for void fields, which cannot be parsed.
                    if matches!(ty, Type::Void) {
                        Some(&Type::Void)
                    } else {
                        None
                    }
                },
            },
        )?;

        let changed = new_fields.is_some();

        if let Some(new_fields) = new_fields {
            fields = new_fields.into_bump_slice();
        }

        if changed {
            // Allocate new type
            Break(bump.alloc(Type::Tuple { fields }))
        } else {
            // Keep original type
            Continue(())
        }
    }
}

#[cfg(test)]
mod test {
    use ffx_validation_proc_macro::schema;

    use crate::schema::{Nothing, Schema, ValueType};

    use super::*;

    struct SimpleAlias;

    schema! {
        type SimpleAlias = u32;
    }

    #[test]
    fn test_simple_alias() {
        let bump = Bump::new();
        let mut ctx = Ctx::new();
        let mut ty = SimpleAlias::TYPE;
        let changed = ctx.process_type(&bump, &mut ty);
        eprintln!("Type changed? {changed:?}\nType is now {ty:?}");

        assert!(changed);
        assert!(matches!(ty, Type::Type { ty: ValueType::Integer }));
    }

    #[test]
    fn test_recursive_alias() {
        struct RecursiveAlias;

        let bump = Bump::new();
        let mut ctx = Ctx::new();

        // TODO(https://fxbug.dev/323967119): Replace this with schema macro once recursive type
        // aliases are fixed.
        // Equivalent to `schema! { type RecursiveAlias = (json::Any, RecursiveAlias);`
        static SCHEMA: Type<'static> = Type::Alias {
            name: std::any::type_name::<RecursiveAlias>,
            id: std::any::TypeId::of::<RecursiveAlias>,
            ty: &Type::Tuple { fields: &[&Type::Any, &SCHEMA] },
        };

        let mut ty = &SCHEMA;

        let changed = ctx.process_type(&bump, &mut ty);
        eprintln!("Type changed? {changed:?}\nType is now {ty:?}");

        // Since the alias contains itself, the alias should not be removed.
        assert!(!changed);
        assert!(std::ptr::eq(ty, &SCHEMA));
    }

    #[test]
    fn test_json_null_value_to_value_type_null() {
        let bump = Bump::new();
        let mut ctx = Ctx::new();
        let mut ty = &Type::Constant { value: &InlineValue::Null };
        let changed = ctx.process_type(&bump, &mut ty);
        eprintln!("Type changed? {changed:?}\nType is now {ty:?}");

        assert!(changed);
        assert!(matches!(ty, Type::Type { ty: ValueType::Null }));
    }

    #[test]
    fn test_empty_tuple() {
        let bump = Bump::new();
        let mut ctx = Ctx::new();
        let mut ty = &Type::Tuple { fields: &[] };
        let changed = ctx.process_type(&bump, &mut ty);
        eprintln!("Type changed? {changed:?}\nType is now {ty:?}");

        assert!(!changed);
        assert!(matches!(ty, Type::Tuple { fields: [] }));
    }

    #[test]
    fn test_tuple_with_void_to_void() {
        struct Tuple;
        schema! {
            type Tuple = (u32, Nothing);
        }

        let bump = Bump::new();
        let mut ctx = Ctx::new();
        let mut ty = Tuple::TYPE;
        let changed = ctx.process_type(&bump, &mut ty);
        eprintln!("Type changed? {changed:?}\nType is now {ty:?}");

        assert!(changed);
        assert!(matches!(ty, Type::Void));
    }

    #[test]
    fn test_tuple_simplifies_field() {
        struct Tuple;
        schema! {
            type Tuple = (u32, SimpleAlias);
        }

        let bump = Bump::new();
        let mut ctx = Ctx::new();
        let mut ty = Tuple::TYPE;
        let changed = ctx.process_type(&bump, &mut ty);
        eprintln!("Type changed? {changed:?}\nType is now {ty:?}");

        assert!(changed);
        assert!(matches!(
            ty,
            Type::Tuple {
                fields: [
                    Type::Type { ty: ValueType::Integer },
                    Type::Type { ty: ValueType::Integer },
                ]
            }
        ));
    }
}
