/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

use bytes::Bytes;
use headers::{ContentType, HeaderMapExt};
use http::HeaderMap;
use hyper_serde::Serde;
use malloc_size_of_derive::MallocSizeOf;
use net_traits::request::{
    CredentialsMode, Destination, RequestBody, RequestId, RequestMode,
    create_request_body_with_content,
};
use net_traits::{FetchMetadata, NetworkError, ResourceFetchTiming};
use script_bindings::str::DOMString;
use serde::Serialize;
use servo_url::{ImmutableOrigin, ServoUrl};

use crate::dom::csp::Violation;
use crate::dom::globalscope::GlobalScope;
use crate::dom::performance::performanceresourcetiming::InitiatorType;
use crate::dom::reporting::report::{Report, ReportBody};
use crate::fetch::fetch::{RequestWithGlobalScope, create_a_potential_cors_request_with_claim};
use crate::fetch::network_listener::{
    FetchResponseListener, ResourceTimingListener, submit_timing,
};
use crate::url::ensure_blob_referenced_by_url_is_kept_alive;

/// <https://w3c.github.io/reporting/#endpoint>
#[derive(Clone, Eq, Hash, MallocSizeOf, PartialEq)]
pub(crate) struct ReportingEndpoint {
    /// <https://w3c.github.io/reporting/#dom-endpoint-name>
    name: DOMString,
    /// <https://w3c.github.io/reporting/#dom-endpoint-url>
    url: ServoUrl,
    /// <https://w3c.github.io/reporting/#dom-endpoint-failures>
    failures: u32,
}

impl ReportingEndpoint {
    /// <https://w3c.github.io/reporting/#process-header>
    pub(crate) fn parse_reporting_endpoints_header(
        response_url: &ServoUrl,
        headers: &Option<Serde<HeaderMap>>,
    ) -> Option<Vec<ReportingEndpoint>> {
        let headers = headers.as_ref()?;
        Some(
            net_traits::reporting::parse_reporting_endpoints(response_url, headers)?
                .into_iter()
                .map(|endpoint| ReportingEndpoint {
                    name: endpoint.name.into(),
                    url: endpoint.url,
                    failures: 0,
                })
                .collect(),
        )
    }
}

pub(crate) trait SendReportsToEndpoints {
    /// <https://w3c.github.io/reporting/#send-reports>
    fn send_reports_to_endpoints(&self, reports: Vec<Report>, endpoints: Vec<ReportingEndpoint>);
    /// <https://w3c.github.io/reporting/#try-delivery>
    fn attempt_to_deliver_reports_to_endpoints(
        &self,
        endpoint: &ServoUrl,
        origin: ImmutableOrigin,
        reports: &[&Report],
    );
    /// <https://w3c.github.io/reporting/#serialize-a-list-of-reports-to-json>
    fn serialize_list_of_reports(reports: &[&Report]) -> Option<RequestBody>;
}

impl SendReportsToEndpoints for GlobalScope {
    fn send_reports_to_endpoints(
        &self,
        mut reports: Vec<Report>,
        endpoints: Vec<ReportingEndpoint>,
    ) {
        // Step 1. Let endpoint map be an empty map of endpoint objects to lists of report objects.
        let mut endpoint_map: HashMap<&ReportingEndpoint, Vec<Report>> = HashMap::new();
        // Step 2. For each report in reports:
        reports.retain_mut(|report| {
            // Step 2.1. If there exists an endpoint (endpoint) in context’s endpoints
            // list whose name is report’s destination:
            if let Some(endpoint) = endpoints
                .iter()
                .find(|e| e.name.str() == report.destination.as_str())
            {
                // Step 2.1.1. Append report to endpoint map’s list of reports for endpoint.
                report.attempts = report.attempts.saturating_add(1);
                endpoint_map
                    .entry(endpoint)
                    .or_default()
                    .push(report.clone());
                true
            } else {
                // Step 2.1.2. Otherwise, remove report from reports.
                false
            }
        });
        // Step 3. For each (endpoint, report list) pair in endpoint map:
        for (endpoint, report_list) in endpoint_map.iter() {
            // Step 3.1. Let origin map be an empty map of origins to lists of report objects.
            let mut origin_map: HashMap<ImmutableOrigin, Vec<&Report>> = HashMap::new();
            // Step 3.2. For each report in report list:
            for report in report_list {
                let Ok(url) = ServoUrl::parse(&report.url) else {
                    continue;
                };
                // Step 3.2.1. Let origin be the origin of report’s url.
                let origin = url.origin();
                // Step 3.2.2. Append report to origin map’s list of reports for origin.
                origin_map.entry(origin).or_default().push(report);
            }
            // Step 3.3. For each (origin, per-origin reports) pair in origin map,
            // execute the following steps asynchronously:
            for (origin, origin_report_list) in origin_map.iter() {
                // Step 3.3.1. Let result be the result of executing
                // § 3.5.2 Attempt to deliver reports to endpoint on endpoint, origin, and per-origin reports.
                self.attempt_to_deliver_reports_to_endpoints(
                    &endpoint.url,
                    origin.clone(),
                    origin_report_list,
                );
                // Step 3.3.2. If result is "Failure":
                // TODO(37238)
                // Step 3.3.2.1. Increment endpoint’s failures.
                // TODO(37238)
                // Step 3.3.3. If result is "Remove Endpoint":
                // TODO(37238)
                // Step 3.3.3.1 Remove endpoint from context’s endpoints list.
                // TODO(37238)
                // Step 3.3.4. Remove each report from reports.
                // TODO(37238)
            }
        }
    }

