// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

fn main() {
    let binary = std::env::args().next().expect("missing first argument");
    let binary = std::path::Path::new(&binary)
        .file_name()
        .expect("missing binary file name")
        .to_str()
        .expect("failed to get file name str");
    match binary {
        "dhcp_client" => dhcp_client::main(),
        "dhcpv4_server" => dhcpv4_server::main().expect("dhcpv4_server exited with an error"),
        "dhcpv6_client" => dhcpv6_client::main().expect("dhcpv6_client exited with an error"),
        "dns_resolver" => dns_resolver::main().expect("dns_resolver exited with an error"),
        "http_client" => http_client::main().expect("http_client exited with an error"),
        "netcfg_basic" => netcfg_basic::main(),
        "netstack3" => netstack3::main(),
        "reachability" => reachability::main(),
        "stack_migration" => stack_migration::main(),
        x => panic!("unknown binary {x}"),
    }
}
