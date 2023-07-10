// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{format_err, Result};
use fidl_fuchsia_sys2 as fsys;
use moniker::RelativeMoniker;
use prettytable::{cell, format::consts::FORMAT_CLEAN, row, Row, Table};

const USE_TITLE: &'static str = "Used Capability";
const EXPOSE_TITLE: &'static str = "Exposed Capability";
const SUCCESS_SUMMARY: &'static str = "Success";
const CAPABILITY_COLUMN_WIDTH: usize = 50;
const SUMMARY_COLUMN_WIDTH: usize = 80;

#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

// Analytical information about a capability.
#[cfg_attr(feature = "serde", derive(Deserialize, Serialize))]
#[derive(Debug)]
pub struct RouteReport {
    pub decl_type: DeclType,
    pub capability: String,
    pub error_summary: Option<String>,
}

impl TryFrom<fsys::RouteReport> for RouteReport {
    type Error = anyhow::Error;

    fn try_from(report: fsys::RouteReport) -> Result<Self> {
        let decl_type = report.decl_type.ok_or(format_err!("missing decl type"))?.try_into()?;
        let capability = report.capability.ok_or(format_err!("missing capability name"))?;
        let error_summary = if let Some(error) = report.error { error.summary } else { None };
        Ok(RouteReport { decl_type, capability, error_summary })
    }
}

#[cfg_attr(feature = "serde", derive(Deserialize, Serialize))]
#[derive(Debug, PartialEq)]
pub enum DeclType {
    Use,
    Expose,
}

impl TryFrom<fsys::DeclType> for DeclType {
    type Error = anyhow::Error;

    fn try_from(value: fsys::DeclType) -> std::result::Result<Self, Self::Error> {
        match value {
            fsys::DeclType::Use => Ok(DeclType::Use),
            fsys::DeclType::Expose => Ok(DeclType::Expose),
            _ => Err(format_err!("unknown decl type")),
        }
    }
}

pub async fn validate_routes(
    route_validator: &fsys::RouteValidatorProxy,
    moniker: RelativeMoniker,
) -> Result<Vec<RouteReport>> {
    let reports = match route_validator.validate(&moniker.to_string()).await? {
        Ok(reports) => reports,
        Err(e) => {
            return Err(format_err!(
                "Component manager returned an unexpected error during validation: {:?}\n\
                 The state of the component instance may have changed.\n\
                 Please report this to the Component Framework team.",
                e
            ));
        }
    };

    reports.into_iter().map(|r| r.try_into()).collect()
}

fn format(report: &RouteReport) -> Row {
    let capability = textwrap::fill(&report.capability, CAPABILITY_COLUMN_WIDTH);
    let (mark, summary) = if let Some(summary) = &report.error_summary {
        let mark = ansi_term::Color::Red.paint("[✗]");
        let summary = textwrap::fill(summary, SUMMARY_COLUMN_WIDTH);
        (mark, summary)
    } else {
        let mark = ansi_term::Color::Green.paint("[✓]");
        let summary = textwrap::fill(SUCCESS_SUMMARY, SUMMARY_COLUMN_WIDTH);
        (mark, summary)
    };
    row!(mark, capability, summary)
}

// Construct the used and exposed capability tables from the given route reports.
pub fn create_tables(reports: Vec<RouteReport>) -> (Table, Table) {
    let mut use_table = new_table(USE_TITLE);
    let mut expose_table = new_table(EXPOSE_TITLE);

    for report in reports {
        match &report.decl_type {
            DeclType::Use => use_table.add_row(format(&report)),
            DeclType::Expose => expose_table.add_row(format(&report)),
        };
    }
    (use_table, expose_table)
}

// Create a new table with the given title.
fn new_table(title: &str) -> Table {
    let mut table = Table::new();
    table.set_format(*FORMAT_CLEAN);
    table.set_titles(row!("", title.to_string(), "Result"));
    table
}

#[cfg(test)]
mod test {
    use {
        super::*,
        fidl::endpoints::create_proxy_and_stream,
        futures::TryStreamExt,
        moniker::{AbsoluteMoniker, AbsoluteMonikerBase, RelativeMonikerBase},
    };

    fn route_validator(
        expected_moniker: &'static str,
        reports: Vec<fsys::RouteReport>,
    ) -> fsys::RouteValidatorProxy {
        let (route_validator, mut stream) =
            create_proxy_and_stream::<fsys::RouteValidatorMarker>().unwrap();
        fuchsia_async::Task::local(async move {
            match stream.try_next().await.unwrap().unwrap() {
                fsys::RouteValidatorRequest::Validate { moniker, responder, .. } => {
                    assert_eq!(
                        AbsoluteMoniker::parse_str(expected_moniker),
                        AbsoluteMoniker::parse_str(&moniker)
                    );
                    responder.send(Ok(&reports)).unwrap();
                }
                fsys::RouteValidatorRequest::Route { .. } => {
                    panic!("unexpected Route request");
                }
            }
        })
        .detach();
        route_validator
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_errors() {
        let validator = route_validator(
            "/test",
            vec![fsys::RouteReport {
                capability: Some("fuchsia.foo.bar".to_string()),
                decl_type: Some(fsys::DeclType::Use),
                error: Some(fsys::RouteError {
                    summary: Some("Access denied".to_string()),
                    ..Default::default()
                }),
                ..Default::default()
            }],
        );

        let mut reports = validate_routes(&validator, RelativeMoniker::parse_str("/test").unwrap())
            .await
            .unwrap();
        assert_eq!(reports.len(), 1);

        let report = reports.remove(0);
        assert_eq!(report.capability, "fuchsia.foo.bar");
        assert_eq!(report.decl_type, DeclType::Use);

        let error = report.error_summary.unwrap();
        assert_eq!(error, "Access denied");
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_no_errors() {
        let validator = route_validator(
            "/test",
            vec![fsys::RouteReport {
                capability: Some("fuchsia.foo.bar".to_string()),
                decl_type: Some(fsys::DeclType::Use),
                error: None,
                ..Default::default()
            }],
        );

        let mut reports = validate_routes(&validator, RelativeMoniker::parse_str("/test").unwrap())
            .await
            .unwrap();
        assert_eq!(reports.len(), 1);

        let report = reports.remove(0);
        assert_eq!(report.capability, "fuchsia.foo.bar");
        assert_eq!(report.decl_type, DeclType::Use);
        assert!(report.error_summary.is_none());
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_no_routes() {
        let validator = route_validator("/test", vec![]);

        let reports = validate_routes(&validator, RelativeMoniker::parse_str("/test").unwrap())
            .await
            .unwrap();
        assert!(reports.is_empty());
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_parse_error() {
        let validator = route_validator(
            "/test",
            vec![
                // Don't set any fields
                fsys::RouteReport::default(),
            ],
        );

        let result =
            validate_routes(&validator, RelativeMoniker::parse_str("/test").unwrap()).await;
        assert!(result.is_err());
    }
}
