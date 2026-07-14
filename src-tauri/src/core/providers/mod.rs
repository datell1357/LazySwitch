pub mod claude;
pub mod codex;
pub mod codex_api;

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use serde_json::Value;

use crate::core::types::{PAccount, PUsage, ProviderPrefs};
use crate::core::CoreError;

pub type HttpFuture = Pin<Box<dyn Future<Output = Result<HttpResponse, CoreError>> + Send>>;

#[derive(Clone, Debug)]
pub struct HttpRequest {
    pub method: String,
    pub url: String,
    pub headers: HashMap<String, String>,
    pub body: Option<String>,
}

#[derive(Clone, Debug)]
pub struct HttpResponse {
    pub status: u16,
    pub headers: HashMap<String, String>,
    pub body: Option<Value>,
}

pub trait HttpTransport: Send + Sync {
    fn request(&self, request: HttpRequest) -> HttpFuture;
}

#[derive(Clone, Default)]
pub struct ReqwestTransport {
    client: reqwest::Client,
}

impl HttpTransport for ReqwestTransport {
    fn request(&self, request: HttpRequest) -> HttpFuture {
        let client = self.client.clone();
        Box::pin(async move {
            let method = reqwest::Method::from_bytes(request.method.as_bytes())
                .map_err(|e| CoreError::Http(e.to_string()))?;
            let mut builder =
                client
                    .request(method, &request.url)
                    .headers(request.headers.iter().try_fold(
                        reqwest::header::HeaderMap::new(),
                        |mut map, (key, value)| {
                            let name = reqwest::header::HeaderName::try_from(key)
                                .map_err(|e| CoreError::Http(e.to_string()))?;
                            let value = reqwest::header::HeaderValue::try_from(value)
                                .map_err(|e| CoreError::Http(e.to_string()))?;
                            map.insert(name, value);
                            Ok::<_, CoreError>(map)
                        },
                    )?);
            if let Some(body) = request.body {
                builder = builder.body(body);
            }
            let response = builder
                .send()
                .await
                .map_err(|e| CoreError::Http(e.to_string()))?;
            let status = response.status().as_u16();
            let headers = response
                .headers()
                .iter()
                .filter_map(|(key, value)| {
                    Some((
                        key.as_str().to_ascii_lowercase(),
                        value.to_str().ok()?.to_owned(),
                    ))
                })
                .collect();
            let bytes = response
                .bytes()
                .await
                .map_err(|e| CoreError::Http(e.to_string()))?;
            let body = serde_json::from_slice(&bytes).ok();
            Ok(HttpResponse {
                status,
                headers,
                body,
            })
        })
    }
}

pub trait Provider: Send + Sync {
    fn list_accounts(&self) -> Vec<PAccount>;
    fn active_account_name(&self) -> Option<String>;
    fn has_live_auth(&self) -> bool;
    fn import_current(&self, name: Option<&str>) -> Result<PAccount, CoreError>;
    fn remove_account(&self, name: &str) -> Result<(), CoreError>;
    fn rename_account(&self, old_name: &str, new_name: &str) -> Result<(), CoreError>;
    fn set_account_enabled(&self, name: &str, enabled: bool) -> Result<(), CoreError>;
    fn sync_live_back_to_slot(&self) -> Result<(), CoreError>;
    fn install_auth(&self, name: &str) -> Result<(), CoreError>;
    fn fetch_usage<'a>(
        &'a self,
        name: Option<&'a str>,
    ) -> Pin<Box<dyn Future<Output = Option<PUsage>> + Send + 'a>>;
    fn cached_usage(&self, name: Option<&str>) -> Option<PUsage>;
    fn cached_usage_updated_at(&self, name: Option<&str>) -> Option<i64>;
    fn desktop_restart<'a>(
        &'a self,
        _prefs: &'a ProviderPrefs,
    ) -> Pin<Box<dyn Future<Output = bool> + Send + 'a>> {
        Box::pin(async { false })
    }
}

pub type SharedTransport = Arc<dyn HttpTransport>;
