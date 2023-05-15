// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use cml::{compile, Document};
use fidl::persist;
use serde_json::json;

fn compile_from_json(contents: serde_json::Value) -> Vec<u8> {
    let document: Document =
        serde_json::from_str(&contents.to_string()).expect("failed to parse JSON");
    let compiled = compile(&document, None).expect("failed to compile manifest");
    persist(&compiled).expect("failed to persist FIDL manifest")
}

pub fn compile_container_manifest(container_name: &str, mounts: &[&str]) -> Vec<u8> {
    compile_from_json(json!({
        "program": {
            "runner": "starnix",
            "apex_hack": [],
            "features": [],
            "init": [],
            "init_user": "root:x:0:0",
            "kernel_cmdline": "",
            "mounts": mounts,
            "name": container_name,
            "startup_file_path": "",
        },
        "collections": [
            {
                "name": "daemons",
                "environment": "#daemon-env",
                "durability": "single_run",
            },
        ],
        "capabilities": [
            {
                "runner": "starnix_container",
                "path": "/svc/fuchsia.component.runner.ComponentRunner",
            },
            {
                "protocol": [
                    "fuchsia.component.runner.ComponentRunner",
                    "fuchsia.starnix.binder.DevBinder",
                    "fuchsia.starnix.container.Controller",
                ],
            },
        ],
        "expose": [
            {
                "runner": "starnix_container",
                "from": "self",
            },
            {
                "protocol": [
                    "fuchsia.component.runner.ComponentRunner",
                    "fuchsia.starnix.container.Controller",
                ],
                "from": "self",
            },
            {
                "protocol": "fuchsia.component.Binder",
                "from": "framework",
            },
        ],
        "environments": [
            {
                "name": "daemon-env",
                "extends": "realm",
                "runners": [
                    {
                        "runner": "starnix_container",
                        "from": "self",
                    },
                ],
            },
        ],
    }))
}

pub fn compile_exec_manifest(command: &[&str], environ: &[&str]) -> Vec<u8> {
    let binary = command[0];
    let args = &command[1..];

    compile_from_json(json!({
        "program": {
            "runner": "starnix_container",
            "binary": binary,
            "args": args,
            "uid": "0",
            "environ": environ,
        },
    }))
}
