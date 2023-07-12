// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::builtin::capability::BuiltinCapability,
    ::routing::capability_source::InternalCapability, anyhow::Error, async_trait::async_trait,
    cm_types::Name, elf_runner::crash_info::CrashRecords, fidl_fuchsia_component as fcomponent,
    fidl_fuchsia_sys2 as fsys, fuchsia_zircon as zx, futures::TryStreamExt,
    lazy_static::lazy_static, std::sync::Arc,
};

lazy_static! {
    static ref CRASH_RECORDS_CAPABILITY_NAME: Name =
        "fuchsia.sys2.CrashIntrospect".parse().unwrap();
}

pub struct CrashIntrospectSvc(CrashRecords);

impl CrashIntrospectSvc {
    pub fn new(crash_records: CrashRecords) -> Arc<Self> {
        Arc::new(Self(crash_records))
    }
}

#[async_trait]
impl BuiltinCapability for CrashIntrospectSvc {
    const NAME: &'static str = "CrashIntrospect";
    type Marker = fsys::CrashIntrospectMarker;

    async fn serve(
        self: Arc<Self>,
        mut stream: fsys::CrashIntrospectRequestStream,
    ) -> Result<(), Error> {
        while let Some(request) = stream.try_next().await? {
            match request {
                fsys::CrashIntrospectRequest::FindComponentByThreadKoid {
                    thread_koid,
                    responder,
                } => match self.0.take_report(&zx::Koid::from_raw(thread_koid)).await {
                    Some(report) => responder.send(Ok(&report.into()))?,
                    None => responder.send(Err(fcomponent::Error::ResourceNotFound))?,
                },
            }
        }
        Ok(())
    }

    fn matches_routed_capability(&self, capability: &InternalCapability) -> bool {
        capability.matches_protocol(&CRASH_RECORDS_CAPABILITY_NAME)
    }
}

#[cfg(test)]
mod tests {
    use {
        super::*, elf_runner::crash_info::ComponentCrashInfo,
        fidl::endpoints::create_proxy_and_stream, fuchsia_async as fasync, moniker::Moniker,
    };

    #[fuchsia::test]
    async fn get_crash_report() -> Result<(), Error> {
        let crash_records = CrashRecords::new();
        let crash_records_svc = CrashIntrospectSvc::new(crash_records.clone());

        let (crash_records_proxy, crash_records_stream) =
            create_proxy_and_stream::<fsys::CrashIntrospectMarker>()?;
        let _task = fasync::Task::local(crash_records_svc.serve(crash_records_stream));

        let koid_raw = 123;
        let koid = zx::Koid::from_raw(koid_raw);
        let url = "456".to_string();
        let moniker = Moniker::try_from(vec!["a"]).unwrap();
        let crash_report = ComponentCrashInfo { url: url.clone(), moniker: moniker.clone() };

        assert_eq!(
            Err(fcomponent::Error::ResourceNotFound),
            crash_records_proxy.find_component_by_thread_koid(koid_raw).await?
        );

        assert_eq!(None, crash_records.take_report(&koid).await);
        crash_records.add_report(koid, crash_report).await;

        assert_eq!(
            Ok(fsys::ComponentCrashInfo {
                url: Some(url.clone()),
                moniker: Some(moniker.to_string()),
                ..Default::default()
            }),
            crash_records_proxy.find_component_by_thread_koid(koid_raw).await?,
        );

        assert_eq!(
            Err(fcomponent::Error::ResourceNotFound),
            crash_records_proxy.find_component_by_thread_koid(koid_raw).await?
        );
        Ok(())
    }
}
