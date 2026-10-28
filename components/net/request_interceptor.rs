/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

use content_security_policy::Destination;
use embedder_traits::{GenericEmbedderProxy, WebResourceRequest, WebResourceResponseMsg};
use log::error;
use net_traits::http_status::HttpStatus;
use net_traits::request::{Request, RequestMode};
use net_traits::response::{Response, ResponseBody};
use net_traits::{NetworkError, ResourceFetchTimingContainer};

use crate::embedder::NetToEmbedderMsg;

#[derive(Clone)]
pub struct RequestInterceptor {
    embedder_proxy: GenericEmbedderProxy<NetToEmbedderMsg>,
}

impl RequestInterceptor {
    pub fn new(embedder_proxy: GenericEmbedderProxy<NetToEmbedderMsg>) -> RequestInterceptor {
        RequestInterceptor { embedder_proxy }
    }

    pub(crate) async fn intercept_request(
        &self,
        request: &mut Request,
        response: &mut Option<Response>,
        timing_info: &ResourceFetchTimingContainer,
    ) {
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        let is_for_main_frame = request.destination == Destination::Document &&
            request.mode == RequestMode::Navigate &&
            request.client.as_ref().is_none_or(|client| !client.is_nested_browsing_context);
        // Document subresources already carry the ancestor-aware, unmasked
        // top-level origin in their cookie context. Never use the UI URL here:
        // history notifications may lag behind the new document's first loads.
        let top_level_origin = if is_for_main_frame {
            Some(request.current_url().origin())
        } else {
            request.cookie_client().and_then(|client| client.cookie_site.clone())
        }.filter(|origin| origin.is_tuple()).map(|origin| origin.ascii_serialization().into_owned());
        let web_resource_request = WebResourceRequest {
            method: request.method.clone(),
            url: request.current_url(),
            headers: request.headers.clone(),
            destination: request.destination,
            is_beacon: request.is_beacon,
            is_websocket: matches!(request.mode, RequestMode::WebSocket { .. }),
            referrer_url: request.referrer.to_url().cloned(),
            initiator_origin: match &request.origin {
                net_traits::request::Origin::Origin(origin) => { let text=origin.ascii_serialization(); (text!="null").then(||text.into_owned()) },
                _ => None,
            },
            top_level_origin,
            is_for_main_frame,
            is_redirect: request.redirect_count > 0,
        };

        self.embedder_proxy
            .send(NetToEmbedderMsg::WebResourceRequested(
                request.target_webview_id,
                web_resource_request,
                sender,
            ));

        // TODO: use done_chan and run in CoreResourceThreadPool.
        let mut accumulated_body = Vec::new();
        while let Some(message) = receiver.recv().await {
            match message {
                WebResourceResponseMsg::RemoveQueryParameters(url) => {
                    if response.is_some() || !request.remove_query_parameters(&url) {
                        *response = Some(Response::network_error(NetworkError::LoadCancelled));
                    }
                    break;
                },
                WebResourceResponseMsg::Start(webresource_response) => {
                    let timing = timing_info.inner().clone();
                    let mut response_override =
                        Response::new(webresource_response.url.into(), timing);
                    response_override.headers = webresource_response.headers;
                    response_override.status = HttpStatus::new(
                        webresource_response.status_code,
                        webresource_response.status_message,
                    );
                    *response = Some(response_override);
                },
                WebResourceResponseMsg::SendBodyData(data) => {
                    accumulated_body.push(data);
                },
                WebResourceResponseMsg::FinishLoad => {
                    if accumulated_body.is_empty() {
                        break;
                    }
                    let Some(response) = response.as_mut() else {
                        error!("Received unexpected FinishLoad message");
                        break;
                    };
                    *response.body.lock() =
                        ResponseBody::Done(accumulated_body.into_iter().flatten().collect());
                    break;
                },
                WebResourceResponseMsg::CancelLoad => {
                    *response = Some(Response::network_error(NetworkError::LoadCancelled));
                    break;
                },
                WebResourceResponseMsg::DoNotIntercept => break,
            }
        }
    }
}
