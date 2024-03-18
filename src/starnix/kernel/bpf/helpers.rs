// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use ebpf::{EbpfHelper, FunctionSignature, Type};
use once_cell::sync::Lazy;
use starnix_logging::track_stub;
use std::sync::Arc;

const MAP_LOOKUP_ELEM_INDEX: u32 = 1;
const MAP_LOOKUP_ELEM_NAME: &'static str = "map_lookup_elem";

fn bpf_map_lookup_elem(
    _map: *mut u8,
    _key: *mut u8,
    _: *mut u8,
    _: *mut u8,
    _: *mut u8,
) -> *mut u8 {
    track_stub!(TODO("https://fxbug.dev/287120494"), "bpf_map_lookup_elem");
    0 as *mut u8
}

const MAP_UPDATE_ELEM_INDEX: u32 = 2;
const MAP_UPDATE_ELEM_NAME: &'static str = "map_update_elem";

fn bpf_map_update_elem(
    _map: *mut u8,
    _key: *mut u8,
    _value: *mut u8,
    _flags: *mut u8,
    _: *mut u8,
) -> *mut u8 {
    track_stub!(TODO("https://fxbug.dev/287120494"), "bpf_map_update_elem");
    u64::MAX as *mut u8
}

const MAP_DELETE_ELEM_INDEX: u32 = 3;
const MAP_DELETE_ELEM_NAME: &'static str = "map_delete_elem";

fn bpf_map_delete_elem(
    _map: *mut u8,
    _key: *mut u8,
    _: *mut u8,
    _: *mut u8,
    _: *mut u8,
) -> *mut u8 {
    track_stub!(TODO("https://fxbug.dev/287120494"), "bpf_map_delete_elem");
    u64::MAX as *mut u8
}

pub static BPF_HELPERS: Lazy<Vec<EbpfHelper>> = Lazy::new(|| {
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
