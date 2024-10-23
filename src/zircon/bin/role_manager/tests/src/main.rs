// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::Result;
use fidl::endpoints::create_endpoints;
use fidl_fuchsia_scheduler::{
    Parameter, ParameterValue, RoleManagerMarker, RoleManagerSetRoleRequest,
    RoleManagerSetRoleResponse, RoleName, RoleTarget,
};
use fidl_test_rolemanager as ftest;
use fuchsia_component::client::connect_to_protocol;
use realm_proxy_client::RealmProxyClient;

async fn create_realm(options: ftest::RealmOptions) -> Result<RealmProxyClient> {
    let realm_factory = connect_to_protocol::<ftest::RealmFactoryMarker>()?;
    let (client, server) = create_endpoints();
    realm_factory
        .create_realm(options, server)
        .await?
        .map_err(realm_proxy_client::Error::OperationError)?;
    Ok(RealmProxyClient::from(client))
}

fn get_test_thread_handle() -> Result<zx::Thread> {
    Ok(fuchsia_runtime::thread_self().duplicate(zx::Rights::SAME_RIGHTS)?)
}

fn get_test_vmar_handle() -> Result<zx::Vmar> {
    Ok(fuchsia_runtime::vmar_root_self().duplicate(zx::Rights::SAME_RIGHTS)?)
}

#[fuchsia::test]
async fn test_set_role_thread() -> Result<()> {
    // Test that setting a basic role on a thread works.
    let realm_options = ftest::RealmOptions::default();
    let realm = create_realm(realm_options).await?;
    let role_manager = realm.connect_to_protocol::<RoleManagerMarker>().await?;

    let request = RoleManagerSetRoleRequest {
        target: Some(RoleTarget::Thread(get_test_thread_handle()?)),
        role: Some(RoleName { role: "test.core.a".to_string() }),
        ..Default::default()
    };
    let response = role_manager.set_role(request).await?;
    assert_eq!(
        response,
        Ok(RoleManagerSetRoleResponse {
            output_parameters: Some(vec![Parameter {
                key: "set_role".to_string(),
                value: ParameterValue::StringValue("test.core.a".to_string())
            }]),
            ..Default::default()
        })
    );
    Ok(())
}

#[fuchsia::test]
async fn test_set_role_vmar() -> Result<()> {
    // Test that setting a basic role on a vmar works.
    let realm_options = ftest::RealmOptions::default();
    let realm = create_realm(realm_options).await?;
    let role_manager = realm.connect_to_protocol::<RoleManagerMarker>().await?;

    let request = RoleManagerSetRoleRequest {
        target: Some(RoleTarget::Vmar(get_test_vmar_handle()?)),
        role: Some(RoleName { role: "test.core.mem.default".to_string() }),
        ..Default::default()
    };
    let response = role_manager.set_role(request).await?;
    assert_eq!(
        response,
        Ok(RoleManagerSetRoleResponse {
            output_parameters: Some(vec![Parameter {
                key: "set_role".to_string(),
                value: ParameterValue::StringValue("test.core.mem.default".to_string())
            }]),
            ..Default::default()
        })
    );
    Ok(())
}

