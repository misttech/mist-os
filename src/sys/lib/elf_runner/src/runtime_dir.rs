// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fidl::endpoints::ServerEnd;
use fidl_fuchsia_io as fio;
use std::sync::Arc;
use vfs::directory::entry_container::Directory;
use vfs::directory::helper::DirectlyMutable;
use vfs::directory::immutable::simple as pfs;
use vfs::execution_scope::ExecutionScope;
use vfs::file::vmo::read_only;
use vfs::object_request::ToObjectRequest as _;
use vfs::tree_builder::TreeBuilder;

// Simple directory type which is used to implement `ComponentStartInfo.runtime_directory`.
pub struct RuntimeDirectory(Arc<pfs::Simple>);

impl RuntimeDirectory {
    pub fn add_process_id(&self, process_id: u64) {
        self.elf_dir()
            .add_entry("process_id", read_only(process_id.to_string()))
            .expect("failed to add process_id");
    }

    pub fn add_process_start_time(&self, process_start_time: i64) {
        self.elf_dir()
            .add_entry("process_start_time", read_only(process_start_time.to_string()))
            .expect("failed to add process_start_time");
    }

    pub fn add_process_start_time_utc_estimate(&self, process_start_time_utc_estimate: String) {
        self.elf_dir()
            .add_entry(
                "process_start_time_utc_estimate",
                read_only(process_start_time_utc_estimate),
            )
            .expect("failed to add process_start_time_utc_estimate");
    }

    // Create an empty runtime directory, for test purpose only.
    #[cfg(test)]
    pub fn empty() -> Self {
        let mut empty = TreeBuilder::empty_dir();
        empty.add_empty_dir(["elf"]).expect("failed to add elf directory");
        RuntimeDirectory(empty.build())
    }

    fn elf_dir(&self) -> Arc<pfs::Simple> {
        self.0
            .get_entry("elf")
            .expect("elf directory should be present")
            .into_any()
            .downcast::<pfs::Simple>()
            .expect("could not downcast elf to a directory")
    }
}

pub struct RuntimeDirBuilder {
    args: Vec<String>,
    job_id: Option<u64>,
    server_end: ServerEnd<fio::NodeMarker>,
}

impl RuntimeDirBuilder {
    pub fn new(server_end: ServerEnd<fio::DirectoryMarker>) -> Self {
        // Transform the server end to speak Node protocol only
        let server_end = ServerEnd::<fio::NodeMarker>::new(server_end.into_channel());
        Self { args: vec![], job_id: None, server_end }
    }

    pub fn args(mut self, args: Vec<String>) -> Self {
        self.args = args;
        self
    }

    pub fn job_id(mut self, job_id: u64) -> Self {
        self.job_id = Some(job_id);
        self
    }

    pub fn serve(mut self) -> RuntimeDirectory {
        // Create the runtime tree structure
        //
        // runtime
        // |- args
        // |  |- 0
        // |  |- 1
        // |  \- ...
        // \- elf
        //    |- job_id
        //    \- process_id
        let mut runtime_tree_builder = TreeBuilder::empty_dir();
        let mut count: u32 = 0;
        for arg in self.args.drain(..) {
            runtime_tree_builder
                .add_entry(["args", &count.to_string()], read_only(arg))
                .expect("Failed to add arg to runtime directory");
            count += 1;
        }

        // Always add the "elf" directory so we can add process information later.
        runtime_tree_builder.add_empty_dir(["elf"]).expect("failed to add elf directory");

        if let Some(job_id) = self.job_id {
            runtime_tree_builder
                .add_entry(["elf", "job_id"], read_only(job_id.to_string()))
                .expect("Failed to add job_id to runtime/elf directory");
        }

        let runtime_directory = runtime_tree_builder.build();

        const SERVE_FLAGS: fio::Flags = fio::PERM_READABLE.union(fio::PERM_WRITABLE);

        // Serve the runtime directory
        let object_request = SERVE_FLAGS.to_object_request(self.server_end.into_channel());
        let dir = runtime_directory.clone();
        object_request.handle(|request| {
            dir.open3(ExecutionScope::new(), vfs::Path::dot(), SERVE_FLAGS, request)
        });
        RuntimeDirectory(runtime_directory)
    }
}
