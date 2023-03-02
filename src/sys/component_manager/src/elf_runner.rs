// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::builtin::runner::BuiltinRunnerFactory, ::routing::policy::ScopedPolicyChecker,
    cm_runner::Runner, elf_runner::ElfRunner, std::sync::Arc,
};

impl BuiltinRunnerFactory for ElfRunner {
    fn get_scoped_runner(self: Arc<Self>, checker: ScopedPolicyChecker) -> Arc<dyn Runner> {
        self.get_scoped_runner(checker)
    }
}
