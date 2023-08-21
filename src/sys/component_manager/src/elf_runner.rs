// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::builtin::runner::BuiltinRunnerFactory, crate::runner::builtin::RemoteRunner,
    crate::runner::Runner, ::routing::policy::ScopedPolicyChecker, elf_runner::ElfRunner,
    fidl_fuchsia_component_runner as fcrunner, std::sync::Arc,
};

impl BuiltinRunnerFactory for ElfRunner {
    fn get_scoped_runner(self: Arc<Self>, checker: ScopedPolicyChecker) -> Arc<dyn Runner> {
        let (proxy, stream) =
            fidl::endpoints::create_proxy_and_stream::<fcrunner::ComponentRunnerMarker>().unwrap();
        self.get_scoped_runner(checker).serve(stream);
        Arc::new(RemoteRunner::new(proxy))
    }
}
