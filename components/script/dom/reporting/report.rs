/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Internal report data and fresh application-exposed dictionary snapshots.
//!
//! No JavaScript value is retained in a report queue. Page code may mutate a
//! delivered dictionary without modifying buffered reports or endpoint payloads.

use js::context::JSContext;
use js::conversions::ToJSValConvertible;
use js::jsapi::Heap;
use js::jsval::NullValue;
use js::rust::MutableHandleValue;
use malloc_size_of_derive::MallocSizeOf;
use net_traits::permissions_policy::ViolationDisposition;
use script_bindings::trace::RootedTraceableBox;
use serde::Serialize;

use crate::dom::bindings::codegen::Bindings::CSPViolationReportBodyBinding::CSPViolationReportBody;
use crate::dom::bindings::codegen::Bindings::PermissionsPolicyViolationReportBodyBinding::PermissionsPolicyViolationReportBody;
use crate::dom::bindings::codegen::Bindings::ReportingObserverBinding::{
    Report as ExposedReport, ReportBody as EmptyReportBody,
};
use crate::dom::bindings::codegen::Bindings::SecurityPolicyViolationEventBinding::SecurityPolicyViolationEventDisposition;
use crate::dom::csppolicyviolationreport::serialize_disposition;

#[derive(Clone, Debug, MallocSizeOf)]
pub(crate) struct Report {
    pub(crate) type_: String,
    pub(crate) url: String,
    pub(crate) body: ReportBody,
    pub(crate) destination: String,
    pub(crate) timestamp: u64,
    pub(crate) attempts: u32,
}

impl Report {
    pub(crate) fn snapshot(&self, cx: &mut JSContext) -> RootedTraceableBox<ExposedReport> {
        rooted!(&in(cx) let mut body = NullValue());
        self.body.to_jsval(cx, body.handle_mut());
        RootedTraceableBox::new(ExposedReport {
            type_: self.type_.clone().into(),
            url: self.url.clone().into(),
            body: Heap::boxed(body.get()),
        })
    }
}

#[derive(Clone, Debug, MallocSizeOf, Serialize)]
#[serde(untagged)]
pub(crate) enum ReportBody {
    Csp(CSPReportingEndpointBody),
    PermissionsPolicy(PermissionsPolicyReportBody),
}

impl ReportBody {
    fn to_jsval(&self, cx: &mut JSContext, rval: MutableHandleValue) {
        match self {
            Self::Csp(body) => body.dictionary().to_jsval(cx, rval),
            Self::PermissionsPolicy(body) => body.dictionary().to_jsval(cx, rval),
        }
    }
}

impl From<CSPViolationReportBody> for ReportBody {
    fn from(body: CSPViolationReportBody) -> Self {
        Self::Csp(body.into())
    }
}

#[derive(Clone, Debug, MallocSizeOf, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PermissionsPolicyReportBody {
    pub(crate) feature_id: String,
    pub(crate) source_file: Option<String>,
    pub(crate) line_number: Option<u32>,
    pub(crate) column_number: Option<u32>,
    pub(crate) disposition: ViolationDisposition,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) allow_attribute: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) src_attribute: Option<String>,
}

impl PermissionsPolicyReportBody {
    fn dictionary(&self) -> PermissionsPolicyViolationReportBody {
        PermissionsPolicyViolationReportBody {
            parent: EmptyReportBody::empty(),
            featureId: self.feature_id.clone().into(),
            sourceFile: self.source_file.clone().map(Into::into),
            lineNumber: self.line_number,
            columnNumber: self.column_number,
            disposition: self.disposition.as_str().into(),
            allowAttribute: self.allow_attribute.clone().map(|value| Some(value.into())),
            srcAttribute: self.src_attribute.clone().map(|value| Some(value.into())),
        }
    }
}

#[derive(Clone, Debug, MallocSizeOf, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CSPReportingEndpointBody {
    sample: Option<String>,
    #[serde(rename = "blockedURL")]
    blocked_url: Option<String>,
    referrer: Option<String>,
    status_code: u16,
    #[serde(rename = "documentURL")]
    document_url: String,
    source_file: Option<String>,
    effective_directive: String,
    line_number: Option<u32>,
    column_number: Option<u32>,
    original_policy: String,
    #[serde(serialize_with = "serialize_disposition")]
    disposition: SecurityPolicyViolationEventDisposition,
}

impl From<CSPViolationReportBody> for CSPReportingEndpointBody {
    fn from(value: CSPViolationReportBody) -> Self {
        CSPReportingEndpointBody {
            sample: value.sample.map(String::from),
            blocked_url: value.blockedURL.map(String::from),
            referrer: value.referrer.map(String::from),
            status_code: value.statusCode,
            document_url: String::from(value.documentURL),
            source_file: value.sourceFile.map(String::from),
            effective_directive: String::from(value.effectiveDirective),
            line_number: value.lineNumber,
            column_number: value.columnNumber,
            original_policy: value.originalPolicy.into(),
            disposition: value.disposition,
        }
    }
}

impl CSPReportingEndpointBody {
    fn dictionary(&self) -> CSPViolationReportBody {
        CSPViolationReportBody {
            parent: EmptyReportBody::empty(),
            sample: self.sample.clone().map(Into::into),
            blockedURL: self.blocked_url.clone().map(Into::into),
            referrer: self.referrer.clone().map(Into::into),
            statusCode: self.status_code,
            documentURL: self.document_url.clone().into(),
            sourceFile: self.source_file.clone().map(Into::into),
            effectiveDirective: self.effective_directive.clone().into(),
            lineNumber: self.line_number,
            columnNumber: self.column_number,
            originalPolicy: self.original_policy.clone().into(),
            disposition: self.disposition,
        }
    }
}
