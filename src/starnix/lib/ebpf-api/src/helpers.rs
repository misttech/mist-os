// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::maps::{Map, MapKey, RingBufferWakeupPolicy};
use ebpf::{BpfValue, EbpfHelperImpl, EbpfProgramContext};
use inspect_stubs::track_stub;
use linux_uapi::{
    bpf_func_id_BPF_FUNC_get_socket_cookie, bpf_func_id_BPF_FUNC_get_socket_uid,
    bpf_func_id_BPF_FUNC_ktime_get_boot_ns, bpf_func_id_BPF_FUNC_ktime_get_ns,
    bpf_func_id_BPF_FUNC_map_delete_elem, bpf_func_id_BPF_FUNC_map_lookup_elem,
    bpf_func_id_BPF_FUNC_map_update_elem, bpf_func_id_BPF_FUNC_ringbuf_discard,
    bpf_func_id_BPF_FUNC_ringbuf_reserve, bpf_func_id_BPF_FUNC_ringbuf_submit,
    bpf_func_id_BPF_FUNC_skb_load_bytes_relative, bpf_func_id_BPF_FUNC_trace_printk, uid_t,
};

fn bpf_map_lookup_elem<C: EbpfProgramContext>(
    _context: &mut C::RunContext<'_>,
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

fn bpf_map_update_elem<C: EbpfProgramContext>(
    _context: &mut C::RunContext<'_>,
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

fn bpf_map_delete_elem<C: EbpfProgramContext>(
    _context: &mut C::RunContext<'_>,
    _map: BpfValue,
    _key: BpfValue,
    _: BpfValue,
    _: BpfValue,
    _: BpfValue,
) -> BpfValue {
    track_stub!(TODO("https://fxbug.dev/287120494"), "bpf_map_delete_elem");
    u64::MAX.into()
}

fn bpf_trace_printk<C: EbpfProgramContext>(
    _context: &mut C::RunContext<'_>,
    _fmt: BpfValue,
    _fmt_size: BpfValue,
    _: BpfValue,
    _: BpfValue,
    _: BpfValue,
) -> BpfValue {
    track_stub!(TODO("https://fxbug.dev/287120494"), "bpf_trace_printk");
    0.into()
}

fn bpf_ktime_get_ns<C: EbpfProgramContext>(
    _context: &mut C::RunContext<'_>,
    _: BpfValue,
    _: BpfValue,
    _: BpfValue,
    _: BpfValue,
    _: BpfValue,
) -> BpfValue {
    track_stub!(TODO("https://fxbug.dev/287120494"), "bpf_ktime_get_ns");
    42.into()
}

fn bpf_ringbuf_reserve<C: EbpfProgramContext>(
    _context: &mut C::RunContext<'_>,
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
    map.ringbuf_reserve(size, flags).map(BpfValue::from).unwrap_or_else(|_| BpfValue::default())
}

fn bpf_ringbuf_submit<C: EbpfProgramContext>(
    _context: &mut C::RunContext<'_>,
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

fn bpf_ringbuf_discard<C: EbpfProgramContext>(
    _context: &mut C::RunContext<'_>,
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

fn bpf_ktime_get_boot_ns<C: EbpfProgramContext>(
    _context: &mut C::RunContext<'_>,
    _: BpfValue,
    _: BpfValue,
    _: BpfValue,
    _: BpfValue,
    _: BpfValue,
) -> BpfValue {
    track_stub!(TODO("https://fxbug.dev/287120494"), "bpf_ktime_get_boot_ns");
    0.into()
}

pub fn get_common_helpers<C: EbpfProgramContext>() -> Vec<(u32, EbpfHelperImpl<C>)> {
    vec![
        (bpf_func_id_BPF_FUNC_map_lookup_elem, EbpfHelperImpl(bpf_map_lookup_elem)),
        (bpf_func_id_BPF_FUNC_map_update_elem, EbpfHelperImpl(bpf_map_update_elem)),
        (bpf_func_id_BPF_FUNC_map_delete_elem, EbpfHelperImpl(bpf_map_delete_elem)),
        (bpf_func_id_BPF_FUNC_trace_printk, EbpfHelperImpl(bpf_trace_printk)),
        (bpf_func_id_BPF_FUNC_ktime_get_ns, EbpfHelperImpl(bpf_ktime_get_ns)),
        (bpf_func_id_BPF_FUNC_ringbuf_reserve, EbpfHelperImpl(bpf_ringbuf_reserve)),
        (bpf_func_id_BPF_FUNC_ringbuf_submit, EbpfHelperImpl(bpf_ringbuf_submit)),
        (bpf_func_id_BPF_FUNC_ringbuf_discard, EbpfHelperImpl(bpf_ringbuf_discard)),
        (bpf_func_id_BPF_FUNC_ktime_get_boot_ns, EbpfHelperImpl(bpf_ktime_get_boot_ns)),
    ]
}

pub trait SocketFilterContext {
    type SkBuf;
    fn get_socket_uid(&self, sk_buf: &Self::SkBuf) -> uid_t;
    fn get_socket_cookie(&self, sk_buf: &Self::SkBuf) -> u64;
}

fn bpf_get_socket_uid<'a, C: EbpfProgramContext>(
    context: &mut C::RunContext<'a>,
    sk_buf: BpfValue,
    _: BpfValue,
    _: BpfValue,
    _: BpfValue,
    _: BpfValue,
) -> BpfValue
where
    for<'b> C::RunContext<'b>: SocketFilterContext,
{
    // SAFETY: Verifier checks that the argument points at the `SkBuf`.
    let sk_buf: &<C::RunContext<'a> as SocketFilterContext>::SkBuf = unsafe { &*sk_buf.as_ptr() };
    context.get_socket_uid(sk_buf).into()
}

fn bpf_get_socket_cookie<'a, C: EbpfProgramContext>(
    context: &mut C::RunContext<'a>,
    sk_buf: BpfValue,
    _: BpfValue,
    _: BpfValue,
    _: BpfValue,
    _: BpfValue,
) -> BpfValue
where
    for<'b> C::RunContext<'b>: SocketFilterContext,
{
    // SAFETY: Verifier checks that the argument points at the `SkBuf`.
    let sk_buf: &<C::RunContext<'a> as SocketFilterContext>::SkBuf = unsafe { &*sk_buf.as_ptr() };
    context.get_socket_cookie(sk_buf).into()
}

fn bpf_skb_load_bytes_relative<C: EbpfProgramContext>(
    _context: &mut C::RunContext<'_>,
    _: BpfValue,
    _: BpfValue,
    _: BpfValue,
    _: BpfValue,
    _: BpfValue,
) -> BpfValue {
    track_stub!(TODO("https://fxbug.dev/287120494"), "bpf_skb_load_bytes_relative");
    0.into()
}

// Helpers that are supplied to socket filter programs in addition to the common helpers.
pub fn get_socket_filter_helpers<C: EbpfProgramContext>() -> Vec<(u32, EbpfHelperImpl<C>)>
where
    for<'a> C::RunContext<'a>: SocketFilterContext,
{
    vec![
        (bpf_func_id_BPF_FUNC_get_socket_uid, EbpfHelperImpl(bpf_get_socket_uid)),
        (bpf_func_id_BPF_FUNC_get_socket_cookie, EbpfHelperImpl(bpf_get_socket_cookie)),
        (bpf_func_id_BPF_FUNC_skb_load_bytes_relative, EbpfHelperImpl(bpf_skb_load_bytes_relative)),
    ]
}
