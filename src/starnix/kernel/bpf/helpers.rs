// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::bpf::map::{Map, RingBufferWakeupPolicy};
use crate::bpf::program::ProgramType;
use crate::task::CurrentTask;
use ebpf::{
    new_bpf_type_identifier, BpfValue, EbpfHelper, EbpfRunContext, FieldDescriptor, FieldMapping,
    FieldType, FunctionSignature, MemoryId, MemoryParameterSize, StructDescriptor, StructMapping,
    Type,
};
use linux_uapi::{
    __sk_buff, bpf_flow_keys, bpf_func_id_BPF_FUNC_csum_update,
    bpf_func_id_BPF_FUNC_get_current_uid_gid, bpf_func_id_BPF_FUNC_get_socket_cookie,
    bpf_func_id_BPF_FUNC_get_socket_uid, bpf_func_id_BPF_FUNC_ktime_get_boot_ns,
    bpf_func_id_BPF_FUNC_ktime_get_ns, bpf_func_id_BPF_FUNC_l3_csum_replace,
    bpf_func_id_BPF_FUNC_l4_csum_replace, bpf_func_id_BPF_FUNC_map_delete_elem,
    bpf_func_id_BPF_FUNC_map_lookup_elem, bpf_func_id_BPF_FUNC_map_update_elem,
    bpf_func_id_BPF_FUNC_probe_read_str, bpf_func_id_BPF_FUNC_redirect,
    bpf_func_id_BPF_FUNC_ringbuf_discard, bpf_func_id_BPF_FUNC_ringbuf_reserve,
    bpf_func_id_BPF_FUNC_ringbuf_submit, bpf_func_id_BPF_FUNC_skb_adjust_room,
    bpf_func_id_BPF_FUNC_skb_change_head, bpf_func_id_BPF_FUNC_skb_change_proto,
    bpf_func_id_BPF_FUNC_skb_load_bytes_relative, bpf_func_id_BPF_FUNC_skb_pull_data,
    bpf_func_id_BPF_FUNC_skb_store_bytes, bpf_func_id_BPF_FUNC_trace_printk, bpf_sock,
    bpf_sock_addr, bpf_sockopt, bpf_user_pt_regs_t, fuse_bpf_arg, fuse_bpf_args,
    fuse_entry_bpf_out, fuse_entry_out, uref, xdp_md,
};
use once_cell::sync::Lazy;
use starnix_logging::track_stub;
use starnix_sync::{BpfHelperOps, Locked};
use std::collections::HashSet;
use std::sync::Arc;
use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout};

pub struct HelperFunctionContext<'a> {
    pub locked: &'a mut Locked<'a, BpfHelperOps>,
    pub current_task: &'a CurrentTask,
}

#[derive(Debug)]
pub enum HelperFunctionContextMarker {}
impl EbpfRunContext for HelperFunctionContextMarker {
    type Context<'a> = HelperFunctionContext<'a>;
}

const MAP_LOOKUP_ELEM_NAME: &'static str = "map_lookup_elem";

fn bpf_map_lookup_elem(
    context: &mut HelperFunctionContext<'_>,
    map: BpfValue,
    key: BpfValue,
    _: BpfValue,
    _: BpfValue,
    _: BpfValue,
) -> BpfValue {
    // SAFETY
    //
    // The safety of the operation is ensured by the bpf verifier. The `map` must be a reference to
    // a `Map` object kept alive by the program itself and the key must be valid for said map.
    let map: &Map = unsafe { &*map.as_ptr::<Map>() };
    let key =
        unsafe { std::slice::from_raw_parts(key.as_ptr::<u8>(), map.schema.key_size as usize) };

    map.get_raw(context.locked, &key).map(BpfValue::from).unwrap_or_else(BpfValue::default)
}

const MAP_UPDATE_ELEM_NAME: &'static str = "map_update_elem";

fn bpf_map_update_elem(
    context: &mut HelperFunctionContext<'_>,
    map: BpfValue,
    key: BpfValue,
    value: BpfValue,
    flags: BpfValue,
    _: BpfValue,
) -> BpfValue {
    // SAFETY
    //
    // The safety of the operation is ensured by the bpf verifier. The `map` must be a reference to
    // a `Map` object kept alive by the program itself.
    let map: &Map = unsafe { &*map.as_ptr::<Map>() };
    let key =
        unsafe { std::slice::from_raw_parts(key.as_ptr::<u8>(), map.schema.key_size as usize) };
    let value =
        unsafe { std::slice::from_raw_parts(value.as_ptr::<u8>(), map.schema.value_size as usize) };
    let flags = flags.as_u64();

    let key = key.to_owned();
    map.update(context.locked, key, value, flags).map(|_| 0).unwrap_or(u64::MAX).into()
}

const MAP_DELETE_ELEM_NAME: &'static str = "map_delete_elem";

fn bpf_map_delete_elem(
    _context: &mut HelperFunctionContext<'_>,
    _map: BpfValue,
    _key: BpfValue,
    _: BpfValue,
    _: BpfValue,
    _: BpfValue,
) -> BpfValue {
    track_stub!(TODO("https://fxbug.dev/287120494"), "bpf_map_delete_elem");
    u64::MAX.into()
}

const TRACE_PRINTK: &'static str = "trace_printk";

