// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fuchsia_inspect::Node as InspectNode;
use fuchsia_inspect_contrib::inspect_log;
use fuchsia_inspect_contrib::nodes::BoundedListNode;

pub const INSPECT_TOGGLE_EVENTS_LIMIT: usize = 20;

#[derive(Debug)]
pub enum ClientConnectionsToggleEvent {
    Enabled,
    Disabled,
}

pub struct ToggleLogger {
    toggle_inspect_node: BoundedListNode,
}

impl ToggleLogger {
    pub fn new(inspect_node: &InspectNode) -> Self {
        // Initialize inspect children
        let toggle_events = inspect_node.create_child("client-connections-toggle-events");
        let toggle_inspect_node = BoundedListNode::new(toggle_events, INSPECT_TOGGLE_EVENTS_LIMIT);

        Self { toggle_inspect_node }
    }

    pub fn log_toggle_event(&mut self, event_type: ClientConnectionsToggleEvent) {
        // This inspect macro logs the time as well
        inspect_log!(self.toggle_inspect_node, {
            event_type: std::format!("{:?}", event_type)
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use diagnostics_assertions::{assert_data_tree, AnyNumericProperty};

    #[fuchsia::test]
    fn test_toggle_is_recorded_to_inspect() {
        let inspector = fuchsia_inspect::Inspector::default();
        let node = inspector.root().create_child("wlan_mock_node");

        // The clone is needed here so that there is still a reference to the inspect node.
        let mut toggle_logger = ToggleLogger::new(&node);

        toggle_logger.log_toggle_event(ClientConnectionsToggleEvent::Enabled);
        toggle_logger.log_toggle_event(ClientConnectionsToggleEvent::Disabled);
        toggle_logger.log_toggle_event(ClientConnectionsToggleEvent::Enabled);

        assert_data_tree!(inspector, root: {
            wlan_mock_node: {
                "client-connections-toggle-events": {
                    "0": {
                        "event_type": "Enabled",
                        "@time": AnyNumericProperty
                    },
                    "1": {
                        "event_type": "Disabled",
                        "@time": AnyNumericProperty
                    },
                    "2": {
                        "event_type": "Enabled",
                        "@time": AnyNumericProperty
                    },
                }
            }
        });
    }
}
