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

use crate::schema::{Field, InlineValue, StructExtras, Type, ValueType};

use self::compare::{
    cmp_processed_types, is_sorted_and_deduped_by, is_sorted_by, struct_field_ord,
};

mod compare;

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
    sorted: bool,
}

impl<'bump> Ctx<'bump> {
    pub fn new() -> Self {
        Self { alias: AliasCtx { current: None, info: HashMap::new() }, sorted: true }
    }

    /// Returns all recursive type aliases in a sorted list of identifier -> type pairs and a map
    /// from alias [`TypeId`] to the respective identifier.
    pub fn sorted_aliases(
        &self,
        bump: &'bump Bump,
    ) -> (Vec<(&'bump str, &'bump Type<'bump>)>, HashMap<TypeId, &'bump str>) {
        // Sort and filter aliases.
        // Only non-recursive aliases are reachable within any simplified type.
        let sorted = self.alias.info.iter().filter(|(_, info)| info.recursive).collect::<Vec<_>>();
        // TODO: Sort when sorting code is available
        // sorted.sort_by(|(_, lhs), (_, rhs)| cmp_processed_types(lhs.ty, rhs.ty));

        let mut alias_id_map = HashMap::new();

        let alias_types = sorted
            .into_iter()
            .enumerate()
            .map(|(i, (id, info))| {
                // Alias identifiers are currently based off of index. In the future this could utilize
                // some sort of stable type hash to reduce churn when a type alias changes.
                let alias_id = bumpalo::format!(in bump, "{i:04X}",).into_bump_str();

                alias_id_map.insert(*id, alias_id);
                (alias_id, info.ty)
            })
            .collect();

        (alias_types, alias_id_map)
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
        ProcessUnion.edit(bump, self, ty)?;
        ProcessArray.edit(bump, self, ty)?;
        ProcessMap.edit(bump, self, ty)?;
        ProcessStruct.edit(bump, self, ty)?;
        DesugarEnum.edit(bump, self, ty)?;

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

/// Processes union types by removing redundancies and rewriting inner types.
///
/// Unions containing `Type::Any` type are replaced with `Type::Any`, since they're functionally
/// equivalent.
///
/// `Type::Void` union variants are removed, since they'll never match.
///
/// Empty unions are replaced with `Type::Void` since they'll never match against a value.
///
/// If requested, union fields are sorted and deduplicated by type.
struct ProcessUnion;

impl<'bump> Editor<'bump> for ProcessUnion {
    fn edit(
        &mut self,
        bump: &'bump Bump,
        ctx: &mut Ctx<'bump>,
        ty: &'bump Type<'bump>,
    ) -> ControlFlow<&'bump Type<'bump>> {
        let Type::Union(mut tys) = *ty else { return Continue(()) };

        let mut new_tys = rewrite_slice::<&'bump Type<'bump>>(
            bump,
            ctx,
            tys,
            RewriteFns {
                type_from_item: |ty| ty,
                remove_item_if: |ty| matches!(ty, Type::Void),
                replace_whole_type_with: |ty| {
                    // A union containing Any can be simplified to just Any.
                    if matches!(ty, Type::Any) {
                        Some(&Type::Any)
                    } else {
                        None
                    }
                },
            },
        )?;

        let compare = |lhs: &&Type<'_>, rhs: &&Type<'_>| cmp_processed_types(*lhs, *rhs);
        if ctx.sorted && !is_sorted_and_deduped_by(new_tys.as_deref().unwrap_or(tys), compare) {
            // If the union's types aren't sorted or there are duplicates, sort and dedup.
            let new_tys: &mut BumpVec<'_, &Type<'_>> =
                new_tys.get_or_insert_with(|| BumpVec::from_iter_in(tys.iter().copied(), bump));
            new_tys.sort_by(compare);
            new_tys.dedup_by(|l, r| compare(l, r).is_eq());
        }

        let changed = new_tys.is_some();

        if let Some(new_tys) = new_tys {
            tys = new_tys.into_bump_slice();
        }

        // An empty union matches nothing.
        if tys.is_empty() {
            return Break(&Type::Void);
        }

        // A union type with a single member is just the member.
        if let [single] = tys {
            return Break(*single);
        }

        if changed {
            // Allocate new type
            Break(bump.alloc(Type::Union(tys)))
        } else {
            // Keep original type
            Continue(())
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

/// Processes an array type by rewriting its element type.
///
/// Arrays of `Type::Void` with a fixed size above zero are replaced with `Type::Void`, since
/// parsing elements are impossible.
///
/// Arrays of `Type::Void` without a fixed size are given a fixed size of 0.
struct ProcessArray;

impl<'bump> Editor<'bump> for ProcessArray {
    fn edit(
        &mut self,
        bump: &'bump Bump,
        ctx: &mut Ctx<'bump>,
        ty: &'bump Type<'bump>,
    ) -> ControlFlow<&'bump Type<'bump>> {
        let Type::Array { mut size, mut ty } = *ty else { return Continue(()) };

        let mut changed = ctx.process_type(bump, &mut ty);

        if matches!(ty, Type::Void) {
            match size {
                // A non-fixed size array of void values requires a size of 0
                None => {
                    size = Some(0);
                    changed = true
                }
                Some(0) => {}
                // Requiring a size greater than 0 is impossible.
                Some(..) => return Break(&Type::Void),
            }
        }

        if changed {
            Break(bump.alloc(Type::Array { size, ty }))
        } else {
            Continue(())
        }
    }
}

/// Processes a map type by rewriting the key and value type.
///
/// If the key or value is `Type::Void` the map is replaced with a JSON constant of an empty map.
struct ProcessMap;

impl<'bump> Editor<'bump> for ProcessMap {
    fn edit(
        &mut self,
        bump: &'bump Bump,
        ctx: &mut Ctx<'bump>,
        ty: &'bump Type<'bump>,
    ) -> ControlFlow<&'bump Type<'bump>> {
        let Type::Map { mut key, mut value } = *ty else { return Continue(()) };

        let key_changed = ctx.process_type(bump, &mut key);
        let value_changed = ctx.process_type(bump, &mut value);

        let changed = key_changed || value_changed;

        if matches!(key, Type::Void) || matches!(value, Type::Void) {
            // Either the key or valid is void, replace with an empty map constant.
            return Break(&Type::Constant { value: &InlineValue::Object(&[]) });
        }

        if changed {
            // Allocate new type
            Break(bump.alloc(Type::Map { key, value }))
        } else {
            // Keep original type
            Continue(())
        }
    }
}

/// Processes structs by rewriting field types.
///
/// Structs containing required `Type::Void` fields replaced with `Type::Void`, since it will never
/// match against a value.
///
/// Optional `Type::Void` fields are removed since it will never have a value.
///
/// If requested, struct fields are sorted by name.
struct ProcessStruct;

impl<'bump> Editor<'bump> for ProcessStruct {
    fn edit(
        &mut self,
        bump: &'bump Bump,
        ctx: &mut Ctx<'bump>,
        ty: &'bump Type<'bump>,
    ) -> ControlFlow<&'bump Type<'bump>> {
        let Type::Struct { mut fields, extras } = *ty else { return Continue(()) };

        let mut new_fields = rewrite_slice(
            bump,
            ctx,
            fields,
            RewriteFns {
                type_from_item: |f| &mut f.value,
                // Optional void fields are removed since they'll never be parsed.
                remove_item_if: |f| matches!(f.value, Type::Void) && f.optional,
                replace_whole_type_with: |f| {
                    if matches!(f.value, Type::Void) && !f.optional {
                        // Required void fields make the entire struct unparsable.
                        Some(&Type::Void)
                    } else {
                        None
                    }
                },
            },
        )?;

        let extras_changed = match extras {
            None | Some(StructExtras::Deny) => false,
            Some(StructExtras::Flatten(_)) => {
                // TODO(https://fxbug.dev/316035760): Add support for struct flattening when implemented in the macro
                panic!("Simplification for struct flattening not yet supported")
            }
        };

        let compare = |l: &Field<'_>, r: &Field<'_>| struct_field_ord(l).cmp(&struct_field_ord(r));
        if ctx.sorted && !is_sorted_by(new_fields.as_deref().unwrap_or(fields), compare) {
            // Sort fields if they're not already sorted.
            let new_fields = new_fields
                .get_or_insert_with(|| BumpVec::from_iter_in(fields.iter().copied(), bump));
            new_fields.sort_by(compare);
        }

        let changed = new_fields.is_some() || extras_changed;

        if let Some(new_fields) = new_fields {
            fields = new_fields.into_bump_slice();
        }

        if changed {
            // Allocate new type
            Break(bump.alloc(Type::Struct { fields, extras }))
        } else {
            // Keep original type
            Continue(())
        }
    }
}

/// Desugars an enum to the union type that represents it.
///
/// Dataless variants are turned into a `Type::Constant` that matches against the variant's name.
///
/// Variants with data are turned into a struct in the form `struct { <name>: <data> }`.
struct DesugarEnum;

impl<'bump> Editor<'bump> for DesugarEnum {
    fn edit(
        &mut self,
        bump: &'bump Bump,
        ctx: &mut Ctx<'bump>,
        ty: &'bump Type<'bump>,
    ) -> ControlFlow<&'bump Type<'bump>> {
        let Type::Enum { variants } = *ty else { return Continue(()) };

        let mut tys = BumpVec::with_capacity_in(variants.len(), bump);

        for (name, mut ty) in variants.iter().copied() {
            ctx.process_type(bump, &mut ty);

            // Turn data-less enums into a simple string constant, otherwise use the traditional
            // serde enum format.
            if let Type::Void = ty {
                tys.push(
                    &*bump.alloc(Type::Constant { value: bump.alloc(InlineValue::String(name)) }),
                );
            } else {
                tys.push(&*bump.alloc(Type::Struct {
                    fields: bump.alloc([Field { key: name, value: ty, optional: false }]),
                    extras: Some(StructExtras::Deny),
                }));
            }
        }

        // Enum variants are not sorted, since the returned union type will be sorted once its
        // processed.
        Break(bump.alloc(Type::Union(tys.into_bump_slice())))
    }
}

#[cfg(test)]
mod test {
    use ffx_validation_proc_macro::schema;

    use crate::schema::{json, json::Any, Nothing, Schema};

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
        schema! {
            #[recursive]
            type RecursiveAlias = (json::Any, RecursiveAlias);
        }

        let bump = Bump::new();
        let mut ctx = Ctx::new();

        let mut ty = RecursiveAlias::TYPE;

        let changed = ctx.process_type(&bump, &mut ty);
        eprintln!("Type changed? {changed:?}\nType is now {ty:?}");

        // Since the alias contains itself, the alias should not be removed.
        assert!(!changed);
        assert!(std::ptr::eq(ty, RecursiveAlias::TYPE));
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
    fn test_empty_union() {
        let bump = Bump::new();
        let mut ctx = Ctx::new();
        let mut ty = &Type::Union(&[]);
        let changed = ctx.process_type(&bump, &mut ty);
        eprintln!("Type changed? {changed:?}\nType is now {ty:?}");

        assert!(changed);
        assert!(matches!(ty, Type::Void));
    }

    #[test]
    fn test_union_unchanged() {
        struct Union;
        schema! {
            // Use transparent to avoid unwrapping an alias, which would set the changed flag
            #[transparent]
            type Union = u32 | String;
        }

        let bump = Bump::new();
        let mut ctx = Ctx::new();
        let mut ty = Union::TYPE;
        let changed = ctx.process_type(&bump, &mut ty);
        eprintln!("Type changed? {changed:?}\nType is now {ty:?}");

        assert!(!changed);
        assert!(matches!(
            ty,
            Type::Union([
                Type::Type { ty: ValueType::Integer },
                Type::Type { ty: ValueType::String }
            ])
        ));
    }

    #[test]
    fn test_union_remove_void() {
        struct Union;
        schema! {
            type Union = Nothing | u32 | String;
        }

        let bump = Bump::new();
        let mut ctx = Ctx::new();
        let mut ty = Union::TYPE;
        let changed = ctx.process_type(&bump, &mut ty);
        eprintln!("Type changed? {changed:?}\nType is now {ty:?}");

        assert!(changed);
        assert!(matches!(
            ty,
            Type::Union([
                Type::Type { ty: ValueType::Integer },
                Type::Type { ty: ValueType::String }
            ])
        ));
    }

    #[test]
    fn test_union_to_any() {
        struct Union;
        schema! {
            type Union = Nothing | Any | String;
        }

        let bump = Bump::new();
        let mut ctx = Ctx::new();
        let mut ty = Union::TYPE;
        let changed = ctx.process_type(&bump, &mut ty);
        eprintln!("Type changed? {changed:?}\nType is now {ty:?}");

        assert!(changed);
        assert!(matches!(ty, Type::Any));
    }

    #[test]
    fn test_union_single_type() {
        struct Union;
        schema! {
            type Union = Nothing | String;
        }

        let bump = Bump::new();
        let mut ctx = Ctx::new();
        let mut ty = Union::TYPE;
        let changed = ctx.process_type(&bump, &mut ty);
        eprintln!("Type changed? {changed:?}\nType is now {ty:?}");

        assert!(changed);
        assert!(matches!(ty, Type::Type { ty: ValueType::String }));
    }

    #[test]
    fn test_union_sorting() {
        struct Union;
        schema! {
            type Union = String | u32 | struct { a: String } | struct { a: u32 };
        }

        let bump = Bump::new();
        let mut ctx = Ctx::new();
        let mut ty = Union::TYPE;
        let changed = ctx.process_type(&bump, &mut ty);
        eprintln!("Type changed? {changed:?}\nType is now {ty:?}");

        assert!(changed);
        assert!(matches!(
            ty,
            Type::Union([
                Type::Type { ty: ValueType::Integer },
                Type::Type { ty: ValueType::String },
                Type::Struct {
                    fields: [Field {
                        key: "a",
                        value: Type::Type { ty: ValueType::Integer },
                        optional: false,
                    }],
                    extras: None,
                },
                Type::Struct {
                    fields: [Field {
                        key: "a",
                        value: Type::Type { ty: ValueType::String },
                        optional: false,
                    }],
                    extras: None,
                },
            ])
        ));
    }

    #[test]
    fn test_union_deduplication() {
        struct Union;
        schema! {
            type Union = String | u32 | String | SimpleAlias;
        }

        let bump = Bump::new();
        let mut ctx = Ctx::new();
        let mut ty = Union::TYPE;
        let changed = ctx.process_type(&bump, &mut ty);
        eprintln!("Type changed? {changed:?}\nType is now {ty:?}");

        assert!(changed);
        assert!(matches!(
            ty,
            Type::Union([
                Type::Type { ty: ValueType::Integer },
                Type::Type { ty: ValueType::String },
            ])
        ));
    }

    #[test]
    fn test_map_with_void_key_into_empty_map() {
        let bump = Bump::new();
        let mut ctx = Ctx::new();
        let mut ty = &Type::Map { key: &Type::Void, value: u32::TYPE };
        let changed = ctx.process_type(&bump, &mut ty);
        eprintln!("Type changed? {changed:?}\nType is now {ty:?}");

        assert!(changed);
        assert!(matches!(ty, Type::Constant { value: InlineValue::Object([]) }));
    }

    #[test]
    fn test_map_with_void_value_into_empty_map() {
        let bump = Bump::new();
        let mut ctx = Ctx::new();
        let mut ty = &Type::Map { key: u32::TYPE, value: &Type::Void };
        let changed = ctx.process_type(&bump, &mut ty);
        eprintln!("Type changed? {changed:?}\nType is now {ty:?}");

        assert!(changed);
        assert!(matches!(ty, Type::Constant { value: InlineValue::Object([]) }));
    }

    #[test]
    fn test_map_simplifies_element_type() {
        let bump = Bump::new();
        let mut ctx = Ctx::new();
        let mut ty = &Type::Map { key: SimpleAlias::TYPE, value: u32::TYPE };
        let changed = ctx.process_type(&bump, &mut ty);
        eprintln!("Type changed? {changed:?}\nType is now {ty:?}");

        assert!(changed);
        assert!(matches!(
            ty,
            Type::Map {
                key: Type::Type { ty: ValueType::Integer },
                value: Type::Type { ty: ValueType::Integer }
            }
        ));
    }

    #[test]
    fn test_array_of_void_to_zero_size() {
        struct Array;
        schema! {
            type Array = [Nothing];
        }

        let bump = Bump::new();
        let mut ctx = Ctx::new();
        let mut ty = Array::TYPE;
        let changed = ctx.process_type(&bump, &mut ty);
        eprintln!("Type changed? {changed:?}\nType is now {ty:?}");

        assert!(changed);
        assert!(matches!(ty, Type::Array { size: Some(0), ty: Type::Void }));
    }

    #[test]
    fn test_fixed_size_array_of_void_to_void() {
        struct Array;
        schema! {
            type Array = [Nothing; 3];
        }

        let bump = Bump::new();
        let mut ctx = Ctx::new();
        let mut ty = Array::TYPE;
        let changed = ctx.process_type(&bump, &mut ty);
        eprintln!("Type changed? {changed:?}\nType is now {ty:?}");

        assert!(changed);
        assert!(matches!(ty, Type::Void));
    }

    #[test]
    fn test_array_simplifies_element_type() {
        struct Array;
        schema! {
            type Array = [SimpleAlias];
        }

        let bump = Bump::new();
        let mut ctx = Ctx::new();
        let mut ty = Array::TYPE;
        let changed = ctx.process_type(&bump, &mut ty);
        eprintln!("Type changed? {changed:?}\nType is now {ty:?}");

        assert!(changed);
        assert!(matches!(
            ty,
            Type::Array { size: None, ty: Type::Type { ty: ValueType::Integer } }
        ));
    }

    #[test]
    fn test_simplify_struct() {
        struct StructWithAliases;

        schema! {
            type StructWithAliases = struct {
                field_a: u32,
                field_b: SimpleAlias,
            };
        }

        let bump = Bump::new();
        let mut ctx = Ctx::new();
        let mut ty = StructWithAliases::TYPE;
        let changed = ctx.process_type(&bump, &mut ty);
        eprintln!("Type changed? {changed:?}\nType is now {ty:?}");

        assert!(changed);
        assert!(matches!(
            ty,
            Type::Struct {
                fields: [
                    Field {
                        key: "field_a",
                        value: Type::Type { ty: ValueType::Integer },
                        optional: false
                    },
                    Field {
                        key: "field_b",
                        value: Type::Type { ty: ValueType::Integer },
                        optional: false
                    }
                ],
                extras: None
            }
        ));
    }

    #[test]
    fn test_struct_with_void_required_field_to_void() {
        struct Struct;
        schema! {
            type Struct = struct {
                field: u32,
                required_nothing: Nothing,
            };
        }

        let bump = Bump::new();
        let mut ctx = Ctx::new();
        let mut ty = Struct::TYPE;
        let changed = ctx.process_type(&bump, &mut ty);
        eprintln!("Type changed? {changed:?}\nType is now {ty:?}");

        assert!(changed);
        assert!(matches!(ty, Type::Void));
    }

    #[test]
    fn test_struct_remove_void_optional_field() {
        struct Struct;
        schema! {
            type Struct = struct {
                field: u32,
                optional_nothing?: Nothing,
            };
        }

        let bump = Bump::new();
        let mut ctx = Ctx::new();
        let mut ty = Struct::TYPE;
        let changed = ctx.process_type(&bump, &mut ty);
        eprintln!("Type changed? {changed:?}\nType is now {ty:?}");

        assert!(changed);
        assert!(matches!(
            ty,
            Type::Struct {
                fields: [Field {
                    key: "field",
                    value: Type::Type { ty: ValueType::Integer },
                    optional: false
                }],
                extras: None
            }
        ));
    }

    #[test]
    fn test_struct_sort_fields() {
        struct Struct;
        schema! {
            type Struct = struct {
                c: u32,
                b: u32,
                a: u32,
            };
        }

        let bump = Bump::new();
        let mut ctx = Ctx::new();
        let mut ty = Struct::TYPE;
        let changed = ctx.process_type(&bump, &mut ty);
        eprintln!("Type changed? {changed:?}\nType is now {ty:?}");

        assert!(changed);
        assert!(matches!(
            ty,
            Type::Struct {
                fields: [
                    Field {
                        key: "a",
                        value: Type::Type { ty: ValueType::Integer },
                        optional: false
                    },
                    Field {
                        key: "b",
                        value: Type::Type { ty: ValueType::Integer },
                        optional: false
                    },
                    Field {
                        key: "c",
                        value: Type::Type { ty: ValueType::Integer },
                        optional: false
                    },
                ],
                extras: None
            }
        ));
    }

    #[test]
    fn test_empty_enum_to_void() {
        struct Enum;
        schema! {
            type Enum = enum {};
        }

        let bump = Bump::new();
        let mut ctx = Ctx::new();
        let mut ty = Enum::TYPE;
        let changed = ctx.process_type(&bump, &mut ty);
        eprintln!("Type changed? {changed:?}\nType is now {ty:?}");

        assert!(changed);
        assert!(matches!(ty, Type::Void));
    }

    #[test]
    fn test_enum_to_union() {
        struct Enum;
        schema! {
            type Enum = enum {
                A, B, C(String), D { a: u32, b: u32 },
            };
        }

        let bump = Bump::new();
        let mut ctx = Ctx::new();

        // Disable sorting to keep the original union order.
        ctx.sorted = false;

        let mut ty = Enum::TYPE;
        let changed = ctx.process_type(&bump, &mut ty);
        eprintln!("Type changed? {changed:?}\nType is now {ty:?}");

        assert!(changed);
        assert!(matches!(
            ty,
            Type::Union([
                Type::Constant { value: InlineValue::String("A") },
                Type::Constant { value: InlineValue::String("B") },
                Type::Struct {
                    fields: [Field {
                        key: "C",
                        value: Type::Type { ty: ValueType::String },
                        optional: false,
                    }],
                    extras: Some(StructExtras::Deny)
                },
                Type::Struct {
                    fields: [Field {
                        key: "D",
                        value: Type::Struct {
                            fields: [
                                Field {
                                    key: "a",
                                    value: Type::Type { ty: ValueType::Integer },
                                    optional: false,
                                },
                                Field {
                                    key: "b",
                                    value: Type::Type { ty: ValueType::Integer },
                                    optional: false,
                                }
                            ],
                            extras: None
                        },
                        optional: false,
                    }],
                    extras: Some(StructExtras::Deny)
                },
            ])
        ));
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
