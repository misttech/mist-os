// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use criterion::{Benchmark, Criterion};
use ebpf::{EbpfProgramContext, FieldMapping, ProgramArgument, StructMapping};
use ebpf_api::{
    AttachType, LoadBytesBase, Map, MapValueRef, PinnedMap, ProgramType, SocketFilterContext,
    __sk_buff, uid_t, SocketCookieContext, CGROUP_SKB_SK_BUF_TYPE, SK_BUF_ID,
};
use fuchsia_criterion::FuchsiaCriterion;
use std::sync::LazyLock;
use std::time::Duration;
use structopt::StructOpt;

/// Benchmark configuration passed from the manifest file through the args.
#[derive(StructOpt, Debug)]
struct BenchmarkConfig {
    #[structopt(long, required = true)]
    file: String,
    #[structopt(long)]
    section: Vec<String>,
    #[structopt(long)]
    name: Vec<String>,
}

struct TestEbpfContext {}

impl EbpfProgramContext for TestEbpfContext {
    type RunContext<'a> = TestEbpfRunContext<'a>;
    type Packet<'a> = ();
    type Map = PinnedMap;

    type Arg1<'a> = &'a SkBuff<'a>;
    type Arg2<'a> = ();
    type Arg3<'a> = ();
    type Arg4<'a> = ();
    type Arg5<'a> = ();
}

#[repr(C)]
struct SkBuff<'a> {
    sk_buff: __sk_buff,

    data_ptr: *const u8,
    data_end_ptr: *const u8,

    uid: u32,
    socket_cookie: u64,
    data: &'a [u8],
}

impl ProgramArgument for &'_ SkBuff<'_> {
    fn get_type() -> &'static ebpf::Type {
        &*CGROUP_SKB_SK_BUF_TYPE
    }
}

static SK_BUF_MAPPING: LazyLock<StructMapping> = LazyLock::new(|| StructMapping {
    memory_id: SK_BUF_ID.clone(),
    fields: vec![
        FieldMapping {
            source_offset: std::mem::offset_of!(__sk_buff, data),
            target_offset: std::mem::offset_of!(SkBuff<'_>, data_ptr),
        },
        FieldMapping {
            source_offset: std::mem::offset_of!(__sk_buff, data_end),
            target_offset: std::mem::offset_of!(SkBuff<'_>, data_end_ptr),
        },
    ],
});

#[derive(Default)]
struct TestEbpfRunContext<'a> {
    map_refs: Vec<MapValueRef<'a>>,
}

impl<'a> ebpf_api::MapsContext<'a> for TestEbpfRunContext<'a> {
    fn add_value_ref(&mut self, map_ref: MapValueRef<'a>) {
        self.map_refs.push(map_ref)
    }
}

impl<'a, 'b> SocketCookieContext<&'a SkBuff<'a>> for TestEbpfRunContext<'b> {
    fn get_socket_cookie(&self, sk_buf: &'a SkBuff<'a>) -> u64 {
        sk_buf.socket_cookie
    }
}

impl<'a, 'b> SocketFilterContext<&'a SkBuff<'a>> for TestEbpfRunContext<'b> {
    fn get_socket_uid(&self, sk_buf: &'a SkBuff<'a>) -> Option<uid_t> {
        Some(sk_buf.uid)
    }

    fn load_bytes_relative(
        &self,
        sk_buf: &'a SkBuff<'a>,
        base: LoadBytesBase,
        offset: usize,
        buf: &mut [u8],
    ) -> i64 {
        if base != LoadBytesBase::NetworkHeader {
            return -1;
        }

        let Some(data) = sk_buf.data.get(offset..(offset + buf.len())) else {
            return -1;
        };

        buf.copy_from_slice(data);
        0
    }
}

fn exit_on_error(msg: String) -> ! {
    eprintln!("{}", msg);
    std::process::exit(1);
}

