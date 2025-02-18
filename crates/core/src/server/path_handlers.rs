//! Handle the `web` part of the bundles.

use std::path::{Path, PathBuf};

use axum::response::{Html, IntoResponse};
use freenet_stdlib::{
    client_api::{ClientRequest, ContractRequest, ContractResponse, HostResponse},
    prelude::*,
};
use tokio::{fs::File, io::AsyncReadExt, sync::mpsc};

use crate::client_events::AuthToken;

use tracing::{debug, instrument};
use super::{
    app_packaging::{WebApp, WebContractError},
    errors::WebSocketApiError,
    http_gateway::HttpGatewayRequest,
    ClientConnection, HostCallbackResult,
};

mod v1;

const ALPHABET: &str = "123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";

#[instrument(level = "debug", skip(request_sender))]
pub(super) async fn contract_home(
    key: String,
    request_sender: HttpGatewayRequest,
    assigned_token: AuthToken,
) -> Result<impl IntoResponse, WebSocketApiError> {
    debug!("contract_home: Converting string key to ContractKey: {}", key);
    let key = ContractKey::from_id(key)
        .map_err(|err| {
            debug!("contract_home: Failed to parse contract key: {}", err);
            WebSocketApiError::InvalidParam {
                error_cause: format!("{err}"),
            }
        })
        .unwrap();
    debug!("contract_home: Successfully parsed contract key");
    let (response_sender, mut response_recv) = mpsc::unbounded_channel();
    debug!("contract_home: Sending NewConnection request");
    request_sender
        .send(ClientConnection::NewConnection {
            callbacks: response_sender,
            assigned_token: Some((assigned_token, key.into())),
        })
        .await
        .map_err(|err| WebSocketApiError::NodeError {
            error_cause: format!("{err}"),
        })
        .unwrap();
    let client_id = if let Some(HostCallbackResult::NewId { id }) = response_recv.recv().await {
        id
    } else {
        return Err(WebSocketApiError::NodeError {
            error_cause: "Couldn't register new client in the node".into(),
        });
    };
    debug!("contract_home: Sending GET request for contract");
    request_sender
        .send(ClientConnection::Request {
            client_id,
            req: Box::new(
                ContractRequest::Get {
                    key,
                    return_contract_code: true,
                }
                .into(),
            ),
            auth_token: None,
        })
        .await
        .map_err(|err| WebSocketApiError::NodeError {
            error_cause: format!("{err}"),
        })
        .unwrap();
    debug!("contract_home: Waiting for GET response");
    let response = match response_recv.recv().await {
        Some(HostCallbackResult::Result {
            result:
                Ok(HostResponse::ContractResponse(ContractResponse::GetResponse {
                    contract,
                    state,
                    ..
                })),
            ..
        }) => match contract {
            Some(contract) => {
                let key = contract.key();
                let path = contract_web_path(&key);
                let web_body = match get_web_body(&path).await {
                    Ok(b) => b.into_response(),
                    Err(err) => match err {
                        WebSocketApiError::NodeError {
                            error_cause: _cause,
                        } => {
                            let state = State::from(state.as_ref());

                            fn err(
                                err: WebContractError,
                                contract: &ContractContainer,
                            ) -> WebSocketApiError {
                                let key = contract.key();
                                tracing::error!("{err}");
                                WebSocketApiError::InvalidParam {
                                    error_cause: format!("failed unpacking contract: {key}"),
                                }
                            }

                            let mut web = WebApp::try_from(state.as_ref())
                                .map_err(|e| err(e, &contract))
                                .unwrap();
                            web.unpack(path).map_err(|e| err(e, &contract)).unwrap();
                            let index = web
                                .get_file("index.html")
                                .map_err(|e| err(e, &contract))
                                .unwrap();
                            let index_body = String::from_utf8(index).map_err(|err| {
                                WebSocketApiError::NodeError {
                                    error_cause: format!("{err}"),
                                }
                            })?;
                            Html(index_body).into_response()
                        }
                        other => {
                            tracing::error!("{other}");
                            return Err(other);
                        }
                    },
                };
                web_body
            }
            None => {
                return Err(WebSocketApiError::MissingContract { key });
            }
        },
        Some(HostCallbackResult::Result {
            result: Err(err), ..
        }) => {
            tracing::error!("error getting contract `{key}`: {err}");
            return Err(WebSocketApiError::AxumError {
                error: err.kind().clone(),
            });
        }
        None => {
            return Err(WebSocketApiError::NodeError {
                error_cause: format!("Contract not found: {key}"),
            });
        }
        other => unreachable!("received unexpected node response: {other:?}"),
    };
    request_sender
        .send(ClientConnection::Request {
            client_id,
            req: Box::new(ClientRequest::Disconnect { cause: None }),
            auth_token: None,
        })
        .await
        .map_err(|err| WebSocketApiError::NodeError {
            error_cause: format!("{err}"),
        })
        .unwrap();
    Ok(response)
}

#[instrument(level = "debug")]
pub(super) async fn variable_content(
    key: String,
    req_path: String,
) -> Result<impl IntoResponse, Box<WebSocketApiError>> {
    debug!("variable_content: Processing request for key: {}, path: {}", key, req_path);
    // compose the correct absolute path
    let key = ContractKey::from_id(key).map_err(|err| WebSocketApiError::InvalidParam {
        error_cause: format!("{err}"),
    })?;
    let base_path = contract_web_path(&key);
    debug!("variable_content: Base path resolved to: {:?}", base_path);
    
    let req_uri = req_path
        .parse()
        .map_err(|err| WebSocketApiError::NodeError {
            error_cause: format!("{err}"),
        })?;
    debug!("variable_content: Parsed request URI: {:?}", req_uri);
    
    let file_path = base_path.join(get_file_path(req_uri)?);
    debug!("variable_content: Full file path to serve: {:?}", file_path);
    debug!("variable_content: Checking if file exists: {}", file_path.exists());

    // serve the file
    let mut serve_file = tower_http::services::fs::ServeFile::new(&file_path);
    let fake_req = axum::http::Request::new(axum::body::Body::empty());
    serve_file
        .try_call(fake_req)
        .await
        .map_err(|err| {
            WebSocketApiError::NodeError {
                error_cause: format!("{err}"),
            }
            .into()
        })
        .map(|r| r.into_response())
}

#[instrument(level = "debug")]
async fn get_web_body(path: &Path) -> Result<impl IntoResponse, WebSocketApiError> {
    debug!("get_web_body: Attempting to read index.html from path: {:?}", path);
    let web_path = path.join("web").join("index.html");
    debug!("get_web_body: Full web path: {:?}", web_path);
    let mut key_file = File::open(&web_path)
        .await
        .map_err(|err| WebSocketApiError::NodeError {
            error_cause: format!("{err}"),
        })?;
    let mut buf = vec![];
    key_file
        .read_to_end(&mut buf)
        .await
        .map_err(|err| WebSocketApiError::NodeError {
            error_cause: format!("{err}"),
        })?;
    let body = String::from_utf8(buf).map_err(|err| WebSocketApiError::NodeError {
        error_cause: format!("{err}"),
    })?;
    Ok(Html(body))
}

fn contract_web_path(key: &ContractKey) -> PathBuf {
    std::env::temp_dir()
        .join("freenet")
        .join("webs")
        .join(key.encoded_contract_id())
        .join("web")
}

#[inline]
fn get_file_path(uri: axum::http::Uri) -> Result<String, Box<WebSocketApiError>> {
    v1::get_file_path(uri)
}
