// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::builtin::runner::BuiltinRunnerFactory, ::routing::policy::ScopedPolicyChecker,
    elf_runner::ElfRunner, fidl::endpoints::ServerEnd, fidl_fuchsia_component_runner as fcrunner,
    std::sync::Arc,
};

impl BuiltinRunnerFactory for ElfRunner {
    fn get_scoped_runner(
        self: Arc<Self>,
        checker: ScopedPolicyChecker,
        server_end: ServerEnd<fcrunner::ComponentRunnerMarker>,
    ) {
        self.get_scoped_runner(checker).serve(server_end.into_stream().unwrap());
    }
}
