// Copyright 2024 The Fuchsia Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#![allow(dead_code)]

use anyhow::Error;

use tee_internal_impl::binding::{TEE_Param, TEE_PropSetHandle, TEE_Result};

pub type SessionContext = *mut ::std::os::raw::c_void;

pub trait TAInterface {
    fn create(&self) -> TEE_Result;
    fn destroy(&self);
    fn open_session(
        &self,
        param_types: u32,
        params: *mut TEE_Param,
        session_context: *mut SessionContext,
    ) -> TEE_Result;
    fn close_session(
        &self,
        session_context: SessionContext,
        command_id: u32,
        param_types: u32,
        params: *mut TEE_Param,
    ) -> TEE_Result;
    fn invoke_command(
        &self,
        propset_or_enumerator: TEE_PropSetHandle,
        name: *mut ::std::os::raw::c_char,
        value_buffer: *mut ::std::os::raw::c_char,
        value_buffer_len: *mut usize,
    ) -> TEE_Result;
}

struct TAFunctions {
    create_fn: fn() -> TEE_Result,
    destroy_fn: fn(),
    open_session_fn: fn(
        param_types: u32,
        params: *mut TEE_Param,
        session_context: *mut SessionContext,
    ) -> TEE_Result,
    close_session_fn: fn(
        session_context: SessionContext,
        command_id: u32,
        param_types: u32,
        params: *mut TEE_Param,
    ) -> TEE_Result,
    invoke_command_fn: fn(
        propset_or_enumerator: TEE_PropSetHandle,
        name: *mut ::std::os::raw::c_char,
        value_buffer: *mut ::std::os::raw::c_char,
        value_buffer_len: *mut usize,
    ) -> TEE_Result,
}

impl TAInterface for TAFunctions {
    fn create(&self) -> TEE_Result {
        (self.create_fn)()
    }

    fn destroy(&self) {
        (self.destroy_fn)()
    }

    fn open_session(
        &self,
        param_types: u32,
        params: *mut TEE_Param,
        session_context: *mut SessionContext,
    ) -> TEE_Result {
        (self.open_session_fn)(param_types, params, session_context)
    }

    fn close_session(
        &self,
        session_context: SessionContext,
        command_id: u32,
        param_types: u32,
        params: *mut TEE_Param,
    ) -> TEE_Result {
        (self.close_session_fn)(session_context, command_id, param_types, params)
    }

    fn invoke_command(
        &self,
        propset_or_enumerator: TEE_PropSetHandle,
        name: *mut ::std::os::raw::c_char,
        value_buffer: *mut ::std::os::raw::c_char,
        value_buffer_len: *mut usize,
    ) -> TEE_Result {
        (self.invoke_command_fn)(propset_or_enumerator, name, value_buffer, value_buffer_len)
    }
}

fn load_sym(handle: *mut libc::c_void, name: &std::ffi::CStr) -> Result<*const (), Error> {
    let fun = unsafe { libc::dlsym(handle, name.as_ptr()) };
    if fun.is_null() {
        anyhow::bail!("Could not find symbol {name:?}: {:?}", std::io::Error::last_os_error());
    }
    Ok(fun as *const ())
}

pub fn load_ta(name: &std::ffi::CStr) -> Result<impl TAInterface, Error> {
    let handle = unsafe { libc::dlopen(name.as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL) };
    if handle.is_null() {
        anyhow::bail!("Could not load {name:?}: {:?}", std::io::Error::last_os_error());
    }
    Ok(TAFunctions {
        create_fn: unsafe { std::mem::transmute(load_sym(handle, c"TA_CreateEntryPoint")?) },
        destroy_fn: unsafe { std::mem::transmute(load_sym(handle, c"TA_DestroyEntryPoint")?) },
        open_session_fn: unsafe {
            std::mem::transmute(load_sym(handle, c"TA_OpenSessionEntryPoint")?)
        },
        close_session_fn: unsafe {
            std::mem::transmute(load_sym(handle, c"TA_CloseSessionEntryPoint")?)
        },
        invoke_command_fn: unsafe {
            std::mem::transmute(load_sym(handle, c"TA_InvokeCommandEntryPoint")?)
        },
    })
}