fn bpf_trace_printk(
    _context: &mut HelperFunctionContext<'_>,
    _fmt: BpfValue,
    _fmt_size: BpfValue,
    _: BpfValue,
    _: BpfValue,
    _: BpfValue,
) -> BpfValue {
    track_stub!(TODO("https://fxbug.dev/287120494"), "bpf_trace_printk");
    0.into()
}

const KTIME_GET_NS_NAME: &'static str = "ktime_get_ns";

fn bpf_ktime_get_ns(
    _context: &mut HelperFunctionContext<'_>,
    _: BpfValue,
    _: BpfValue,
    _: BpfValue,
    _: BpfValue,
    _: BpfValue,
) -> BpfValue {
    track_stub!(TODO("https://fxbug.dev/287120494"), "bpf_ktime_get_ns");
    42.into()
}

const GET_SOCKET_UID_NAME: &'static str = "get_socket_uid";

fn bpf_get_socket_uid(
    _context: &mut HelperFunctionContext<'_>,
    _sk_buf: BpfValue,
    _: BpfValue,
    _: BpfValue,
    _: BpfValue,
    _: BpfValue,
) -> BpfValue {
    track_stub!(TODO("https://fxbug.dev/287120494"), "bpf_get_socket_uid");
    0.into()
}

const GET_CURRENT_UID_GID_NAME: &'static str = "get_current_uid_gid";

fn bpf_get_current_uid_gid(
    context: &mut HelperFunctionContext<'_>,
    _: BpfValue,
    _: BpfValue,
    _: BpfValue,
    _: BpfValue,
    _: BpfValue,
) -> BpfValue {
    let creds = context.current_task.creds();
    let uid = creds.uid as u64;
    let gid = creds.gid as u64;
    BpfValue::from(gid << 32 | uid)
}

const SKB_PULL_DATA_NAME: &'static str = "skb_pull_data";

fn bpf_skb_pull_data(
    _context: &mut HelperFunctionContext<'_>,
    _: BpfValue,
    _: BpfValue,
    _: BpfValue,
    _: BpfValue,
    _: BpfValue,
) -> BpfValue {
    track_stub!(TODO("https://fxbug.dev/287120494"), "bpf_skb_pull_data");
    0.into()
}

const RINGBUF_RESERVE_NAME: &'static str = "ringbuf_reserve";

fn bpf_ringbuf_reserve(
    context: &mut HelperFunctionContext<'_>,
    map: BpfValue,
    size: BpfValue,
    flags: BpfValue,
    _: BpfValue,
    _: BpfValue,
) -> BpfValue {
    // SAFETY
    //
    // The safety of the operation is ensured by the bpf verifier. The `map` must be a reference to
    // a `Map` object kept alive by the program itself.
    let map: &Map = unsafe { &*map.as_ptr::<Map>() };
    let size = u32::from(size);
    let flags = u64::from(flags);
    map.ringbuf_reserve(context.locked, size, flags)
        .map(BpfValue::from)
        .unwrap_or_else(|_| BpfValue::default())
}

const RINGBUF_SUBMIT_NAME: &'static str = "ringbuf_submit";

fn bpf_ringbuf_submit(
    _context: &mut HelperFunctionContext<'_>,
    data: BpfValue,
    flags: BpfValue,
    _: BpfValue,
    _: BpfValue,
    _: BpfValue,
) -> BpfValue {
    let flags = RingBufferWakeupPolicy::from(u32::from(flags));

    // SAFETY
    //
    // The safety of the operation is ensured by the bpf verifier. The data has to come from the
    // result of a reserve call.
    unsafe {
        Map::ringbuf_submit(u64::from(data), flags);
    }
    0.into()
}

const RINGBUF_DISCARD_NAME: &'static str = "ringbuf_discard";

fn bpf_ringbuf_discard(
    _context: &mut HelperFunctionContext<'_>,
    data: BpfValue,
    flags: BpfValue,
    _: BpfValue,
    _: BpfValue,
    _: BpfValue,
) -> BpfValue {
    let flags = RingBufferWakeupPolicy::from(u32::from(flags));

    // SAFETY
    //
    // The safety of the operation is ensured by the bpf verifier. The data has to come from the
    // result of a reserve call.
    unsafe {
        Map::ringbuf_discard(u64::from(data), flags);
    }
    0.into()
}

const SKB_CHANGE_PROTO_NAME: &'static str = "skb_change_proto";

fn bpf_skb_change_proto(
    _context: &mut HelperFunctionContext<'_>,
    _: BpfValue,
    _: BpfValue,
    _: BpfValue,
    _: BpfValue,
    _: BpfValue,
) -> BpfValue {
    track_stub!(TODO("https://fxbug.dev/287120494"), "bpf_skb_change_proto");
    0.into()
}

const CSUM_UPDATE_NAME: &'static str = "csum_update";

fn bpf_csum_update(
    _context: &mut HelperFunctionContext<'_>,
    _: BpfValue,
    _: BpfValue,
    _: BpfValue,
    _: BpfValue,
    _: BpfValue,
) -> BpfValue {
    track_stub!(TODO("https://fxbug.dev/287120494"), "bpf_csum_update");
    0.into()
}

