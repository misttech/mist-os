// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{events::error::EventError, identity::ComponentIdentity};
use fidl::endpoints::{ClientEnd, ServerEnd};
use fidl::prelude::*;
use fidl_fuchsia_component as fcomponent;
use fidl_fuchsia_inspect as finspect;
use fidl_fuchsia_io as fio;
use fidl_fuchsia_logger as flogger;
use fidl_table_validation::ValidFidlTable;
use fuchsia_zircon as zx;
use moniker::ExtendedMoniker;
use std::{convert::TryFrom, sync::Arc};

/// Event types that contain singleton data. When these events are cloned, their singleton data
/// won't be cloned.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum EventType {
    DiagnosticsReady,
    LogSinkRequested,
    InspectSinkRequested,
}

impl AsRef<str> for EventType {
    fn as_ref(&self) -> &str {
        match &self {
            Self::DiagnosticsReady => "diagnostics_ready",
            Self::LogSinkRequested => "log_sink_requested",
            Self::InspectSinkRequested => "inspect_sink_requested",
        }
    }
}

/// An event that is emitted and consumed.
#[derive(Debug)]
pub struct Event {
    /// The contents of the event.
    pub payload: EventPayload,
    pub timestamp: zx::Time,
}

impl Event {
    pub fn ty(&self) -> EventType {
        match &self.payload {
            EventPayload::DiagnosticsReady(_) => EventType::DiagnosticsReady,
            EventPayload::LogSinkRequested(_) => EventType::LogSinkRequested,
            EventPayload::InspectSinkRequested(_) => EventType::InspectSinkRequested,
        }
    }
}

/// The contents of the event depending on the type of event.
#[derive(Debug)]
pub enum EventPayload {
    DiagnosticsReady(DiagnosticsReadyPayload),
    LogSinkRequested(LogSinkRequestedPayload),
    InspectSinkRequested(InspectSinkRequestedPayload),
}

pub struct InspectSinkRequestedPayload {
    /// The component that is connecting to `InspectSink`.
    pub component: Arc<ComponentIdentity>,
    /// The stream containing requests made on the `InspectSink` channel by the component.
    pub request_stream: finspect::InspectSinkRequestStream,
}

impl std::fmt::Debug for InspectSinkRequestedPayload {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InspectSinkRequestedPayload").field("component", &self.component).finish()
    }
}

/// Payload for a CapabilityReady(diagnostics) event.
#[derive(Debug)]
pub struct DiagnosticsReadyPayload {
    /// The component which diagnostics directory is available.
    pub component: Arc<ComponentIdentity>,
    /// The `out/diagnostics` directory of the component.
    pub directory: fio::DirectoryProxy,
}

/// Payload for a connection to the `LogSink` protocol.
pub struct LogSinkRequestedPayload {
    /// The component that is connecting to `LogSink`.
    pub component: Arc<ComponentIdentity>,
    /// The stream containing requests made on the `LogSink` channel by the component.
    pub request_stream: flogger::LogSinkRequestStream,
}

impl std::fmt::Debug for LogSinkRequestedPayload {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LogSinkRequestedPayload").field("component", &self.component).finish()
    }
}

#[derive(Debug, ValidFidlTable)]
#[fidl_table_src(fcomponent::EventHeader)]
pub struct ValidatedEventHeader {
    pub event_type: fcomponent::EventType,
    pub component_url: String,
    pub moniker: String,
    pub timestamp: i64,
}

#[derive(Debug, ValidFidlTable)]
#[fidl_table_src(fcomponent::Event)]
pub struct ValidatedEvent {
    /// Information about the component for which this event was generated.
    pub header: ValidatedEventHeader,

    pub payload: fcomponent::EventPayload,
}

impl TryFrom<fcomponent::Event> for Event {
    type Error = EventError;

    fn try_from(event: fcomponent::Event) -> Result<Event, Self::Error> {
        if let Ok(event) = ValidatedEvent::try_from(event) {
            let identity = ComponentIdentity::new(
                ExtendedMoniker::parse_str(&event.header.moniker)?,
                &event.header.component_url,
            );

            match event.header.event_type {
                fcomponent::EventType::DirectoryReady => {
                    let directory = match event.payload {
                        fcomponent::EventPayload::DirectoryReady(directory_ready) => {
                            let name =
                                directory_ready.name.ok_or(EventError::MissingField("name"))?;
                            if name != "diagnostics" {
                                return Err(EventError::IncorrectName {
                                    received: name,
                                    expected: "diagnostics",
                                });
                            }
                            match directory_ready.node {
                                Some(node) => {
                                    let directory =
                                        ClientEnd::<fio::DirectoryMarker>::new(node.into_channel());
                                    directory.into_proxy()?
                                }
                                None => return Err(EventError::MissingDiagnosticsDir),
                            }
                        }
                        _ => return Err(EventError::UnknownResult(event.payload)),
                    };
                    Ok(Event {
                        timestamp: zx::Time::from_nanos(event.header.timestamp),
                        payload: EventPayload::DiagnosticsReady(DiagnosticsReadyPayload {
                            component: Arc::new(identity),
                            directory,
                        }),
                    })
                }
                fcomponent::EventType::CapabilityRequested => {
                    let request_stream = match event.payload {
                        fcomponent::EventPayload::CapabilityRequested(capability_requested) => {
                            let name = capability_requested
                                .name
                                .ok_or(EventError::MissingField("name"))?;

                            if name != flogger::LogSinkMarker::PROTOCOL_NAME {
                                return Err(EventError::IncorrectName {
                                    received: name,
                                    expected: flogger::LogSinkMarker::PROTOCOL_NAME,
                                });
                            }
                            let capability = capability_requested
                                .capability
                                .ok_or(EventError::MissingField("capability"))?;
                            ServerEnd::<flogger::LogSinkMarker>::new(capability).into_stream()?
                        }
                        _ => return Err(EventError::UnknownResult(event.payload)),
                    };
                    Ok(Event {
                        timestamp: zx::Time::from_nanos(event.header.timestamp),
                        payload: EventPayload::LogSinkRequested(LogSinkRequestedPayload {
                            component: Arc::new(identity),
                            request_stream,
                        }),
                    })
                }
                _ => Err(EventError::InvalidEventType(event.header.event_type)),
            }
        } else {
            Err(EventError::MissingField("Payload or header is missing"))
        }
    }
}
