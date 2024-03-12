// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    fidl_fuchsia_element as element,
    fuchsia_component::client::connect_to_protocol,
    tracing::{info, warn},
};

#[fuchsia::main(logging = true)]
async fn main() {
    info!("element_launcher is starting.");
    let config = element_launcher_config::Config::take_from_startup_handle();
    if config.main_element_url.is_empty() {
        warn!("element_launcher was started without a main_element_url; quitting.");
        return;
    }

    let element_manager = connect_to_protocol::<element::ManagerMarker>()
        .expect("failed to connect to fuchsia.element.Manager");
    element_manager
        .propose_element(
            element::Spec {
                component_url: Some(config.main_element_url),
                annotations: Some(vec![element::Annotation {
                    key: element::AnnotationKey {
                        namespace: "element_manager".to_string(),
                        value: "name".to_string(),
                    },
                    value: element::AnnotationValue::Text("main".to_string()),
                }]),
                ..Default::default()
            },
            None,
        )
        .await
        .expect("Failed to call ProposeElement.")
        .expect("Failed to propose element.");
}

#[cfg(test)]
mod tests {
    // TODO(https://fxbug.dev/329232994): Add unit tests
}
