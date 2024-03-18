// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{bpf::map::Map, task::CurrentTask};
use ebpf::{EbpfHelper, EpbfRunContext, FunctionSignature, Type};
use once_cell::sync::Lazy;
use starnix_logging::track_stub;
use starnix_sync::{BpfHelperOps, Locked};
use std::sync::Arc;

pub struct HelperFunctionContext<'a> {
    pub locked: &'a mut Locked<'a, BpfHelperOps>,
    pub _current_task: &'a CurrentTask,
}

pub enum HelperFunctionContextMarker {}
impl EpbfRunContext for HelperFunctionContextMarker {
    type Context<'a> = HelperFunctionContext<'a>;
}

const MAP_LOOKUP_ELEM_INDEX: u32 = 1;
const MAP_LOOKUP_ELEM_NAME: &'static str = "map_lookup_elem";

fn bpf_map_lookup_elem(
    context: &mut HelperFunctionContext<'_>,
    map: *mut u8,
    key: *mut u8,
    _: *mut u8,
    _: *mut u8,
    _: *mut u8,
) -> *mut u8 {
    let map: &Map = unsafe { &*(map as *const Map) };
    let key = unsafe { std::slice::from_raw_parts(key as *const u8, map.schema.key_size as usize) };

    let key = key.to_owned();
    map.get_raw(context.locked, &key).map(|v| v as *mut u8).unwrap_or(0 as *mut u8)
}

const MAP_UPDATE_ELEM_INDEX: u32 = 2;
const MAP_UPDATE_ELEM_NAME: &'static str = "map_update_elem";

fn bpf_map_update_elem(
    context: &mut HelperFunctionContext<'_>,
    map: *mut u8,
    key: *mut u8,
    value: *mut u8,
    flags: *mut u8,
    _: *mut u8,
) -> *mut u8 {
    let map: &Map = unsafe { &*(map as *const Map) };
    let key = unsafe { std::slice::from_raw_parts(key, map.schema.key_size as usize) };
    let value = unsafe { std::slice::from_raw_parts(value, map.schema.value_size as usize) };
    let flags = flags as u64;

    let key = key.to_owned();
    map.update(context.locked, key, value, flags).map(|_| 0).unwrap_or(u64::MAX) as *mut u8
}

const MAP_DELETE_ELEM_INDEX: u32 = 3;
const MAP_DELETE_ELEM_NAME: &'static str = "map_delete_elem";

fn bpf_map_delete_elem(
    _context: &mut HelperFunctionContext<'_>,
    _map: *mut u8,
    _key: *mut u8,
    _: *mut u8,
    _: *mut u8,
    _: *mut u8,
) -> *mut u8 {
    track_stub!(TODO("https://fxbug.dev/287120494"), "bpf_map_delete_elem");
    u64::MAX as *mut u8
}

pub static BPF_HELPERS: Lazy<Vec<EbpfHelper<HelperFunctionContextMarker>>> = Lazy::new(|| {
    vec![
        EbpfHelper {
            index: MAP_LOOKUP_ELEM_INDEX,
            name: MAP_LOOKUP_ELEM_NAME,
            function_pointer: Arc::new(bpf_map_lookup_elem),
            signature: FunctionSignature {
                args: &[Type::ConstPtrToMapParameter, Type::MapKeyParameter { map_ptr_index: 0 }],
                return_value: Type::NullOr(Box::new(Type::MapValueParameter { map_ptr_index: 0 })),
            },
        },
        EbpfHelper {
            index: MAP_UPDATE_ELEM_INDEX,
            name: MAP_UPDATE_ELEM_NAME,
            function_pointer: Arc::new(bpf_map_update_elem),
            signature: FunctionSignature {
                args: &[
                    Type::ConstPtrToMapParameter,
                    Type::MapKeyParameter { map_ptr_index: 0 },
                    Type::MapValueParameter { map_ptr_index: 0 },
                    Type::ScalarValueParameter,
                ],
                return_value: Type::unknown_written_scalar_value(),
            },
        },
        EbpfHelper {
            index: MAP_DELETE_ELEM_INDEX,
            name: MAP_DELETE_ELEM_NAME,
            function_pointer: Arc::new(bpf_map_delete_elem),
            signature: FunctionSignature {
                args: &[Type::ConstPtrToMapParameter, Type::MapKeyParameter { map_ptr_index: 0 }],
                return_value: Type::unknown_written_scalar_value(),
            },
        },
    ]
});
