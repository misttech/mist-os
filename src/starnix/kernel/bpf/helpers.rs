// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use starnix_logging::track_stub;
use ubpf::{BpfHelper, FunctionSignature, Type};

const MAP_LOOKUP_ELEM_INDEX: u32 = 1;
const MAP_LOOKUP_ELEM_NAME: &'static str = "map_lookup_elem";

extern "C" fn bpf_map_lookup_elem(
    _map: *const std::os::raw::c_void,
    _key: *const std::os::raw::c_void,
) -> *const std::os::raw::c_void {
    track_stub!(TODO("https://fxbug.dev/287120494"), "bpf_map_lookup_elem");
    std::ptr::null()
}

const MAP_UPDATE_ELEM_INDEX: u32 = 2;
const MAP_UPDATE_ELEM_NAME: &'static str = "map_update_elem";

extern "C" fn bpf_map_update_elem(
    _map: *const std::os::raw::c_void,
    _key: *const std::os::raw::c_void,
    _value: *const std::os::raw::c_void,
    _flags: u64,
) -> std::os::raw::c_long {
    track_stub!(TODO("https://fxbug.dev/287120494"), "bpf_map_update_elem");
    -1
}

const MAP_DELETE_ELEM_INDEX: u32 = 3;
const MAP_DELETE_ELEM_NAME: &'static str = "map_delete_elem";

extern "C" fn bpf_map_delete_elem(
    _map: *const std::os::raw::c_void,
    _key: *const std::os::raw::c_void,
) -> std::os::raw::c_long {
    track_stub!(TODO("https://fxbug.dev/287120494"), "bpf_map_delete_elem");
    -1
}

pub static BPF_HELPERS: &'static [BpfHelper] = &[
    BpfHelper {
        index: MAP_LOOKUP_ELEM_INDEX,
        name: MAP_LOOKUP_ELEM_NAME,
        function_pointer: bpf_map_lookup_elem as *mut std::os::raw::c_void,
        signature: FunctionSignature {
            args: &[Type::ConstPtrToMap, Type::ConstMapKey { map_ptr_index: 0 }],
            return_value: Type::NullOr(&Type::ConstMapValue { map_ptr_index: 0 }),
        },
    },
    BpfHelper {
        index: MAP_UPDATE_ELEM_INDEX,
        name: MAP_UPDATE_ELEM_NAME,
        function_pointer: bpf_map_update_elem as *mut std::os::raw::c_void,
        signature: FunctionSignature {
            args: &[
                Type::ConstPtrToMap,
                Type::ConstMapKey { map_ptr_index: 0 },
                Type::ConstMapValue { map_ptr_index: 0 },
                Type::ScalarValue,
            ],
            return_value: Type::ScalarValue,
        },
    },
    BpfHelper {
        index: MAP_DELETE_ELEM_INDEX,
        name: MAP_DELETE_ELEM_NAME,
        function_pointer: bpf_map_delete_elem as *mut std::os::raw::c_void,
        signature: FunctionSignature {
            args: &[Type::ConstPtrToMap, Type::ConstMapKey { map_ptr_index: 0 }],
            return_value: Type::ScalarValue,
        },
    },
];