const PROBE_READ_STR_NAME: &'static str = "probe_read_str";

fn bpf_probe_read_str(
    _context: &mut HelperFunctionContext<'_>,
    _: BpfValue,
    _: BpfValue,
    _: BpfValue,
    _: BpfValue,
    _: BpfValue,
) -> BpfValue {
    track_stub!(TODO("https://fxbug.dev/287120494"), "bpf_probe_read_str");
    0.into()
}

const GET_SOCKET_COOKIE_NAME: &'static str = "get_socket_cookie";

fn bpf_get_socket_cookie_sk_buf(
    _context: &mut HelperFunctionContext<'_>,
    _: BpfValue,
    _: BpfValue,
    _: BpfValue,
    _: BpfValue,
    _: BpfValue,
) -> BpfValue {
    track_stub!(TODO("https://fxbug.dev/287120494"), "bpf_get_socket_cookie");
    0.into()
}

fn bpf_get_socket_cookie_bpf_sock(
    _context: &mut HelperFunctionContext<'_>,
    _: BpfValue,
    _: BpfValue,
    _: BpfValue,
    _: BpfValue,
    _: BpfValue,
) -> BpfValue {
    track_stub!(TODO("https://fxbug.dev/287120494"), "bpf_get_socket_cookie");
    0.into()
}

const REDIRECT_NAME: &'static str = "redirect";

fn bpf_redirect(
    _context: &mut HelperFunctionContext<'_>,
    _: BpfValue,
    _: BpfValue,
    _: BpfValue,
    _: BpfValue,
    _: BpfValue,
) -> BpfValue {
    track_stub!(TODO("https://fxbug.dev/287120494"), "bpf_redirect");
    0.into()
}

const SKB_ADJUST_ROOM_NAME: &'static str = "skb_adjust_room";

fn bpf_skb_adjust_room(
    _context: &mut HelperFunctionContext<'_>,
    _: BpfValue,
    _: BpfValue,
    _: BpfValue,
    _: BpfValue,
    _: BpfValue,
) -> BpfValue {
    track_stub!(TODO("https://fxbug.dev/287120494"), "bpf_skb_adjust_room");
    0.into()
}

const SKB_STORE_BYTES: &'static str = "skb_store_bytes";

fn bpf_skb_store_bytes(
    _context: &mut HelperFunctionContext<'_>,
    _: BpfValue,
    _: BpfValue,
    _: BpfValue,
    _: BpfValue,
    _: BpfValue,
) -> BpfValue {
    track_stub!(TODO("https://fxbug.dev/287120494"), "bpf_skb_store_bytes");
    0.into()
}

const SKB_CHANGE_HEAD: &'static str = "skb_change_head";

fn bpf_skb_change_head(
    _context: &mut HelperFunctionContext<'_>,
    _: BpfValue,
    _: BpfValue,
    _: BpfValue,
    _: BpfValue,
    _: BpfValue,
) -> BpfValue {
    track_stub!(TODO("https://fxbug.dev/287120494"), "bpf_skb_change_head");
    0.into()
}

const L3_CSUM_REPLACE: &'static str = "l3_csum_replace";

fn bpf_l3_csum_replace(
    _context: &mut HelperFunctionContext<'_>,
    _: BpfValue,
    _: BpfValue,
    _: BpfValue,
    _: BpfValue,
    _: BpfValue,
) -> BpfValue {
    track_stub!(TODO("https://fxbug.dev/287120494"), "bpf_l3_csum_replace");
    0.into()
}

const L4_CSUM_REPLACE: &'static str = "l4_csum_replace";

fn bpf_l4_csum_replace(
    _context: &mut HelperFunctionContext<'_>,
    _: BpfValue,
    _: BpfValue,
    _: BpfValue,
    _: BpfValue,
    _: BpfValue,
) -> BpfValue {
    track_stub!(TODO("https://fxbug.dev/287120494"), "bpf_l4_csum_replace");
    0.into()
}

const SKB_LOAD_BYTES_RELATIVE_NAME: &'static str = "skb_load_bytes_relative";

fn bpf_skb_load_bytes_relative(
    _context: &mut HelperFunctionContext<'_>,
    _: BpfValue,
    _: BpfValue,
    _: BpfValue,
    _: BpfValue,
    _: BpfValue,
) -> BpfValue {
    track_stub!(TODO("https://fxbug.dev/287120494"), "bpf_skb_load_bytes_relative");
    0.into()
}

const KTIME_GET_BOOT_NS_NAME: &'static str = "ktime_get_boot_ns";

fn bpf_ktime_get_boot_ns(
    _context: &mut HelperFunctionContext<'_>,
    _: BpfValue,
    _: BpfValue,
    _: BpfValue,
    _: BpfValue,
    _: BpfValue,
) -> BpfValue {
    track_stub!(TODO("https://fxbug.dev/287120494"), "bpf_ktime_get_boot_ns");
    0.into()
}

#[derive(Clone, Default, Debug)]
pub struct BpfTypeFilter(HashSet<ProgramType>);

impl BpfTypeFilter {
    pub fn accept(&self, program_type: ProgramType) -> bool {
        self.0.is_empty() || self.0.contains(&program_type)
    }
}

