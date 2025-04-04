// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

pub mod dispatcher;
pub mod event;
pub mod hook_observer;
pub mod registry;
pub(crate) mod serve;
pub mod source;
pub mod source_factory;
pub mod stream;
pub mod stream_provider;
pub mod synthesizer;
pub mod use_router;

use cm_rust::DictionaryValue;
use fidl_fuchsia_component as fcomponent;
use futures::channel::mpsc;
use futures::stream::StreamExt;
use std::collections::BTreeMap;
use std::sync::Arc;

/// Forwards events from the receiver in `receiver_lock` to `sender`. See the comment on
/// `crate::model::component::instance::ResolvedInstanceState` for more information on this
/// receiver.
pub async fn forward_capability_requested_events(
    sender: mpsc::UnboundedSender<fcomponent::Event>,
    receiver_lock: Arc<futures::lock::Mutex<mpsc::UnboundedReceiver<fcomponent::Event>>>,
) {
    if let Some(mut guard) = receiver_lock.try_lock() {
        while let Some(event) = guard.next().await {
            let _ = sender.unbounded_send(event);
        }
    }
}

/// Given a filter from an `UseEventStreamDecl`, extracts the set of strings listed under the key
/// "name". Returns None if the key doesn't exist.
pub fn names_from_filter(
    filter: &Option<BTreeMap<String, DictionaryValue>>,
) -> Option<Vec<String>> {
    let names = match filter.as_ref()?.get(&"name".to_string()) {
        Some(DictionaryValue::Str(name)) => vec![name.clone()],
        Some(DictionaryValue::StrVec(names)) => names.iter().map(|n| n.clone()).collect(),
        _ => return None,
    };
    Some(names)
}
