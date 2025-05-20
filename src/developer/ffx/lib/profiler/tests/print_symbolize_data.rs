// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use symbolize_test_utils::collector::collect_modules;
use symbolize_test_utils::{Mapping, Module};

fn main() {
    let mark_up_data = generate_unsymbolized_profile_data();
    println!("{}", serde_json::to_string_pretty(&mark_up_data).unwrap());
}

fn generate_unsymbolized_profile_data() -> String {
    let mut mark_up_data = String::new();
    mark_up_data.push_str("{{{reset}}}\n");
    let module_with_mapping_data = generate_markup_module_mapping_data(collect_modules());
    mark_up_data.push_str(&module_with_mapping_data);
    //push a pid and a tid to the profiler data
    mark_up_data.push_str("12345\n");
    mark_up_data.push_str("54321\n");
    mark_up_data.push_str(&generate_bt_data().as_str());
    mark_up_data
}

fn generate_bt_data() -> String {
    let addrs = get_function_addr();
    let mut bt_list = String::new();
    for addr in addrs {
        let mut bt = String::new();
        bt.push_str("{{{bt:0:");
        bt.push_str(&format!("0x{:x}", addr));
        bt.push_str(format!(":ra}}}}}}\n").as_str());
        bt_list.push_str(bt.as_str());
    }
    bt_list
}

macro_rules! define_to_be_symbolized_function {
    ($t:expr) => {
        ::paste::paste! {
            pub fn [<to_be_symbolized_ $t>]() {
                println!(stringify!($t));
            }
        }
    };
}

fn get_function_addr() -> Vec<u64> {
    let mut result = vec![];
    define_to_be_symbolized_function!(1);
    define_to_be_symbolized_function!(2);
    define_to_be_symbolized_function!(3);
    define_to_be_symbolized_function!(4);
    define_to_be_symbolized_function!(5);
    result.push(to_be_symbolized_1 as *const () as u64 + 1);
    result.push(to_be_symbolized_2 as *const () as u64 + 1);
    result.push(to_be_symbolized_3 as *const () as u64 + 1);
    result.push(to_be_symbolized_4 as *const () as u64 + 1);
    result.push(to_be_symbolized_5 as *const () as u64 + 1);
    result.push(zx::sys::zx_channel_create as *const () as u64 + 1);
    result
}

fn generate_markup_module_mapping_data(modules_with_mapping_list: Vec<Module>) -> String {
    let mut mark_up_data = String::new();
    for module in modules_with_mapping_list {
        mark_up_data.push_str(&generate_module_mark_up_data(&module));
        mark_up_data.push_str(&generate_mapping_mark_up_data(module.mappings));
    }
    mark_up_data
}

fn generate_mapping_mark_up_data(mapping_list: Vec<Mapping>) -> String {
    let mut mark_up_data = String::new();
    for mapping in mapping_list {
        // example data: {{{mmap:0x7acba69d5000:0x5a000:load:1:rx:0x1000}}}
        let mut mmap_data = String::new();
        mmap_data.push_str("{{{mmap:");
        mmap_data.push_str(&format!("0x{:x}", mapping.start_addr));
        mmap_data.push_str(":");
        mmap_data.push_str(&format!("0x{:x}", mapping.size));
        mmap_data.push_str(":load:1:");
        if mapping.readable {
            mmap_data.push_str("r");
        }
        if mapping.writeable {
            mmap_data.push_str("w");
        }
        if mapping.executable {
            mmap_data.push_str("x");
        }
        mmap_data.push_str(":");
        mmap_data.push_str(&format!("0x{:x}", mapping.vaddr));
        mmap_data.push_str(format!("}}}}}}\n").as_str());
        mark_up_data.push_str(mmap_data.as_str());
    }
    mark_up_data
}

fn generate_module_mark_up_data(module: &Module) -> String {
    // {{{module:1:libc.so:elf:83238ab56ba10497}}}
    let mut module_data = String::new();
    module_data.push_str("{{{module:1:");
    module_data.push_str(module.name.as_str());
    module_data.push_str(":elf:");
    module_data.push_str(hex::encode(module.build_id.clone()).as_str());
    module_data.push_str(format!("}}}}}}\n").as_str());
    module_data
}
