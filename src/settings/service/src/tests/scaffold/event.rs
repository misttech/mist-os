// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

pub(crate) mod subscriber {
    use crate::{event, service};
    use futures::future::LocalBoxFuture;
    use std::sync::Arc;

    type Generate = Arc<dyn Fn(service::message::Delegate) -> LocalBoxFuture<'static, ()>>;

    /// This blueprint provides a way for tests to specify an asynchronous
    /// closure as the create function for an event subscriber.
    pub(crate) struct Blueprint {
        generate: Generate,
    }

    impl Blueprint {
        pub(crate) fn create(generate: Generate) -> event::subscriber::BlueprintHandle {
            Arc::new(Self { generate })
        }
    }

    impl event::subscriber::Blueprint for Blueprint {
        fn create(&self, delegate: service::message::Delegate) -> LocalBoxFuture<'static, ()> {
            (self.generate)(delegate)
        }
    }
}
