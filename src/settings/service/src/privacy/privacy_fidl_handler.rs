// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::base::{SettingInfo, SettingType};
use crate::handler::base::Request;
use crate::ingress::{request, watch, Scoped};
use crate::job::source::{Error as JobError, ErrorResponder};
use crate::job::Job;
use fidl::prelude::*;
use fidl_fuchsia_settings::{
    PrivacyRequest, PrivacySetResponder, PrivacySetResult, PrivacySettings, PrivacyWatchResponder,
};

impl ErrorResponder for PrivacySetResponder {
    fn id(&self) -> &'static str {
        "Privacy_Set"
    }

    fn respond(self: Box<Self>, error: fidl_fuchsia_settings::Error) -> Result<(), fidl::Error> {
        self.send(Err(error))
    }
}

impl request::Responder<Scoped<PrivacySetResult>> for PrivacySetResponder {
    fn respond(self, Scoped(response): Scoped<PrivacySetResult>) {
        let _ = self.send(response);
    }
}

impl watch::Responder<PrivacySettings, zx::Status> for PrivacyWatchResponder {
    fn respond(self, response: Result<PrivacySettings, zx::Status>) {
        match response {
            Ok(settings) => {
                let _ = self.send(&settings);
            }
            Err(error) => {
                self.control_handle().shutdown_with_epitaph(error);
            }
        }
    }
}

impl TryFrom<PrivacyRequest> for Job {
    type Error = JobError;

    fn try_from(item: PrivacyRequest) -> Result<Self, Self::Error> {
        #[allow(unreachable_patterns)]
        match item {
            PrivacyRequest::Set { settings, responder } => {
                Ok(request::Work::new(SettingType::Privacy, to_request(settings), responder).into())
            }
            PrivacyRequest::Watch { responder } => {
                Ok(watch::Work::new_job(SettingType::Privacy, responder))
            }
            _ => {
                log::warn!("Received a call to an unsupported API: {:?}", item);
                Err(JobError::Unsupported)
            }
        }
    }
}

impl From<SettingInfo> for PrivacySettings {
    fn from(response: SettingInfo) -> Self {
        if let SettingInfo::Privacy(info) = response {
            return PrivacySettings {
                user_data_sharing_consent: info.user_data_sharing_consent,
                ..Default::default()
            };
        }

        panic!("incorrect value sent to privacy");
    }
}

fn to_request(settings: PrivacySettings) -> Request {
    Request::SetUserDataSharingConsent(settings.user_data_sharing_consent)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::job::{execution, work};
    use assert_matches::assert_matches;
    use fidl_fuchsia_settings::{PrivacyMarker, PrivacyRequestStream};
    use futures::StreamExt;

    #[fuchsia::test]
    fn test_request_from_settings_empty() {
        let request = to_request(PrivacySettings::default());

        assert_eq!(request, Request::SetUserDataSharingConsent(None));
    }

    #[fuchsia::test]
    fn test_request_from_settings() {
        const USER_DATA_SHARING_CONSENT: bool = true;

        let request = to_request(PrivacySettings {
            user_data_sharing_consent: Some(USER_DATA_SHARING_CONSENT),
            ..Default::default()
        });

        assert_eq!(request, Request::SetUserDataSharingConsent(Some(USER_DATA_SHARING_CONSENT)));
    }

    #[fuchsia::test(allow_stalls = false)]
    async fn try_from_set_converts_supplied_params() {
        let (proxy, server) = fidl::endpoints::create_proxy::<PrivacyMarker>();
        let _fut = proxy
            .set(&PrivacySettings { user_data_sharing_consent: Some(true), ..Default::default() });
        let mut request_stream: PrivacyRequestStream = server.into_stream();
        let request = request_stream
            .next()
            .await
            .expect("should have on request before stream is closed")
            .expect("should have gotten a request");
        let job = Job::try_from(request);
        let job = job.as_ref();
        assert_matches!(job.map(|j| j.workload()), Ok(work::Load::Independent(_)));
        assert_matches!(job.map(|j| j.execution_type()), Ok(execution::Type::Independent));
    }

    #[fuchsia::test(allow_stalls = false)]
    async fn try_from_watch_converts_supplied_params() {
        let (proxy, server) = fidl::endpoints::create_proxy::<PrivacyMarker>();
        let _fut = proxy.watch();
        let mut request_stream: PrivacyRequestStream = server.into_stream();
        let request = request_stream
            .next()
            .await
            .expect("should have on request before stream is closed")
            .expect("should have gotten a request");
        let job = Job::try_from(request);
        let job = job.as_ref();
        assert_matches!(job.map(|j| j.workload()), Ok(work::Load::Sequential(_, _)));
        assert_matches!(job.map(|j| j.execution_type()), Ok(execution::Type::Sequential(_)));
    }
}
