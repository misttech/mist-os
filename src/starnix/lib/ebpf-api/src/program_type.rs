// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use ebpf::{
    CallingContext, EbpfError, FieldDescriptor, FieldType, FunctionSignature, MapSchema, MemoryId,
    MemoryParameterSize, StructDescriptor, Type,
};
use linux_uapi::{
    __sk_buff, bpf_func_id_BPF_FUNC_csum_update, bpf_func_id_BPF_FUNC_get_current_uid_gid,
    bpf_func_id_BPF_FUNC_get_socket_cookie, bpf_func_id_BPF_FUNC_get_socket_uid,
    bpf_func_id_BPF_FUNC_ktime_get_boot_ns, bpf_func_id_BPF_FUNC_ktime_get_coarse_ns,
    bpf_func_id_BPF_FUNC_ktime_get_ns, bpf_func_id_BPF_FUNC_l3_csum_replace,
    bpf_func_id_BPF_FUNC_l4_csum_replace, bpf_func_id_BPF_FUNC_map_delete_elem,
    bpf_func_id_BPF_FUNC_map_lookup_elem, bpf_func_id_BPF_FUNC_map_update_elem,
    bpf_func_id_BPF_FUNC_probe_read_str, bpf_func_id_BPF_FUNC_probe_read_user,
    bpf_func_id_BPF_FUNC_probe_read_user_str, bpf_func_id_BPF_FUNC_redirect,
    bpf_func_id_BPF_FUNC_ringbuf_discard, bpf_func_id_BPF_FUNC_ringbuf_reserve,
    bpf_func_id_BPF_FUNC_ringbuf_submit, bpf_func_id_BPF_FUNC_skb_adjust_room,
    bpf_func_id_BPF_FUNC_skb_change_head, bpf_func_id_BPF_FUNC_skb_change_proto,
    bpf_func_id_BPF_FUNC_skb_load_bytes_relative, bpf_func_id_BPF_FUNC_skb_pull_data,
    bpf_func_id_BPF_FUNC_skb_store_bytes, bpf_func_id_BPF_FUNC_trace_printk,
    bpf_prog_type_BPF_PROG_TYPE_CGROUP_SKB, bpf_prog_type_BPF_PROG_TYPE_CGROUP_SOCK,
    bpf_prog_type_BPF_PROG_TYPE_CGROUP_SOCKOPT, bpf_prog_type_BPF_PROG_TYPE_CGROUP_SOCK_ADDR,
    bpf_prog_type_BPF_PROG_TYPE_KPROBE, bpf_prog_type_BPF_PROG_TYPE_SCHED_ACT,
    bpf_prog_type_BPF_PROG_TYPE_SCHED_CLS, bpf_prog_type_BPF_PROG_TYPE_SOCKET_FILTER,
    bpf_prog_type_BPF_PROG_TYPE_TRACEPOINT, bpf_prog_type_BPF_PROG_TYPE_XDP, bpf_sock,
    bpf_sock_addr, bpf_sockopt, bpf_user_pt_regs_t, fuse_bpf_arg, fuse_bpf_args,
    fuse_entry_bpf_out, fuse_entry_out, xdp_md,
};
use std::collections::HashMap;
use std::sync::{Arc, LazyLock};
use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout};

pub const BPF_PROG_TYPE_FUSE: u32 = 0x77777777;

pub struct EbpfHelperDefinition {
    pub index: u32,
    pub name: &'static str,
    pub signature: FunctionSignature,
}

#[derive(Clone, Default, Debug)]
pub struct BpfTypeFilter(Vec<ProgramType>);

impl<T: IntoIterator<Item = ProgramType>> From<T> for BpfTypeFilter {
    fn from(types: T) -> Self {
        Self(types.into_iter().collect())
    }
}

impl BpfTypeFilter {
    pub fn accept(&self, program_type: ProgramType) -> bool {
        self.0.is_empty() || self.0.iter().find(|v| **v == program_type).is_some()
    }
}

