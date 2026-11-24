//! Streamable HTTP transport backed by the official MCP SDK.
use std::sync::Arc;

use axum::{middleware, Router};
use rmcp::{
    model::*,
    service::RequestContext,
    transport::streamable_http_server::{
        session::local::LocalSessionManager, StreamableHttpServerConfig, StreamableHttpService,
    },
    ErrorData, RoleServer, ServerHandler,
};
use serde_json::Value;

use crate::{
    api::AppState,
    api_policy::{self, ApiPolicy},
    mcp,
};

#[derive(Clone)]
struct WeatherMcp(AppState);

impl ServerHandler for WeatherMcp {
    fn get_info(&self) -> ServerConfig {
        let mut info = ServerConfig::default();
        info.capabilities = ServerCapabilities::builder().enable_tools().build();
        info.server_info = Implementation::new("jma-nowcast-rs", env!("CARGO_PKG_VERSION"));
        info
    }

    async fn list_tools(
        &self,
        _: Option<PaginatedRequestParams>,
        _: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        serde_json::from_value(mcp::tool_definitions())
            .map_err(|e| ErrorData::internal_error(e.to_string(), None))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        let result = mcp::execute_tool(
            &self.0.nowcast,
            &self.0.http_client,
            &self.0.short_range_base_url,
            &request.name,
            request.arguments.map(Value::Object).unwrap_or(Value::Null),
        )
        .await;
        serde_json::from_value::<CallToolResult>(result)
            .map(Into::into)
            .map_err(|e| ErrorData::internal_error(e.to_string(), None))
    }
}

pub fn router(state: AppState, policy: Arc<ApiPolicy>) -> Router {
    let mut config = StreamableHttpServerConfig::default();
    config.legacy_session_mode = false;
    config.json_response = true;
    config.allowed_hosts = vec![
        "weather.hisho.uk".into(),
        "localhost".into(),
        "127.0.0.1".into(),
    ];
    config.allowed_origins = vec!["https://weather.hisho.uk".into()];
    let service = StreamableHttpService::new(
        move || Ok(WeatherMcp(state.clone())),
        Arc::new(LocalSessionManager::default()),
        config,
    );
    Router::new()
        .nest_service("/mcp", service)
        .layer(middleware::from_fn_with_state(policy, api_policy::admit))
}