impl<T: IntoIterator<Item = ProgramType>> From<T> for BpfTypeFilter {
    fn from(types: T) -> Self {
        Self(types.into_iter().collect())
    }
}

pub static BPF_HELPERS: Lazy<Vec<(BpfTypeFilter, EbpfHelper<HelperFunctionContextMarker>)>> =
    Lazy::new(|| {
        let ring_buffer_reservation = RING_BUFFER_RESERVATION.clone();
        let sk_buf_id = SK_BUF_ID.clone();
        let bpf_sock_id = BPF_SOCK_ID.clone();
        vec![
            (
                BpfTypeFilter::default(),
                EbpfHelper {
                    index: bpf_func_id_BPF_FUNC_map_lookup_elem,
                    name: MAP_LOOKUP_ELEM_NAME,
                    function_pointer: Arc::new(bpf_map_lookup_elem),
                    signature: FunctionSignature {
                        args: vec![
                            Type::ConstPtrToMapParameter,
                            Type::MapKeyParameter { map_ptr_index: 0 },
                        ],
                        return_value: Type::NullOrParameter(Box::new(Type::MapValueParameter {
                            map_ptr_index: 0,
                        })),
                        invalidate_array_bounds: false,
                    },
                },
            ),
            (
                BpfTypeFilter::default(),
                EbpfHelper {
                    index: bpf_func_id_BPF_FUNC_map_update_elem,
                    name: MAP_UPDATE_ELEM_NAME,
                    function_pointer: Arc::new(bpf_map_update_elem),
                    signature: FunctionSignature {
                        args: vec![
                            Type::ConstPtrToMapParameter,
                            Type::MapKeyParameter { map_ptr_index: 0 },
                            Type::MapValueParameter { map_ptr_index: 0 },
                            Type::ScalarValueParameter,
                        ],
                        return_value: Type::unknown_written_scalar_value(),
                        invalidate_array_bounds: false,
                    },
                },
            ),
            (
                BpfTypeFilter::default(),
                EbpfHelper {
                    index: bpf_func_id_BPF_FUNC_map_delete_elem,
                    name: MAP_DELETE_ELEM_NAME,
                    function_pointer: Arc::new(bpf_map_delete_elem),
                    signature: FunctionSignature {
                        args: vec![
                            Type::ConstPtrToMapParameter,
                            Type::MapKeyParameter { map_ptr_index: 0 },
                        ],
                        return_value: Type::unknown_written_scalar_value(),
                        invalidate_array_bounds: false,
                    },
                },
            ),
            (
                BpfTypeFilter::default(),
                EbpfHelper {
                    index: bpf_func_id_BPF_FUNC_trace_printk,
                    name: TRACE_PRINTK,
                    function_pointer: Arc::new(bpf_trace_printk),
                    signature: FunctionSignature {
                        // TODO("https://fxbug.dev/287120494"): Specify arguments
                        args: vec![],
                        return_value: Type::unknown_written_scalar_value(),
                        invalidate_array_bounds: false,
                    },
                },
            ),
            (
                BpfTypeFilter::default(),
                EbpfHelper {
                    index: bpf_func_id_BPF_FUNC_ktime_get_ns,
                    name: KTIME_GET_NS_NAME,
                    function_pointer: Arc::new(bpf_ktime_get_ns),
                    signature: FunctionSignature {
                        args: vec![],
                        return_value: Type::unknown_written_scalar_value(),
                        invalidate_array_bounds: false,
                    },
                },
            ),
            (
                BpfTypeFilter::default(),
                EbpfHelper {
                    index: bpf_func_id_BPF_FUNC_get_socket_uid,
                    name: GET_SOCKET_UID_NAME,
                    function_pointer: Arc::new(bpf_get_socket_uid),
                    signature: FunctionSignature {
                        args: vec![Type::StructParameter { id: sk_buf_id.clone() }],
                        return_value: Type::unknown_written_scalar_value(),
                        invalidate_array_bounds: false,
                    },
                },
            ),
            (
                BpfTypeFilter::default(),
                EbpfHelper {
                    index: bpf_func_id_BPF_FUNC_get_current_uid_gid,
                    name: GET_CURRENT_UID_GID_NAME,
                    function_pointer: Arc::new(bpf_get_current_uid_gid),
                    signature: FunctionSignature {
                        args: vec![],
                        return_value: Type::unknown_written_scalar_value(),
                        invalidate_array_bounds: false,
                    },
                },
            ),
            (
                BpfTypeFilter::default(),
                EbpfHelper {
                    index: bpf_func_id_BPF_FUNC_skb_pull_data,
                    name: SKB_PULL_DATA_NAME,
                    function_pointer: Arc::new(bpf_skb_pull_data),
                    signature: FunctionSignature {
                        args: vec![
                            Type::StructParameter { id: sk_buf_id.clone() },
                            Type::ScalarValueParameter,
                        ],
                        return_value: Type::unknown_written_scalar_value(),
                        invalidate_array_bounds: true,
                    },
                },
            ),
            (
                BpfTypeFilter::default(),
                EbpfHelper {
                    index: bpf_func_id_BPF_FUNC_ringbuf_reserve,
                    name: RINGBUF_RESERVE_NAME,
                    function_pointer: Arc::new(bpf_ringbuf_reserve),
                    signature: FunctionSignature {
                        args: vec![
                            Type::ConstPtrToMapParameter,
                            Type::ScalarValueParameter,
                            Type::ScalarValueParameter,
                        ],
                        return_value: Type::NullOrParameter(Box::new(Type::ReleasableParameter {
                            id: ring_buffer_reservation.clone(),
                            inner: Box::new(Type::MemoryParameter {
                                size: MemoryParameterSize::Reference { index: 1 },
                                input: false,
                                output: false,
                            }),
                        })),
                        invalidate_array_bounds: false,
                    },
                },
            ),
            (
                BpfTypeFilter::default(),
                EbpfHelper {
                    index: bpf_func_id_BPF_FUNC_ringbuf_submit,
                    name: RINGBUF_SUBMIT_NAME,
                    function_pointer: Arc::new(bpf_ringbuf_submit),
                    signature: FunctionSignature {
                        args: vec![
                            Type::ReleaseParameter { id: ring_buffer_reservation.clone() },
                            Type::ScalarValueParameter,
                        ],
                        return_value: Type::default(),
                        invalidate_array_bounds: false,
                    },
                },
            ),
            (
                BpfTypeFilter::default(),
                EbpfHelper {
                    index: bpf_func_id_BPF_FUNC_ringbuf_discard,
                    name: RINGBUF_DISCARD_NAME,
                    function_pointer: Arc::new(bpf_ringbuf_discard),
                    signature: FunctionSignature {
                        args: vec![
                            Type::ReleaseParameter { id: ring_buffer_reservation },
                            Type::ScalarValueParameter,
                        ],
                        return_value: Type::default(),
                        invalidate_array_bounds: false,
                    },
                },
            ),
            (
                BpfTypeFilter::default(),
                EbpfHelper {
                    index: bpf_func_id_BPF_FUNC_skb_change_proto,
                    name: SKB_CHANGE_PROTO_NAME,
                    function_pointer: Arc::new(bpf_skb_change_proto),
                    signature: FunctionSignature {
                        args: vec![
                            Type::StructParameter { id: sk_buf_id.clone() },
                            Type::ScalarValueParameter,
                            Type::ScalarValueParameter,
                        ],
                        return_value: Type::unknown_written_scalar_value(),
                        invalidate_array_bounds: true,
                    },
                },
            ),
            (
                BpfTypeFilter::default(),
                EbpfHelper {
                    index: bpf_func_id_BPF_FUNC_csum_update,
                    name: CSUM_UPDATE_NAME,
                    function_pointer: Arc::new(bpf_csum_update),
                    signature: FunctionSignature {
                        args: vec![
                            Type::StructParameter { id: sk_buf_id.clone() },
                            Type::ScalarValueParameter,
                        ],
                        return_value: Type::unknown_written_scalar_value(),
                        invalidate_array_bounds: false,
                    },
                },
            ),
            (
                BpfTypeFilter::default(),
                EbpfHelper {
                    index: bpf_func_id_BPF_FUNC_probe_read_str,
                    name: PROBE_READ_STR_NAME,
                    function_pointer: Arc::new(bpf_probe_read_str),
                    signature: FunctionSignature {
                        // TODO(347257215): Implement verifier feature
                        args: vec![],
                        return_value: Type::unknown_written_scalar_value(),
                        invalidate_array_bounds: false,
                    },
                },
            ),
            (
                vec![
                    ProgramType::CgroupSkb,
                    ProgramType::SchedAct,
                    ProgramType::SchedCls,
                    ProgramType::SocketFilter,
                ]
                .into(),
                EbpfHelper {
                    index: bpf_func_id_BPF_FUNC_get_socket_cookie,
                    name: GET_SOCKET_COOKIE_NAME,
                    function_pointer: Arc::new(bpf_get_socket_cookie_sk_buf),
                    signature: FunctionSignature {
                        args: vec![Type::StructParameter { id: sk_buf_id.clone() }],
                        return_value: Type::unknown_written_scalar_value(),
                        invalidate_array_bounds: false,
                    },
                },
            ),
            (
                vec![ProgramType::CgroupSock].into(),
                EbpfHelper {
                    index: bpf_func_id_BPF_FUNC_get_socket_cookie,
                    name: GET_SOCKET_COOKIE_NAME,
                    function_pointer: Arc::new(bpf_get_socket_cookie_bpf_sock),
                    signature: FunctionSignature {
                        args: vec![Type::StructParameter { id: bpf_sock_id }],
                        return_value: Type::unknown_written_scalar_value(),
                        invalidate_array_bounds: false,
                    },
                },
            ),
            (
                BpfTypeFilter::default(),
                EbpfHelper {
                    index: bpf_func_id_BPF_FUNC_redirect,
                    name: REDIRECT_NAME,
                    function_pointer: Arc::new(bpf_redirect),
                    signature: FunctionSignature {
                        args: vec![Type::ScalarValueParameter, Type::ScalarValueParameter],
                        return_value: Type::unknown_written_scalar_value(),
                        invalidate_array_bounds: false,
                    },
                },
            ),
            (
                BpfTypeFilter::default(),
                EbpfHelper {
                    index: bpf_func_id_BPF_FUNC_skb_adjust_room,
                    name: SKB_ADJUST_ROOM_NAME,
                    function_pointer: Arc::new(bpf_skb_adjust_room),
                    signature: FunctionSignature {
                        args: vec![
                            Type::StructParameter { id: sk_buf_id.clone() },
                            Type::ScalarValueParameter,
                            Type::ScalarValueParameter,
                            Type::ScalarValueParameter,
                        ],
                        return_value: Type::unknown_written_scalar_value(),
                        invalidate_array_bounds: true,
                    },
                },
            ),
            (
                BpfTypeFilter::default(),
                EbpfHelper {
                    index: bpf_func_id_BPF_FUNC_l3_csum_replace,
                    name: L3_CSUM_REPLACE,
                    function_pointer: Arc::new(bpf_l3_csum_replace),
                    signature: FunctionSignature {
                        args: vec![
                            Type::StructParameter { id: sk_buf_id.clone() },
                            Type::ScalarValueParameter,
                            Type::ScalarValueParameter,
                            Type::ScalarValueParameter,
                            Type::ScalarValueParameter,
                        ],
                        return_value: Type::unknown_written_scalar_value(),
                        invalidate_array_bounds: true,
                    },
                },
            ),
            (
                BpfTypeFilter::default(),
                EbpfHelper {
                    index: bpf_func_id_BPF_FUNC_l4_csum_replace,
                    name: L4_CSUM_REPLACE,
                    function_pointer: Arc::new(bpf_l4_csum_replace),
                    signature: FunctionSignature {
                        args: vec![
                            Type::StructParameter { id: sk_buf_id.clone() },
                            Type::ScalarValueParameter,
                            Type::ScalarValueParameter,
                            Type::ScalarValueParameter,
                            Type::ScalarValueParameter,
                        ],
                        return_value: Type::unknown_written_scalar_value(),
                        invalidate_array_bounds: true,
                    },
                },
            ),
            (
                BpfTypeFilter::default(),
                EbpfHelper {
                    index: bpf_func_id_BPF_FUNC_skb_store_bytes,
                    name: SKB_STORE_BYTES,
                    function_pointer: Arc::new(bpf_skb_store_bytes),
                    signature: FunctionSignature {
                        args: vec![
                            Type::StructParameter { id: sk_buf_id.clone() },
                            Type::ScalarValueParameter,
                            Type::MemoryParameter {
                                size: MemoryParameterSize::Reference { index: 3 },
                                input: true,
                                output: false,
                            },
                            Type::ScalarValueParameter,
                            Type::ScalarValueParameter,
                        ],
                        return_value: Type::unknown_written_scalar_value(),
                        invalidate_array_bounds: true,
                    },
                },
            ),
            (
                BpfTypeFilter::default(),
                EbpfHelper {
                    index: bpf_func_id_BPF_FUNC_skb_change_head,
                    name: SKB_CHANGE_HEAD,
                    function_pointer: Arc::new(bpf_skb_change_head),
                    signature: FunctionSignature {
                        args: vec![
                            Type::StructParameter { id: sk_buf_id.clone() },
                            Type::ScalarValueParameter,
                            Type::ScalarValueParameter,
                        ],
                        return_value: Type::unknown_written_scalar_value(),
                        invalidate_array_bounds: true,
                    },
                },
            ),
            (
                BpfTypeFilter::default(),
                EbpfHelper {
                    index: bpf_func_id_BPF_FUNC_skb_load_bytes_relative,
                    name: SKB_LOAD_BYTES_RELATIVE_NAME,
                    function_pointer: Arc::new(bpf_skb_load_bytes_relative),
                    signature: FunctionSignature {
                        args: vec![
                            Type::StructParameter { id: sk_buf_id },
                            Type::ScalarValueParameter,
                            Type::MemoryParameter {
                                size: MemoryParameterSize::Reference { index: 3 },
                                input: false,
                                output: true,
                            },
                            Type::ScalarValueParameter,
                        ],
                        return_value: Type::unknown_written_scalar_value(),
                        invalidate_array_bounds: false,
                    },
                },
            ),
            (
                BpfTypeFilter::default(),
                EbpfHelper {
                    index: bpf_func_id_BPF_FUNC_ktime_get_boot_ns,
                    name: KTIME_GET_BOOT_NS_NAME,
                    function_pointer: Arc::new(bpf_ktime_get_boot_ns),
                    signature: FunctionSignature {
                        args: vec![],
                        return_value: Type::unknown_written_scalar_value(),
                        invalidate_array_bounds: false,
                    },
                },
            ),
        ]
    });

