use std::env;
use std::sync::Arc;

use axum::{Json, http::StatusCode, response::IntoResponse};
use serde::Serialize;
use tokio::net::TcpListener;
use tracing::info;

use crate::{api::router::router, rpc_client::RpcClient};

mod router;
pub struct ApiServer {
    listener: TcpListener,
    rpc_client: Arc<RpcClient>,
}

impl ApiServer {
    pub async fn new(rpc_client: Arc<RpcClient>) -> anyhow::Result<Self> {
        let listen_addr = env::var("LISTEN_ADDR").unwrap_or_else(|_| "0.0.0.0:3000".to_string());
        let listener = TcpListener::bind(&listen_addr).await?;

        info!(addr = %listen_addr, "HTTP server listening");

        Ok(Self {
            listener,
            rpc_client,
        })
    }

    pub async fn run(self) -> anyhow::Result<()> {
        let jwt_secret =
            env::var("JWT_SECRET").unwrap_or_else(|_| "super-secret-change-me".to_string());

        let app_state = AppState {
            rpc_client: self.rpc_client,
            jwt_secret,
        };

        let app = router(app_state);

        axum::serve(self.listener, app)
            .await
            .map_err(|e| anyhow::anyhow!(e))
    }
}

pub struct ApiError {
    status: StatusCode,
    message: String,
}

impl ApiError {
    pub fn new(message: &str) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: message.into(),
        }
    }

    pub fn unauthorized(message: &str) -> Self {
        Self {
            status: StatusCode::UNAUTHORIZED,
            message: message.into(),
        }
    }

    pub fn bad_request(message: &str) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            message: message.into(),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> axum::response::Response {
        #[derive(Serialize)]
        #[serde(rename_all = "camelCase")]
        struct ErrorResponse {
            message: String,
        }

        (
            self.status,
            Json(ErrorResponse {
                message: self.message,
            }),
        )
            .into_response()
    }
}

type ApiResult<T> = Result<Json<T>, ApiError>;

#[derive(Clone)]
pub struct AppState {
    pub rpc_client: Arc<RpcClient>,
    pub jwt_secret: String,
}