    fn attempt_to_deliver_reports_to_endpoints(
        &self,
        endpoint: &ServoUrl,
        origin: ImmutableOrigin,
        reports: &[&Report],
    ) {
        // Step 1. Let body be the result of executing serialize a list of reports to JSON on reports.
        let request_body = Self::serialize_list_of_reports(reports);
        // Step 2. Let request be a new request with the following properties [FETCH]:
        let mut headers = HeaderMap::with_capacity(1);
        headers.typed_insert(ContentType::from(
            "application/reports+json".parse::<mime::Mime>().unwrap(),
        ));
        let url = ensure_blob_referenced_by_url_is_kept_alive(self, endpoint.clone());
        let request = create_a_potential_cors_request_with_claim(
            None,
            url,
            Destination::Report,
            None,
            None,
            self.get_referrer(),
        )
        .with_global_scope(self)
        .method(http::Method::POST)
        .body(request_body)
        .origin(origin)
        .mode(RequestMode::CorsMode)
        .credentials_mode(CredentialsMode::CredentialsSameOrigin)
        .unsafe_request(true)
        .headers(headers);
        // Step 3. Queue a task to fetch request.
        self.fetch(
            request,
            CSPReportEndpointFetchListener {
                endpoint: endpoint.clone(),
            },
            self.task_manager().networking_task_source().into(),
        );
        // Step 4. Wait for a response (response).
        // TODO(37238)
        // Step 5. If response’s status is an OK status (200-299), return "Success".
        // TODO(37238)
        // Step 6. If response’s status is 410 Gone [RFC9110], return "Remove Endpoint".
        // TODO(37238)
        // Step 7. Return "Failure".
        // TODO(37238)
    }

    fn serialize_list_of_reports(reports: &[&Report]) -> Option<RequestBody> {
        // Step 1. Let collection be an empty list.
        // Step 2. For each report in reports:
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
            .min(u64::MAX as u128) as u64;
        let report_body: Vec<SerializedReport> = reports
            .iter()
            // Step 2.1. Let data be a map with the following key/value pairs:
            .map(|r| SerializedReport {
                age: now.saturating_sub(r.timestamp),
                type_: r.type_.to_string(),
                url: r.url.to_string(),
                user_agent: String::new(),
                body: r.body.clone(),
            })
            // Attempts are counted when each pending report is selected for delivery.
            // Step 2.3. Append data to collection.
            .collect();
        // Step 3. Return the byte sequence resulting from executing serialize an
        // Infra value to JSON bytes on collection.
        Some(create_request_body_with_content(
            serde_json::to_string(&report_body).unwrap_or_default(),
        ))
    }
}

#[derive(Serialize)]
struct SerializedReport {
    age: u64,
    #[serde(rename = "type")]
    type_: String,
    url: String,
    user_agent: String,
    body: ReportBody,
}

struct CSPReportEndpointFetchListener {
    /// Endpoint URL of this request.
    endpoint: ServoUrl,
}

impl FetchResponseListener for CSPReportEndpointFetchListener {
    fn process_request_body(&mut self, _: &GlobalScope, _: RequestId) {}

    fn process_response(
        &mut self,
        _: &mut js::context::JSContext,
        _: &GlobalScope,
        _: RequestId,
        fetch_metadata: Result<FetchMetadata, NetworkError>,
    ) {
        _ = fetch_metadata;
    }

    fn process_response_chunk(
        &mut self,
        _: &mut js::context::JSContext,
        _: &GlobalScope,
        _: RequestId,
        chunk: Bytes,
    ) {
        _ = chunk;
    }

    fn process_response_eof(
        self,
        cx: &mut js::context::JSContext,
        global: &GlobalScope,
        _: RequestId,
        response: Result<(), NetworkError>,
        timing: ResourceFetchTiming,
    ) {
        submit_timing(cx, global, &self, &response, &timing);
    }

    fn process_csp_violations(
        &mut self,
        _cx: &mut js::context::JSContext,
        _: &GlobalScope,
        _request_id: RequestId,
        _violations: Vec<Violation>,
    ) {
    }
}

impl ResourceTimingListener for CSPReportEndpointFetchListener {
    fn resource_timing_information(&self) -> (InitiatorType, ServoUrl) {
        (InitiatorType::Other, self.endpoint.clone())
    }
}
