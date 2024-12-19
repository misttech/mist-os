// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::bpf::map::{Map, MapKey, RingBufferWakeupPolicy};
use crate::task::CurrentTask;
use ebpf::{BpfValue, EbpfHelperImpl, EbpfRunContext, FieldMapping, StructMapping};
use ebpf_api::SK_BUF_ID;
use linux_uapi::{
    __sk_buff, bpf_flow_keys, bpf_func_id_BPF_FUNC_get_current_uid_gid,
    bpf_func_id_BPF_FUNC_get_socket_cookie, bpf_func_id_BPF_FUNC_get_socket_uid,
    bpf_func_id_BPF_FUNC_ktime_get_boot_ns, bpf_func_id_BPF_FUNC_ktime_get_ns,
    bpf_func_id_BPF_FUNC_map_delete_elem, bpf_func_id_BPF_FUNC_map_lookup_elem,
    bpf_func_id_BPF_FUNC_map_update_elem, bpf_func_id_BPF_FUNC_ringbuf_discard,
    bpf_func_id_BPF_FUNC_ringbuf_reserve, bpf_func_id_BPF_FUNC_ringbuf_submit,
    bpf_func_id_BPF_FUNC_skb_load_bytes_relative, bpf_func_id_BPF_FUNC_trace_printk, bpf_sock,
    uref,
};
use starnix_logging::track_stub;
use starnix_sync::{BpfHelperOps, Locked};
use std::sync::LazyLock;
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

fn bpf_map_lookup_elem(
    _context: &mut HelperFunctionContext<'_>,
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

    map.get_raw(&key).map(BpfValue::from).unwrap_or_else(BpfValue::default)
}

fn bpf_map_update_elem(
    _context: &mut HelperFunctionContext<'_>,
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

    let key = MapKey::from_slice(key);
    map.update(key, value, flags).map(|_| 0).unwrap_or(u64::MAX).into()
}

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

fn bpf_ringbuf_reserve(
    _context: &mut HelperFunctionContext<'_>,
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
    map.ringbuf_reserve(size, flags)
        .map(BpfValue::from)
        .unwrap_or_else(|_| BpfValue::default())
}

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

// TODO(https://fxbug.dev/378507648): Move to ebpf crate.
pub static BPF_COMMON_HELPER_IMPLS: &[(u32, EbpfHelperImpl<HelperFunctionContextMarker>)] = &[
    (bpf_func_id_BPF_FUNC_map_lookup_elem, EbpfHelperImpl(bpf_map_lookup_elem)),
    (bpf_func_id_BPF_FUNC_map_update_elem, EbpfHelperImpl(bpf_map_update_elem)),
    (bpf_func_id_BPF_FUNC_map_delete_elem, EbpfHelperImpl(bpf_map_delete_elem)),
    (bpf_func_id_BPF_FUNC_trace_printk, EbpfHelperImpl(bpf_trace_printk)),
    (bpf_func_id_BPF_FUNC_ktime_get_ns, EbpfHelperImpl(bpf_ktime_get_ns)),
    (bpf_func_id_BPF_FUNC_get_current_uid_gid, EbpfHelperImpl(bpf_get_current_uid_gid)),
    (bpf_func_id_BPF_FUNC_ringbuf_reserve, EbpfHelperImpl(bpf_ringbuf_reserve)),
    (bpf_func_id_BPF_FUNC_ringbuf_submit, EbpfHelperImpl(bpf_ringbuf_submit)),
    (bpf_func_id_BPF_FUNC_ringbuf_discard, EbpfHelperImpl(bpf_ringbuf_discard)),
    (bpf_func_id_BPF_FUNC_ktime_get_boot_ns, EbpfHelperImpl(bpf_ktime_get_boot_ns)),
];

// Helpers that are supplied to socket filter programs in addition to the common helpers.
pub static BPF_HELPER_IMPLS_FOR_SOCKET_FILTER: &[(
    u32,
    EbpfHelperImpl<HelperFunctionContextMarker>,
)] = &[
    (bpf_func_id_BPF_FUNC_get_socket_uid, EbpfHelperImpl(bpf_get_socket_uid)),
    (bpf_func_id_BPF_FUNC_get_socket_cookie, EbpfHelperImpl(bpf_get_socket_cookie_sk_buf)),
    (bpf_func_id_BPF_FUNC_skb_load_bytes_relative, EbpfHelperImpl(bpf_skb_load_bytes_relative)),
];

#[repr(C)]
#[derive(Copy, Clone, IntoBytes, Immutable, KnownLayout, FromBytes, Default)]
pub struct SkBuf {
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

pub static SK_BUF_MAPPING: LazyLock<StructMapping> = LazyLock::new(|| StructMapping {
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