#[derive(Debug, Default)]
struct ArgBuilder {
    id: Option<MemoryId>,
    descriptor: StructDescriptor,
}

impl ArgBuilder {
    fn set_id(&mut self, id: MemoryId) {
        self.id = Some(id);
    }
    fn add_scalar(&mut self, offset: usize, size: usize) {
        self.add_field(FieldDescriptor { offset, field_type: FieldType::Scalar { size } });
    }
    fn add_mut_scalar(&mut self, offset: usize, size: usize) {
        self.add_field(FieldDescriptor { offset, field_type: FieldType::MutableScalar { size } });
    }
    fn add_u32_scalar(&mut self, offset: usize) {
        self.add_scalar(offset, 4);
    }

    fn add_array_32(&mut self, start_offset: usize, end_offset: usize) {
        // Create a memory id for the data array
        let array_id = new_bpf_type_identifier();

        self.add_field(FieldDescriptor {
            offset: start_offset,
            field_type: FieldType::PtrToArray { id: array_id.clone(), is_32_bit: true },
        });
        self.add_field(FieldDescriptor {
            offset: end_offset,
            field_type: FieldType::PtrToEndArray { id: array_id, is_32_bit: true },
        });
    }

    fn add_field(&mut self, descriptor: FieldDescriptor) {
        self.descriptor.fields.push(descriptor);
    }
    fn build(self) -> Vec<Type> {
        let Self { id, descriptor } = self;
        vec![Type::PtrToStruct {
            id: id.unwrap_or_else(new_bpf_type_identifier),
            offset: 0,
            descriptor: Arc::new(descriptor),
        }]
    }
}

