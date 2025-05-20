// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{format_err, Result};
use async_utils::hanging_get::client::HangingGetStream;
use fuchsia_sync::Mutex;
use futures::{FutureExt, StreamExt};
use std::collections::HashMap;
use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use {fidl_fuchsia_bluetooth_hfp as hfp, fuchsia_async as fasync};

pub type LocalCallId = u64;

#[derive(Clone)]
pub struct CallInfo {
    pub local_id: LocalCallId,

    number: String,
    direction: hfp::CallDirection,
    state: hfp::CallState,

    #[allow(unused)]
    pub proxy: hfp::CallProxy,
}

pub struct Call {
    pub info: CallInfo,
    task: fasync::Task<LocalCallId>,
}

struct CallProxyTask {
    local_id: LocalCallId,

    calls: Arc<Mutex<HashMap<LocalCallId, Call>>>,
    proxy: hfp::CallProxy,
}

impl Future for Call {
    type Output = LocalCallId;

    fn poll(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        self.task.poll_unpin(context)
    }
}

impl fmt::Debug for CallInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "call {}: [ number: {}, direction: {:?}, state: {:?} ]",
            self.local_id, self.number, self.direction, self.state,
        )
    }
}

impl fmt::Debug for Call {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}", self.info)
    }
}

impl Call {
    pub fn new(
        local_id: LocalCallId,
        number: String,
        direction: hfp::CallDirection,
        state: hfp::CallState,
        calls: Arc<Mutex<HashMap<LocalCallId, Call>>>,
        proxy: hfp::CallProxy,
    ) -> Call {
        let call_task = CallProxyTask { local_id, calls, proxy: proxy.clone() };
        let call_fut = call_task.run();
        let task = fasync::Task::local(call_fut);

        let info = CallInfo { local_id, number, direction, state, proxy };
        Self { info, task }
    }
}

impl CallProxyTask {
    async fn run(mut self) -> LocalCallId {
        let result = self.run_inner().await;
        if let Err(err) = result {
            println!("Error running Peer task for call {}: {err:?}", self.local_id)
        }

        self.local_id
    }

    async fn run_inner(&mut self) -> Result<LocalCallId> {
        let mut call_state_stream =
            HangingGetStream::new(self.proxy.clone(), hfp::CallProxy::watch_state);
        loop {
            let new_call_state = call_state_stream.next().await;
            let new_call_state = new_call_state
                .ok_or_else(|| format_err!("Call stream closed."))?
                .map_err(|e| format_err!("FIDL error: {e}"))?;

            self.handle_new_call_state(new_call_state);
        }
    }

    fn handle_new_call_state(&mut self, new_state: hfp::CallState) {
        let mut calls = self.calls.lock();
        let call = calls.get_mut(&self.local_id);

        match call {
            None => {
                println!("BUG: got state {new_state:?} for nonexistent call {}", self.local_id)
            }
            Some(call) => {
                println!(
                    "Got state update for call {}: {:?} -> {:?})",
                    call.info.local_id, call.info.state, new_state
                );
                call.info.state = new_state;
            }
        }
    }
}