// Fake IPv6 TCP packet.
const TEST_PACKET: [u8; 60] = [
    0x60, 0x00, 0x00, 0x00, 0x00, 0x1a, 0x06, 0x00, 0x20, 0x01, 0x0d, 0xb8, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x20, 0x01, 0x0d, 0xb8, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x02, 0x04, 0xd2, 0x16, 0x2e, 0x00, 0x00, 0x00, 0x21,
    0x00, 0x00, 0x00, 0x00, 0x50, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00,
];

fn main() {
    // List of benchmark programs is passed as the argument list from the
    // component manifest. The arguments passed by the test executor are
    // separated from the arguments in the manifest file by adding "--" at
    // the end of the argument list in the manifest file.
    let mut args: Vec<_> = std::env::args().collect();
    let Some(separator_pos) = args.iter().position(|s| s == "--") else {
        eprintln!("{:?}", args);
        exit_on_error("-- not found in the argument list".to_string());
    };

    // Replace separate with the program name.
    args[separator_pos] = args[0].clone();

    let benchmark_args: Vec<_> = args[separator_pos..].iter().map(|s| &**s).collect();
    let mut fc = FuchsiaCriterion::fuchsia_bench_with_args(&benchmark_args[..]);
    let c: &mut Criterion = &mut fc;
    *c = std::mem::take(c)
        .warm_up_time(Duration::from_millis(1))
        .measurement_time(Duration::from_millis(100))
        .sample_size(100);

    // Parse benchmark config passed in the manifest file.
    let config = BenchmarkConfig::from_iter_safe(args[..separator_pos].iter())
        .unwrap_or_else(|e| exit_on_error(format!("Failed to parse args: {}", e)));

    if config.section.len() != config.name.len() {
        eprintln!("The same number of --section and --name arguments is expected");
        std::process::exit(1);
    }

    let elf_file = ebpf_loader::ElfFile::new(&config.file).unwrap_or_else(|e| {
        exit_on_error(format!("Failed to load ELF file {}: {}", config.file, e))
    });
    for (section, name) in config.section.iter().zip(config.name.iter()) {
        let prog = ebpf_loader::load_ebpf_program_from_file(&elf_file, section, name)
            .unwrap_or_else(|e| exit_on_error(format!("Failed to load program {}: {}", name, e)));
        let maps_schema = prog.maps.iter().map(|m| m.schema).collect();
        let calling_context = ProgramType::CgroupSkb
            .create_calling_context(AttachType::CgroupInetEgress, maps_schema)
            .expect("Failed to create CallingContext");
        let verified =
            ebpf::verify_program(prog.code, calling_context, &mut ebpf::NullVerifierLogger)
                .unwrap_or_else(|e| exit_on_error(format!("Failed to verify {}: {}", name, e)));

        let maps: Vec<_> = prog
            .maps
            .iter()
            .map(|def| {
                Map::new(def.schema, def.flags)
                    .unwrap_or_else(|e| exit_on_error(format!("Failed to create a map: {:?}", e)))
            })
            .collect();

        let helpers = ebpf_api::get_common_helpers()
            .into_iter()
            .chain(ebpf_api::get_socket_filter_helpers())
            .collect();

        let prog = ebpf::link_program::<TestEbpfContext>(
            &verified,
            &[SK_BUF_MAPPING.clone()],
            maps,
            helpers,
        )
        .unwrap_or_else(|e| exit_on_error(format!("Failed to link program {}: {}", name, e)));

        let bench = Benchmark::new(name, move |b| {
            let skb = SkBuff {
                sk_buff: __sk_buff {
                    len: TEST_PACKET.len() as u32,
                    protocol: 0xDD86, // hton(ETH_P_IPV6)
                    ifindex: 2,
                    ..__sk_buff::default()
                },

                data_ptr: TEST_PACKET.as_ptr(),
                data_end_ptr: unsafe { TEST_PACKET.as_ptr().add(TEST_PACKET.len()) },

                uid: 52342,
                socket_cookie: 0x21ab3,
                data: &TEST_PACKET,
            };

            b.iter(|| {
                let mut ctx = TestEbpfRunContext::default();
                let _r = prog.run_with_1_argument(&mut ctx, &skb);
            })
        });

        c.bench("fuchsia.ebpf", bench);
    }
}