fn build_bpf_args_with_id<T: IntoBytes>(id: MemoryId) -> Vec<Type> {
    vec![Type::PtrToMemory { id, offset: 0, buffer_size: std::mem::size_of::<T>() as u64 }]
}

fn build_bpf_args<T: IntoBytes>() -> Vec<Type> {
    build_bpf_args_with_id::<T>(new_bpf_type_identifier())
}

#[repr(C)]
#[derive(Copy, Clone, IntoBytes, Immutable, KnownLayout, FromBytes)]
struct SkBuf {
    pub len: u32,
    pub pkt_type: u32,
    pub mark: u32,
    pub queue_mapping: u32,
    pub protocol: u32,
    pub vlan_present: u32,
    pub vlan_tci: u32,
    pub vlan_proto: u32,
    pub priority: u32,
    pub ingress_ifindex: u32,
    pub ifindex: u32,
    pub tc_index: u32,
    pub cb: [u32; 5usize],
    pub hash: u32,
    pub tc_classid: u32,
    pub _unused_original_data: u32,
    pub _unused_original_end_data: u32,
    pub napi_id: u32,
    pub family: u32,
    pub remote_ip4: u32,
    pub local_ip4: u32,
    pub remote_ip6: [u32; 4usize],
    pub local_ip6: [u32; 4usize],
    pub remote_port: u32,
    pub local_port: u32,
    pub data_meta: u32,
    pub flow_keys: uref<bpf_flow_keys>,
    pub tstamp: u64,
    pub wire_len: u32,
    pub gso_segs: u32,
    pub sk: uref<bpf_sock>,
    pub gso_size: u32,
    pub tstamp_type: u8,
    pub _padding: [u8; 3usize],
    pub hwtstamp: u64,
    pub data: uref<u8>,
    pub data_end: uref<u8>,
}

