// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use ffx_e2e_emu::IsolatedEmulator;
use ffx_symbolize::{MappingDetails, MappingFlags, Resolver};
use tracing::info;

mod shared;

use shared::*;

#[fuchsia::test]
async fn symbolize_fn_ptr() {
    info!("starting emulator...");
    let emu = IsolatedEmulator::start("test-ffx-symbolize-lib").await.unwrap();
    let stdout = emu
        .ffx_output(&[
            // JSON output prevents the command from printing its status messages to stdout.
            "--machine",
            "json",
            "component",
            "run",
            "/core/ffx-laboratory:print-fn-ptr",
            "fuchsia-pkg://fuchsia.com/print_fn_ptr#meta/print_fn_ptr.cm",
            // Ensure we get the component's stdout to the ffx command.
            "--connect-stdio",
        ])
        .await
        .unwrap();
    let outputs: SymbolizationTestOutputs = serde_json::from_str(&stdout).unwrap();

    let mut resolver = Resolver::with_context(emu.env_context()).await.unwrap();

    for Module { name, build_id, mappings } in outputs.modules {
        let id = resolver.add_module(&name, &build_id);
        for mapping in mappings {
            let mut flags = MappingFlags::empty();
            if mapping.readable {
                flags |= MappingFlags::READ;
            }
            if mapping.writeable {
                flags |= MappingFlags::WRITE;
            }
            if mapping.executable {
                flags |= MappingFlags::EXECUTE;
            }
            resolver.add_mapping(
                id,
                MappingDetails {
                    start_addr: mapping.start_addr,
                    size: mapping.size,
                    vaddr: mapping.vaddr,
                    flags,
                },
            );
        }
    }

    let symbol_one = resolver.resolve_addr(outputs.fn_one_addr).unwrap();
    assert_eq!(symbol_one.addr, outputs.fn_one_addr);
    assert_eq!(symbol_one.locations.len(), 1);
    let location_one = &symbol_one.locations[0];
    assert_eq!(location_one.function, "print_fn_ptr_bin::to_be_symbolized_one()");
    assert_eq!(location_one.file, "../../src/developer/ffx/lib/symbolize/tests/print_fn_ptr.rs");
    assert!(
        outputs.fn_one_source_line - 2 <= location_one.line
            || location_one.line <= outputs.fn_one_source_line + 2
    );
    assert_eq!(location_one.library, None);

    let symbol_two = resolver.resolve_addr(outputs.fn_two_addr).unwrap();
    assert_eq!(symbol_two.addr, outputs.fn_two_addr);
    assert_eq!(symbol_two.locations.len(), 1);
    let location_two = &symbol_two.locations[0];
    assert_eq!(location_two.function, "print_fn_ptr_bin::to_be_symbolized_two()");
    assert_eq!(location_two.file, "../../src/developer/ffx/lib/symbolize/tests/print_fn_ptr.rs");
    assert!(
        outputs.fn_two_source_line - 2 <= location_two.line
            || location_two.line <= outputs.fn_two_source_line + 2
    );
    assert_eq!(location_two.library, None);

    let libc_symbol = resolver.resolve_addr(outputs.libc_addr).unwrap();
    assert_eq!(libc_symbol.addr, outputs.libc_addr);
    assert_eq!(libc_symbol.locations.len(), 1);
    let libc_location = &libc_symbol.locations[0];
    assert_eq!(libc_location.function, "open(const char*, int)");
    assert_eq!(libc_location.file, "../../sdk/lib/fdio/unistd.cc");
    assert_eq!(libc_location.library, Some("libfdio.so".to_string()));

    // Make sure the batch API returns things in the expected order.
    let batch_symbols = resolver
        .resolve_addresses(&[outputs.fn_one_addr, outputs.fn_two_addr, outputs.libc_addr])
        .unwrap();
    assert_eq!(batch_symbols[0], symbol_one);
    assert_eq!(batch_symbols[1], symbol_two);
    assert_eq!(batch_symbols[2], libc_symbol);
}
