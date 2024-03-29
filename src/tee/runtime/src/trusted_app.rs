// Copyright 2024 The Fuchsia Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#![allow(dead_code)]
use crate::ta_loader::TAInterface;
use anyhow::Error;
use tee_internal_impl::binding::TEE_SUCCESS;

// This structure stores the entry points to the TA and application-specific
// state like sessions.
pub struct TrustedApp<T: TAInterface> {
    interface: T,
}

impl<T: TAInterface> TrustedApp<T> {
    pub fn new(interface: T) -> Result<Self, Error> {
        let result = interface.create();
        if result != TEE_SUCCESS {
            anyhow::bail!("Create callback failed: {result:?}");
        }
        Ok(Self { interface })
    }

    pub fn open_session(&mut self) {
        let _ = self;
        todo!();
    }

    pub fn close_session(&mut self) {
        let _ = self;
        todo!();
    }

    pub fn invoke_command(&self) {
        let _ = self;
        todo!();
    }

    pub fn destroy(&self) {
        self.interface.destroy()
    }
}