static RING_BUFFER_RESERVATION: Lazy<MemoryId> = Lazy::new(new_bpf_type_identifier);

static SK_BUF_ID: Lazy<MemoryId> = Lazy::new(new_bpf_type_identifier);
static SK_BUF_ARGS: Lazy<Vec<Type>> = Lazy::new(|| {
    let mut builder = ArgBuilder::default();
    // Set the id of the main struct.
    builder.set_id(SK_BUF_ID.clone());

    let cb_offset = std::mem::offset_of!(__sk_buff, cb);
    let hash_offset = std::mem::offset_of!(__sk_buff, hash);

    // All fields from the start of `__sk_buff` to `cb` are read-only scalars.
    builder.add_scalar(0, cb_offset);

    // `cb` is a mutable array.
    builder.add_mut_scalar(cb_offset, hash_offset - cb_offset);

    builder.add_u32_scalar(std::mem::offset_of!(__sk_buff, hash));
    builder.add_u32_scalar(std::mem::offset_of!(__sk_buff, napi_id));
    builder.add_u32_scalar(std::mem::offset_of!(__sk_buff, tstamp));
    builder.add_u32_scalar(std::mem::offset_of!(__sk_buff, gso_segs));
    builder.add_u32_scalar(std::mem::offset_of!(__sk_buff, gso_size));

    // Add and remap `data` and `data_end` fields.
    builder.add_array_32(
        std::mem::offset_of!(__sk_buff, data),
        std::mem::offset_of!(__sk_buff, data_end),
    );

    builder.build()
});

static SK_BUF_MAPPING: Lazy<StructMapping> = Lazy::new(|| StructMapping {
    memory_id: SK_BUF_ID.clone(),
    fields: vec![
        FieldMapping {
            source_offset: std::mem::offset_of!(__sk_buff, data),
            target_offset: std::mem::offset_of!(SkBuf, data),
        },
        FieldMapping {
            source_offset: std::mem::offset_of!(__sk_buff, data_end),
            target_offset: std::mem::offset_of!(SkBuf, data_end),
        },
    ],
});

#[repr(C)]
#[derive(Copy, Clone, IntoBytes, Immutable, KnownLayout, FromBytes)]
struct XdpMd {
    pub data: uref<u8>,
    pub data_meta: u32,
    pub ingress_ifindex: u32,
    pub rx_queue_index: u32,
    pub egress_ifindex: u32,
    pub data_end: uref<u8>,
}