#[fuchsia::test]
async fn test_input_parameters() -> Result<()> {
    // Test that passing in input parameters selects the correct role.
    let realm_options = ftest::RealmOptions::default();
    let realm = create_realm(realm_options).await?;
    let role_manager = realm.connect_to_protocol::<RoleManagerMarker>().await?;

    // First, verify that not passing input parameters to a parameterized role fails.
    let request = RoleManagerSetRoleRequest {
        target: Some(RoleTarget::Thread(get_test_thread_handle()?)),
        role: Some(RoleName { role: "test.core.parameterized.role".to_string() }),
        ..Default::default()
    };
    let response = role_manager.set_role(request).await?;
    assert_eq!(response, Err(zx::sys::ZX_ERR_NOT_FOUND));

    // Next, verify that passing in input parameters selects the correct role. We do so by passing
    // in the same role twice with different input parameters and verify that we get the correct
    // output parameters back.
    let request = RoleManagerSetRoleRequest {
        target: Some(RoleTarget::Thread(get_test_thread_handle()?)),
        role: Some(RoleName { role: "test.core.parameterized.role".to_string() }),
        input_parameters: Some(vec![Parameter {
            key: "input".to_string(),
            value: ParameterValue::StringValue("foo".to_string()),
        }]),
        ..Default::default()
    };
    let response = role_manager.set_role(request).await?;
    assert_eq!(
        response,
        Ok(RoleManagerSetRoleResponse {
            output_parameters: Some(vec![
                Parameter { key: "output1".to_string(), value: ParameterValue::IntValue(1) },
                Parameter { key: "output2".to_string(), value: ParameterValue::FloatValue(2.5) },
            ]),
            ..Default::default()
        })
    );
    let request = RoleManagerSetRoleRequest {
        target: Some(RoleTarget::Thread(get_test_thread_handle()?)),
        role: Some(RoleName { role: "test.core.parameterized.role".to_string() }),
        input_parameters: Some(vec![Parameter {
            key: "input".to_string(),
            value: ParameterValue::StringValue("bar".to_string()),
        }]),
        ..Default::default()
    };
    let response = role_manager.set_role(request).await?;
    assert_eq!(
        response,
        Ok(RoleManagerSetRoleResponse {
            output_parameters: Some(vec![
                Parameter { key: "output1".to_string(), value: ParameterValue::IntValue(5) },
                Parameter { key: "output2".to_string(), value: ParameterValue::FloatValue(42.6) },
            ]),
            ..Default::default()
        })
    );

    // Finally, verify that the order of input parameters does not change the outcome.
    let request = RoleManagerSetRoleRequest {
        target: Some(RoleTarget::Thread(get_test_thread_handle()?)),
        role: Some(RoleName { role: "test.core.parameterized.role".to_string() }),
        input_parameters: Some(vec![
            Parameter {
                key: "param1".to_string(),
                value: ParameterValue::StringValue("foo".to_string()),
            },
            Parameter {
                key: "param2".to_string(),
                value: ParameterValue::StringValue("bar".to_string()),
            },
            Parameter {
                key: "param3".to_string(),
                value: ParameterValue::StringValue("baz".to_string()),
            },
        ]),
        ..Default::default()
    };
    let response = role_manager.set_role(request).await?;
    assert_eq!(
        response,
        Ok(RoleManagerSetRoleResponse {
            output_parameters: Some(vec![
                Parameter { key: "output1".to_string(), value: ParameterValue::IntValue(489) },
                Parameter { key: "output2".to_string(), value: ParameterValue::FloatValue(297.5) },
                Parameter {
                    key: "output3".to_string(),
                    value: ParameterValue::StringValue("Hello, World!".to_string())
                },
            ]),
            ..Default::default()
        })
    );
    let request = RoleManagerSetRoleRequest {
        target: Some(RoleTarget::Thread(get_test_thread_handle()?)),
        role: Some(RoleName { role: "test.core.parameterized.role".to_string() }),
        input_parameters: Some(vec![
            Parameter {
                key: "param2".to_string(),
                value: ParameterValue::StringValue("bar".to_string()),
            },
            Parameter {
                key: "param3".to_string(),
                value: ParameterValue::StringValue("baz".to_string()),
            },
            Parameter {
                key: "param1".to_string(),
                value: ParameterValue::StringValue("foo".to_string()),
            },
        ]),
        ..Default::default()
    };
    let response = role_manager.set_role(request).await?;
    assert_eq!(
        response,
        Ok(RoleManagerSetRoleResponse {
            output_parameters: Some(vec![
                Parameter { key: "output1".to_string(), value: ParameterValue::IntValue(489) },
                Parameter { key: "output2".to_string(), value: ParameterValue::FloatValue(297.5) },
                Parameter {
                    key: "output3".to_string(),
                    value: ParameterValue::StringValue("Hello, World!".to_string())
                },
            ]),
            ..Default::default()
        })
    );

    Ok(())
}

#[fuchsia::test]
async fn test_scope_overrides() -> Result<()> {
    // Test that product role configurations override core role configurations.
    let realm_options = ftest::RealmOptions::default();
    let realm = create_realm(realm_options).await?;
    let role_manager = realm.connect_to_protocol::<RoleManagerMarker>().await?;

    let request = RoleManagerSetRoleRequest {
        target: Some(RoleTarget::Thread(get_test_thread_handle()?)),
        role: Some(RoleName { role: "test.core.product".to_string() }),
        ..Default::default()
    };
    let response = role_manager.set_role(request).await?;
    assert_eq!(
        response,
        Ok(RoleManagerSetRoleResponse {
            output_parameters: Some(vec![
                Parameter {
                    key: "set_role".to_string(),
                    value: ParameterValue::StringValue("test.core.product".to_string()),
                },
                Parameter {
                    key: "scope".to_string(),
                    value: ParameterValue::StringValue("product".to_string()),
                },
            ]),
            ..Default::default()
        })
    );

    Ok(())
}

#[fuchsia::test]
async fn test_nonexistent_role() -> Result<()> {
    let realm_options = ftest::RealmOptions::default();
    let realm = create_realm(realm_options).await?;
    let role_manager = realm.connect_to_protocol::<RoleManagerMarker>().await?;

    let request = RoleManagerSetRoleRequest {
        target: Some(RoleTarget::Thread(get_test_thread_handle()?)),
        role: Some(RoleName { role: "invalid_role".to_string() }),
        ..Default::default()
    };
    let response = role_manager.set_role(request).await?;
    assert_eq!(response, Err(zx::sys::ZX_ERR_NOT_FOUND));
    Ok(())
}

#[fuchsia::test]
async fn test_bad_config_extension() -> Result<()> {
    // Test that a profile from a file that does not have the extension `.profiles` was not
    // parsed and therefore does not exist.
    let realm_options = ftest::RealmOptions::default();
    let realm = create_realm(realm_options).await?;
    let role_manager = realm.connect_to_protocol::<RoleManagerMarker>().await?;

    let request = RoleManagerSetRoleRequest {
        target: Some(RoleTarget::Thread(get_test_thread_handle()?)),
        role: Some(RoleName { role: "bad.extension.role".to_string() }),
        ..Default::default()
    };
    let response = role_manager.set_role(request).await?;
    assert_eq!(response, Err(zx::sys::ZX_ERR_NOT_FOUND));
    Ok(())
}
