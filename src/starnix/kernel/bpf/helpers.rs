// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{
    bpf::{map::Map, program::ProgramType},
    task::CurrentTask,
};
use ebpf::{
    new_bpf_type_identifier, BpfValue, EbpfHelper, EpbfRunContext, FieldMapping, FieldType,
    FunctionSignature, Type,
};
use linux_uapi::{
    __sk_buff, bpf_flow_keys, bpf_func_id_BPF_FUNC_map_delete_elem,
    bpf_func_id_BPF_FUNC_map_lookup_elem, bpf_func_id_BPF_FUNC_map_update_elem, bpf_sock,
    bpf_sock_addr, bpf_user_pt_regs_t, uref, xdp_md,
};
use once_cell::sync::Lazy;
use starnix_logging::track_stub;
use starnix_sync::{BpfHelperOps, Locked};
use std::sync::Arc;
use zerocopy::{AsBytes, FromBytes, FromZeros, NoCell};

pub struct HelperFunctionContext<'a> {
    pub locked: &'a mut Locked<'a, BpfHelperOps>,
    pub _current_task: &'a CurrentTask,
}

pub enum HelperFunctionContextMarker {}
impl EpbfRunContext for HelperFunctionContextMarker {
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
    let map: &Map = unsafe { &*map.as_ptr::<Map>() };
    let key =
        unsafe { std::slice::from_raw_parts(key.as_ptr::<u8>(), map.schema.key_size as usize) };

    let key = key.to_owned();
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

pub static BPF_HELPERS: Lazy<Vec<EbpfHelper<HelperFunctionContextMarker>>> = Lazy::new(|| {
    vec![
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
    ]
});

#[derive(Debug, Default)]
struct ArgBuilder {
    fields: Vec<FieldType>,
    mappings: Vec<FieldMapping>,
}

impl ArgBuilder {
    fn add_field(&mut self, field_type: FieldType) {
        self.fields.push(field_type);
    }
    fn add_mapping(&mut self, mapping: FieldMapping) {
        self.mappings.push(mapping);
    }
    fn build<T: AsBytes>(self) -> Vec<Type> {
        let buffer_size = std::mem::size_of::<T>() as u64;
        vec![
            Type::PtrToMemory {
                id: new_bpf_type_identifier(),
                offset: 0,
                buffer_size,
                fields: self.fields,
                mappings: self.mappings,
            },
            Type::from(buffer_size),
        ]
    }
}

fn build_bpf_args<T: AsBytes>() -> Vec<Type> {
    ArgBuilder::default().build::<T>()
}

#[repr(C)]
#[derive(Copy, Clone, AsBytes, FromBytes, NoCell, FromZeros)]
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
static SK_BUF_ARGS: Lazy<Vec<Type>> = Lazy::new(|| {
    let mut builder = ArgBuilder::default();
    // Create a memory id for the data array
    let array_id = new_bpf_type_identifier();
    // Map and define the data field
    builder.add_mapping(FieldMapping::new_size_mapping(
        std::mem::offset_of!(__sk_buff, data).try_into().unwrap(),
        std::mem::offset_of!(SkBuf, data).try_into().unwrap(),
    ));
    builder.add_field(FieldType {
        offset: std::mem::offset_of!(SkBuf, data).try_into().unwrap(),
        field_type: Box::new(Type::PtrToArray { id: array_id.clone(), offset: 0 }),
    });
    // Map and define the data_end field
    builder.add_mapping(FieldMapping::new_size_mapping(
        std::mem::offset_of!(__sk_buff, data_end).try_into().unwrap(),
        std::mem::offset_of!(SkBuf, data_end).try_into().unwrap(),
    ));
    builder.add_field(FieldType {
        offset: std::mem::offset_of!(SkBuf, data_end).try_into().unwrap(),
        field_type: Box::new(Type::PtrToEndArray { id: array_id }),
    });
    builder.build::<SkBuf>()
});

#[repr(C)]
#[derive(Copy, Clone, AsBytes, FromBytes, NoCell, FromZeros)]
struct XdpMd {
    pub data: uref<u8>,
    pub data_meta: u32,
    pub ingress_ifindex: u32,
    pub rx_queue_index: u32,
    pub egress_ifindex: u32,
    pub data_end: uref<u8>,
}
static XDP_MD_ARGS: Lazy<Vec<Type>> = Lazy::new(|| {
    let mut builder = ArgBuilder::default();
    // Create a memory id for the data array
    let array_id = new_bpf_type_identifier();
    // Map and define the data field
    builder.add_mapping(FieldMapping::new_size_mapping(
        std::mem::offset_of!(xdp_md, data).try_into().unwrap(),
        std::mem::offset_of!(XdpMd, data).try_into().unwrap(),
    ));
    builder.add_field(FieldType {
        offset: std::mem::offset_of!(XdpMd, data).try_into().unwrap(),
        field_type: Box::new(Type::PtrToArray { id: array_id.clone(), offset: 0 }),
    });
    // Map and define the data_end field
    builder.add_mapping(FieldMapping::new_size_mapping(
        std::mem::offset_of!(xdp_md, data_end).try_into().unwrap(),
        std::mem::offset_of!(XdpMd, data_end).try_into().unwrap(),
    ));
    builder.add_field(FieldType {
        offset: std::mem::offset_of!(XdpMd, data_end).try_into().unwrap(),
        field_type: Box::new(Type::PtrToEndArray { id: array_id }),
    });
    builder.build::<XdpMd>()
});

static BPF_USER_PT_REGS_T_ARGS: Lazy<Vec<Type>> =
    Lazy::new(|| build_bpf_args::<bpf_user_pt_regs_t>());

static U64_ARGS: Lazy<Vec<Type>> = Lazy::new(|| build_bpf_args::<u64>());

static BPF_SOCK_ARGS: Lazy<Vec<Type>> = Lazy::new(|| build_bpf_args::<bpf_sock>());

static BPF_SOCK_ADDR_ARGS: Lazy<Vec<Type>> = Lazy::new(|| build_bpf_args::<bpf_sock_addr>());

pub fn get_bpf_args(program_type: &ProgramType) -> &'static [Type] {
    match program_type {
        ProgramType::CgroupSkb
        | ProgramType::SchedAct
        | ProgramType::SchedCls
        | ProgramType::SocketFilter => &SK_BUF_ARGS,
        ProgramType::Xdp => &XDP_MD_ARGS,
        ProgramType::KProbe => &BPF_USER_PT_REGS_T_ARGS,
        ProgramType::TracePoint => &U64_ARGS,
        ProgramType::CgroupSock => &BPF_SOCK_ARGS,
        ProgramType::CgroupSockAddr => &BPF_SOCK_ADDR_ARGS,
        ProgramType::Unknown(_) => &[],
    }
}