static XDP_MD_ID: Lazy<MemoryId> = Lazy::new(new_bpf_type_identifier);
static XDP_MD_ARGS: Lazy<Vec<Type>> = Lazy::new(|| {
    let mut builder = ArgBuilder::default();
    builder.set_id(XDP_MD_ID.clone());

    // Define and map `data` and `data_end` fields.
    builder
        .add_array_32(std::mem::offset_of!(xdp_md, data), std::mem::offset_of!(xdp_md, data_end));

    // All fields starting from data_meta are readable
    let data_meta_offset = std::mem::offset_of!(xdp_md, data_meta);
    builder.add_scalar(data_meta_offset, std::mem::size_of::<xdp_md>() - data_meta_offset);

    builder.build()
});

static XDP_MD_MAPPING: Lazy<StructMapping> = Lazy::new(|| StructMapping {
    memory_id: XDP_MD_ID.clone(),
    fields: vec![
        FieldMapping {
            source_offset: std::mem::offset_of!(xdp_md, data),
            target_offset: std::mem::offset_of!(XdpMd, data),
        },
        FieldMapping {
            source_offset: std::mem::offset_of!(xdp_md, data_end),
            target_offset: std::mem::offset_of!(XdpMd, data_end),
        },
    ],
});

static BPF_USER_PT_REGS_T_ARGS: Lazy<Vec<Type>> =
    Lazy::new(|| build_bpf_args::<bpf_user_pt_regs_t>());

static BPF_SOCK_ID: Lazy<MemoryId> = Lazy::new(new_bpf_type_identifier);
static BPF_SOCK_ARGS: Lazy<Vec<Type>> =
    Lazy::new(|| build_bpf_args_with_id::<bpf_sock>(BPF_SOCK_ID.clone()));

static BPF_SOCKOPT_ARGS: Lazy<Vec<Type>> = Lazy::new(|| build_bpf_args::<bpf_sockopt>());

static BPF_SOCK_ADDR_ARGS: Lazy<Vec<Type>> = Lazy::new(|| build_bpf_args::<bpf_sock_addr>());

static BPF_FUSE_ARGS: Lazy<Vec<Type>> = Lazy::new(|| {
    let mut builder = ArgBuilder::default();
    builder.add_field(FieldDescriptor {
        offset: (std::mem::offset_of!(fuse_bpf_args, out_args)
            + std::mem::offset_of!(fuse_bpf_arg, value)),
        field_type: FieldType::PtrToMemory {
            id: new_bpf_type_identifier(),
            buffer_size: std::mem::size_of::<fuse_entry_out>(),
            is_32_bit: false,
        },
    });
    builder.add_field(FieldDescriptor {
        offset: (std::mem::offset_of!(fuse_bpf_args, out_args)
            + std::mem::size_of::<fuse_bpf_arg>()
            + std::mem::offset_of!(fuse_bpf_arg, value)),
        field_type: FieldType::PtrToMemory {
            id: new_bpf_type_identifier(),
            buffer_size: std::mem::size_of::<fuse_entry_bpf_out>(),
            is_32_bit: false,
        },
    });
    builder.build()
});

#[repr(C)]
#[derive(Copy, Clone, IntoBytes, Immutable, KnownLayout, FromBytes)]
struct TraceEntry {
    r#type: u16,
    flags: u8,
    preemp_count: u8,
    pid: u32,
}

#[repr(C)]
#[derive(Copy, Clone, IntoBytes, Immutable, KnownLayout, FromBytes)]
struct TraceEvent {
    trace_entry: TraceEntry,
    id: u64,
    // This is defined a being big enough for all expected tracepoint. It is not clear how the
    // verifier can know which tracepoint is targeted when the program is loaded. Instead, this
    // array will be big enough, and will be filled with 0 when running a given program.
    args: [u64; 16],
}

static BPF_TRACEPOINT_ARGS: Lazy<Vec<Type>> = Lazy::new(|| build_bpf_args::<TraceEvent>());

pub fn get_bpf_args_and_mapping(
    program_type: ProgramType,
) -> (&'static [Type], Option<&'static StructMapping>) {
    match program_type {
        ProgramType::CgroupSkb
        | ProgramType::SchedAct
        | ProgramType::SchedCls
        | ProgramType::SocketFilter => (&SK_BUF_ARGS, Some(&SK_BUF_MAPPING)),
        ProgramType::Xdp => (&XDP_MD_ARGS, Some(&XDP_MD_MAPPING)),
        ProgramType::KProbe => (&BPF_USER_PT_REGS_T_ARGS, None),
        ProgramType::TracePoint => (&BPF_TRACEPOINT_ARGS, None),
        ProgramType::CgroupSock => (&BPF_SOCK_ARGS, None),
        ProgramType::CgroupSockopt => (&BPF_SOCKOPT_ARGS, None),
        ProgramType::CgroupSockAddr => (&BPF_SOCK_ADDR_ARGS, None),
        ProgramType::Fuse => (&BPF_FUSE_ARGS, None),
        ProgramType::Unknown(_) => (&[], None),
    }
}

pub fn get_packet_memory_id(program_type: ProgramType) -> Option<MemoryId> {
    match program_type {
        ProgramType::CgroupSkb
        | ProgramType::SchedAct
        | ProgramType::SchedCls
        | ProgramType::SocketFilter => Some(SK_BUF_ID.clone()),
        _ => None,
    }
}