static BPF_HELPERS_DEFINITIONS: LazyLock<Vec<(BpfTypeFilter, EbpfHelperDefinition)>> =
    LazyLock::new(|| {
        vec![
            (
                BpfTypeFilter::default(),
                EbpfHelperDefinition {
                    index: bpf_func_id_BPF_FUNC_map_lookup_elem,
                    name: "map_lookup_elem",
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
                EbpfHelperDefinition {
                    index: bpf_func_id_BPF_FUNC_map_update_elem,
                    name: "map_update_elem",
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
                EbpfHelperDefinition {
                    index: bpf_func_id_BPF_FUNC_map_delete_elem,
                    name: "map_delete_elem",
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
                EbpfHelperDefinition {
                    index: bpf_func_id_BPF_FUNC_trace_printk,
                    name: "trace_printk",
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
                EbpfHelperDefinition {
                    index: bpf_func_id_BPF_FUNC_ktime_get_ns,
                    name: "ktime_get_ns",
                    signature: FunctionSignature {
                        args: vec![],
                        return_value: Type::unknown_written_scalar_value(),
                        invalidate_array_bounds: false,
                    },
                },
            ),
            (
                BpfTypeFilter::default(),
                EbpfHelperDefinition {
                    index: bpf_func_id_BPF_FUNC_probe_read_user,
                    name: "probe_read_user",
                    signature: FunctionSignature {
                        args: vec![
                            Type::MemoryParameter {
                                size: MemoryParameterSize::Reference { index: 1 },
                                input: false,
                                output: true,
                            },
                            Type::ScalarValueParameter,
                            Type::ScalarValueParameter,
                        ],
                        return_value: Type::unknown_written_scalar_value(),
                        invalidate_array_bounds: false,
                    },
                },
            ),
            (
                BpfTypeFilter::default(),
                EbpfHelperDefinition {
                    index: bpf_func_id_BPF_FUNC_probe_read_user_str,
                    name: "probe_read_user_str",
                    signature: FunctionSignature {
                        args: vec![
                            Type::MemoryParameter {
                                size: MemoryParameterSize::Reference { index: 1 },
                                input: false,
                                output: true,
                            },
                            Type::ScalarValueParameter,
                            Type::ScalarValueParameter,
                        ],
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
                EbpfHelperDefinition {
                    index: bpf_func_id_BPF_FUNC_get_socket_uid,
                    name: "get_socket_uid",
                    signature: FunctionSignature {
                        args: vec![Type::StructParameter { id: SK_BUF_ID.clone() }],
                        return_value: Type::unknown_written_scalar_value(),
                        invalidate_array_bounds: false,
                    },
                },
            ),
            (
                vec![
                    ProgramType::KProbe,
                    ProgramType::TracePoint,
                    ProgramType::CgroupSock,
                    ProgramType::CgroupSockopt,
                    ProgramType::CgroupSockAddr,
                ]
                .into(),
                EbpfHelperDefinition {
                    index: bpf_func_id_BPF_FUNC_get_current_uid_gid,
                    name: "get_current_uid_gid",
                    signature: FunctionSignature {
                        args: vec![],
                        return_value: Type::unknown_written_scalar_value(),
                        invalidate_array_bounds: false,
                    },
                },
            ),
            (
                vec![ProgramType::SchedAct, ProgramType::SchedCls].into(),
                EbpfHelperDefinition {
                    index: bpf_func_id_BPF_FUNC_skb_pull_data,
                    name: "skb_pull_data",
                    signature: FunctionSignature {
                        args: vec![
                            Type::StructParameter { id: SK_BUF_ID.clone() },
                            Type::ScalarValueParameter,
                        ],
                        return_value: Type::unknown_written_scalar_value(),
                        invalidate_array_bounds: true,
                    },
                },
            ),
            (
                BpfTypeFilter::default(),
                EbpfHelperDefinition {
                    index: bpf_func_id_BPF_FUNC_ringbuf_reserve,
                    name: "ringbuf_reserve",
                    signature: FunctionSignature {
                        args: vec![
                            Type::ConstPtrToMapParameter,
                            Type::ScalarValueParameter,
                            Type::ScalarValueParameter,
                        ],
                        return_value: Type::NullOrParameter(Box::new(Type::ReleasableParameter {
                            id: RING_BUFFER_RESERVATION.clone(),
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
                EbpfHelperDefinition {
                    index: bpf_func_id_BPF_FUNC_ringbuf_submit,
                    name: "ringbuf_submit",
                    signature: FunctionSignature {
                        args: vec![
                            Type::ReleaseParameter { id: RING_BUFFER_RESERVATION.clone() },
                            Type::ScalarValueParameter,
                        ],
                        return_value: Type::default(),
                        invalidate_array_bounds: false,
                    },
                },
            ),
            (
                BpfTypeFilter::default(),
                EbpfHelperDefinition {
                    index: bpf_func_id_BPF_FUNC_ringbuf_discard,
                    name: "ringbuf_discard",
                    signature: FunctionSignature {
                        args: vec![
                            Type::ReleaseParameter { id: RING_BUFFER_RESERVATION.clone() },
                            Type::ScalarValueParameter,
                        ],
                        return_value: Type::default(),
                        invalidate_array_bounds: false,
                    },
                },
            ),
            (
                vec![ProgramType::SchedAct, ProgramType::SchedCls].into(),
                EbpfHelperDefinition {
                    index: bpf_func_id_BPF_FUNC_skb_change_proto,
                    name: "skb_change_proto",
                    signature: FunctionSignature {
                        args: vec![
                            Type::StructParameter { id: SK_BUF_ID.clone() },
                            Type::ScalarValueParameter,
                            Type::ScalarValueParameter,
                        ],
                        return_value: Type::unknown_written_scalar_value(),
                        invalidate_array_bounds: true,
                    },
                },
            ),
            (
                vec![ProgramType::SchedAct, ProgramType::SchedCls].into(),
                EbpfHelperDefinition {
                    index: bpf_func_id_BPF_FUNC_csum_update,
                    name: "csum_update",
                    signature: FunctionSignature {
                        args: vec![
                            Type::StructParameter { id: SK_BUF_ID.clone() },
                            Type::ScalarValueParameter,
                        ],
                        return_value: Type::unknown_written_scalar_value(),
                        invalidate_array_bounds: false,
                    },
                },
            ),
            (
                vec![ProgramType::KProbe, ProgramType::TracePoint].into(),
                EbpfHelperDefinition {
                    index: bpf_func_id_BPF_FUNC_probe_read_str,
                    name: "probe_read_str",
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
                EbpfHelperDefinition {
                    index: bpf_func_id_BPF_FUNC_get_socket_cookie,
                    name: "get_socket_cookie",
                    signature: FunctionSignature {
                        args: vec![Type::StructParameter { id: SK_BUF_ID.clone() }],
                        return_value: Type::unknown_written_scalar_value(),
                        invalidate_array_bounds: false,
                    },
                },
            ),
            (
                vec![ProgramType::CgroupSock].into(),
                EbpfHelperDefinition {
                    index: bpf_func_id_BPF_FUNC_get_socket_cookie,
                    name: "get_socket_cookie",
                    signature: FunctionSignature {
                        args: vec![Type::StructParameter { id: BPF_SOCK_ID.clone() }],
                        return_value: Type::unknown_written_scalar_value(),
                        invalidate_array_bounds: false,
                    },
                },
            ),
            (
                vec![ProgramType::SchedAct, ProgramType::SchedCls].into(),
                EbpfHelperDefinition {
                    index: bpf_func_id_BPF_FUNC_redirect,
                    name: "redirect",
                    signature: FunctionSignature {
                        args: vec![Type::ScalarValueParameter, Type::ScalarValueParameter],
                        return_value: Type::unknown_written_scalar_value(),
                        invalidate_array_bounds: false,
                    },
                },
            ),
            (
                vec![ProgramType::SchedAct, ProgramType::SchedCls].into(),
                EbpfHelperDefinition {
                    index: bpf_func_id_BPF_FUNC_skb_adjust_room,
                    name: "skb_adjust_room",
                    signature: FunctionSignature {
                        args: vec![
                            Type::StructParameter { id: SK_BUF_ID.clone() },
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
                vec![ProgramType::SchedAct, ProgramType::SchedCls].into(),
                EbpfHelperDefinition {
                    index: bpf_func_id_BPF_FUNC_l3_csum_replace,
                    name: "l3_csum_replace",
                    signature: FunctionSignature {
                        args: vec![
                            Type::StructParameter { id: SK_BUF_ID.clone() },
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
                vec![ProgramType::SchedAct, ProgramType::SchedCls].into(),
                EbpfHelperDefinition {
                    index: bpf_func_id_BPF_FUNC_l4_csum_replace,
                    name: "l4_csum_replace",
                    signature: FunctionSignature {
                        args: vec![
                            Type::StructParameter { id: SK_BUF_ID.clone() },
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
                vec![ProgramType::SchedAct, ProgramType::SchedCls].into(),
                EbpfHelperDefinition {
                    index: bpf_func_id_BPF_FUNC_skb_store_bytes,
                    name: "skb_store_bytes",
                    signature: FunctionSignature {
                        args: vec![
                            Type::StructParameter { id: SK_BUF_ID.clone() },
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
                vec![ProgramType::SchedAct, ProgramType::SchedCls].into(),
                EbpfHelperDefinition {
                    index: bpf_func_id_BPF_FUNC_skb_change_head,
                    name: "skb_change_head",
                    signature: FunctionSignature {
                        args: vec![
                            Type::StructParameter { id: SK_BUF_ID.clone() },
                            Type::ScalarValueParameter,
                            Type::ScalarValueParameter,
                        ],
                        return_value: Type::unknown_written_scalar_value(),
                        invalidate_array_bounds: true,
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
                EbpfHelperDefinition {
                    index: bpf_func_id_BPF_FUNC_skb_load_bytes_relative,
                    name: "skb_load_bytes_relative",
                    signature: FunctionSignature {
                        args: vec![
                            Type::StructParameter { id: SK_BUF_ID.clone() },
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
                EbpfHelperDefinition {
                    index: bpf_func_id_BPF_FUNC_ktime_get_boot_ns,
                    name: "ktime_get_boot_ns",
                    signature: FunctionSignature {
                        args: vec![],
                        return_value: Type::unknown_written_scalar_value(),
                        invalidate_array_bounds: false,
                    },
                },
            ),
            (
                BpfTypeFilter::default(),
                EbpfHelperDefinition {
                    index: bpf_func_id_BPF_FUNC_ktime_get_coarse_ns,
                    name: "ktime_get_coarse_ns",
                    signature: FunctionSignature {
                        args: vec![],
                        return_value: Type::unknown_written_scalar_value(),
                        invalidate_array_bounds: false,
                    },
                },
            ),
        ]
    });

fn scalar_field(offset: usize, size: usize) -> FieldDescriptor {
    FieldDescriptor { offset, field_type: FieldType::Scalar { size } }
}

fn scalar_mut_field(offset: usize, size: usize) -> FieldDescriptor {
    FieldDescriptor { offset, field_type: FieldType::MutableScalar { size } }
}

fn scalar_u32_field(offset: usize) -> FieldDescriptor {
    FieldDescriptor { offset, field_type: FieldType::Scalar { size: std::mem::size_of::<u32>() } }
}

fn array_start_32_field(offset: usize, id: MemoryId) -> FieldDescriptor {
    FieldDescriptor { offset, field_type: FieldType::PtrToArray { id, is_32_bit: true } }
}

fn array_end_32_field(offset: usize, id: MemoryId) -> FieldDescriptor {
    FieldDescriptor { offset, field_type: FieldType::PtrToArray { id, is_32_bit: true } }
}

fn ptr_to_struct_type(id: MemoryId, fields: Vec<FieldDescriptor>) -> Type {
    Type::PtrToStruct { id, offset: 0, descriptor: Arc::new(StructDescriptor { fields }) }
}

fn ptr_to_mem_type<T: IntoBytes>(id: MemoryId) -> Type {
    Type::PtrToMemory { id, offset: 0, buffer_size: std::mem::size_of::<T>() as u64 }
}

static RING_BUFFER_RESERVATION: LazyLock<MemoryId> = LazyLock::new(MemoryId::new);

pub static SK_BUF_ID: LazyLock<MemoryId> = LazyLock::new(MemoryId::new);
pub static SK_BUF_TYPE: LazyLock<Type> = LazyLock::new(|| {
    let cb_offset = std::mem::offset_of!(__sk_buff, cb);
    let hash_offset = std::mem::offset_of!(__sk_buff, hash);
    let data_id = MemoryId::new();

    ptr_to_struct_type(
        SK_BUF_ID.clone(),
        vec![
            // All fields from the start of `__sk_buff` to `cb` are read-only scalars.
            scalar_field(0, cb_offset),
            // `cb` is a mutable array.
            scalar_mut_field(cb_offset, hash_offset - cb_offset),
            scalar_u32_field(std::mem::offset_of!(__sk_buff, hash)),
            scalar_u32_field(std::mem::offset_of!(__sk_buff, napi_id)),
            scalar_u32_field(std::mem::offset_of!(__sk_buff, tstamp)),
            scalar_u32_field(std::mem::offset_of!(__sk_buff, gso_segs)),
            scalar_u32_field(std::mem::offset_of!(__sk_buff, gso_size)),
            array_start_32_field(std::mem::offset_of!(__sk_buff, data), data_id.clone()),
            array_end_32_field(std::mem::offset_of!(__sk_buff, data_end), data_id),
        ],
    )
});
pub static SK_BUF_ARGS: LazyLock<Vec<Type>> = LazyLock::new(|| vec![SK_BUF_TYPE.clone()]);

static XDP_MD_ID: LazyLock<MemoryId> = LazyLock::new(MemoryId::new);
static XDP_MD_TYPE: LazyLock<Type> = LazyLock::new(|| {
    let data_id = MemoryId::new();

    ptr_to_struct_type(
        XDP_MD_ID.clone(),
        vec![
            array_start_32_field(std::mem::offset_of!(xdp_md, data), data_id.clone()),
            array_end_32_field(std::mem::offset_of!(xdp_md, data_end), data_id),
            // All fields starting from `data_meta` are readable.
            {
                let data_meta_offset = std::mem::offset_of!(xdp_md, data_meta);
                scalar_field(data_meta_offset, std::mem::size_of::<xdp_md>() - data_meta_offset)
            },
        ],
    )
});
static XDP_MD_ARGS: LazyLock<Vec<Type>> = LazyLock::new(|| vec![XDP_MD_TYPE.clone()]);

static BPF_USER_PT_REGS_T_ID: LazyLock<MemoryId> = LazyLock::new(MemoryId::new);
static BPF_USER_PT_REGS_T_ARGS: LazyLock<Vec<Type>> =
    LazyLock::new(|| vec![ptr_to_mem_type::<bpf_user_pt_regs_t>(BPF_USER_PT_REGS_T_ID.clone())]);

static BPF_SOCK_ID: LazyLock<MemoryId> = LazyLock::new(MemoryId::new);
static BPF_SOCK_ARGS: LazyLock<Vec<Type>> =
    LazyLock::new(|| vec![ptr_to_mem_type::<bpf_sock>(BPF_SOCK_ID.clone())]);

static BPF_SOCKOPT_ID: LazyLock<MemoryId> = LazyLock::new(MemoryId::new);
static BPF_SOCKOPT_ARGS: LazyLock<Vec<Type>> =
    LazyLock::new(|| vec![ptr_to_mem_type::<bpf_sockopt>(BPF_SOCKOPT_ID.clone())]);

static BPF_SOCK_ADDR_ID: LazyLock<MemoryId> = LazyLock::new(MemoryId::new);
static BPF_SOCK_ADDR_ARGS: LazyLock<Vec<Type>> =
    LazyLock::new(|| vec![ptr_to_mem_type::<bpf_sock_addr>(BPF_SOCK_ADDR_ID.clone())]);

static BPF_FUSE_ID: LazyLock<MemoryId> = LazyLock::new(MemoryId::new);
static BPF_FUSE_TYPE: LazyLock<Type> = LazyLock::new(|| {
    ptr_to_struct_type(
        BPF_FUSE_ID.clone(),
        vec![
            FieldDescriptor {
                offset: (std::mem::offset_of!(fuse_bpf_args, out_args)
                    + std::mem::offset_of!(fuse_bpf_arg, value)),
                field_type: FieldType::PtrToMemory {
                    id: MemoryId::new(),
                    buffer_size: std::mem::size_of::<fuse_entry_out>(),
                    is_32_bit: false,
                },
            },
            FieldDescriptor {
                offset: (std::mem::offset_of!(fuse_bpf_args, out_args)
                    + std::mem::size_of::<fuse_bpf_arg>()
                    + std::mem::offset_of!(fuse_bpf_arg, value)),
                field_type: FieldType::PtrToMemory {
                    id: MemoryId::new(),
                    buffer_size: std::mem::size_of::<fuse_entry_bpf_out>(),
                    is_32_bit: false,
                },
            },
        ],
    )
});
static BPF_FUSE_ARGS: LazyLock<Vec<Type>> = LazyLock::new(|| vec![BPF_FUSE_TYPE.clone()]);

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

static BPF_TRACEPOINT_ID: LazyLock<MemoryId> = LazyLock::new(MemoryId::new);
static BPF_TRACEPOINT_ARGS: LazyLock<Vec<Type>> =
    LazyLock::new(|| vec![ptr_to_mem_type::<TraceEvent>(BPF_TRACEPOINT_ID.clone())]);

/// The different type of BPF programs.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ProgramType {
    SocketFilter,
    KProbe,
    SchedCls,
    SchedAct,
    TracePoint,
    Xdp,
    CgroupSkb,
    CgroupSock,
    CgroupSockopt,
    CgroupSockAddr,
    /// Custom id for Fuse
    Fuse,
}

impl TryFrom<u32> for ProgramType {
    type Error = EbpfError;

    fn try_from(program_type: u32) -> Result<Self, Self::Error> {
        match program_type {
            #![allow(non_upper_case_globals)]
            bpf_prog_type_BPF_PROG_TYPE_SOCKET_FILTER => Ok(Self::SocketFilter),
            bpf_prog_type_BPF_PROG_TYPE_KPROBE => Ok(Self::KProbe),
            bpf_prog_type_BPF_PROG_TYPE_SCHED_CLS => Ok(Self::SchedCls),
            bpf_prog_type_BPF_PROG_TYPE_SCHED_ACT => Ok(Self::SchedAct),
            bpf_prog_type_BPF_PROG_TYPE_TRACEPOINT => Ok(Self::TracePoint),
            bpf_prog_type_BPF_PROG_TYPE_XDP => Ok(Self::Xdp),
            bpf_prog_type_BPF_PROG_TYPE_CGROUP_SKB => Ok(Self::CgroupSkb),
            bpf_prog_type_BPF_PROG_TYPE_CGROUP_SOCK => Ok(Self::CgroupSock),
            bpf_prog_type_BPF_PROG_TYPE_CGROUP_SOCKOPT => Ok(Self::CgroupSockopt),
            bpf_prog_type_BPF_PROG_TYPE_CGROUP_SOCK_ADDR => Ok(Self::CgroupSockAddr),
            BPF_PROG_TYPE_FUSE => Ok(Self::Fuse),
            program_type @ _ => Err(EbpfError::UnsupportedProgramType(program_type)),
        }
    }
}

impl From<ProgramType> for u32 {
    fn from(program_type: ProgramType) -> u32 {
        match program_type {
            ProgramType::SocketFilter => bpf_prog_type_BPF_PROG_TYPE_SOCKET_FILTER,
            ProgramType::KProbe => bpf_prog_type_BPF_PROG_TYPE_KPROBE,
            ProgramType::SchedCls => bpf_prog_type_BPF_PROG_TYPE_SCHED_CLS,
            ProgramType::SchedAct => bpf_prog_type_BPF_PROG_TYPE_SCHED_ACT,
            ProgramType::TracePoint => bpf_prog_type_BPF_PROG_TYPE_TRACEPOINT,
            ProgramType::Xdp => bpf_prog_type_BPF_PROG_TYPE_XDP,
            ProgramType::CgroupSkb => bpf_prog_type_BPF_PROG_TYPE_CGROUP_SKB,
            ProgramType::CgroupSock => bpf_prog_type_BPF_PROG_TYPE_CGROUP_SOCK,
            ProgramType::CgroupSockopt => bpf_prog_type_BPF_PROG_TYPE_CGROUP_SOCKOPT,
            ProgramType::CgroupSockAddr => bpf_prog_type_BPF_PROG_TYPE_CGROUP_SOCK_ADDR,
            ProgramType::Fuse => BPF_PROG_TYPE_FUSE,
        }
    }
}

impl ProgramType {
    pub fn get_helpers(self) -> HashMap<u32, FunctionSignature> {
        BPF_HELPERS_DEFINITIONS
            .iter()
            .filter_map(|(filter, helper)| {
                filter.accept(self).then_some((helper.index, helper.signature.clone()))
            })
            .collect()
    }

    pub fn get_args(self) -> &'static [Type] {
        match self {
            Self::CgroupSkb | Self::SchedAct | Self::SchedCls | Self::SocketFilter => &SK_BUF_ARGS,
            Self::Xdp => &XDP_MD_ARGS,
            Self::KProbe => &BPF_USER_PT_REGS_T_ARGS,
            Self::TracePoint => &BPF_TRACEPOINT_ARGS,
            Self::CgroupSock => &BPF_SOCK_ARGS,
            Self::CgroupSockopt => &BPF_SOCKOPT_ARGS,
            Self::CgroupSockAddr => &BPF_SOCK_ADDR_ARGS,
            Self::Fuse => &BPF_FUSE_ARGS,
        }
    }

    pub fn create_calling_context(self, maps: Vec<MapSchema>) -> CallingContext {
        let args = self.get_args().to_vec();
        let packet_type = match self {
            Self::CgroupSkb | Self::SchedAct | Self::SchedCls | Self::SocketFilter => {
                Some(SK_BUF_TYPE.clone())
            }
            _ => None,
        };
        CallingContext { maps, helpers: self.get_helpers(), args, packet_type }
    }
}
