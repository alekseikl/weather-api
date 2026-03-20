use std::env;
use std::sync::Arc;

use axum::{
    Json, Router,
    extract::{FromRequestParts, Path, State},
    http::{StatusCode, request::Parts},
    middleware,
    response::IntoResponse,
    routing::{get, post},
};
use axum_extra::{
    TypedHeader,
    headers::{Authorization, authorization::Bearer},
};
use chrono::{Duration, Utc};
use jsonwebtoken::{DecodingKey, EncodingKey, Header, Validation, decode, encode};
use serde::{Deserialize, Serialize};
use tokio::net::TcpListener;
use tower_http::cors::CorsLayer;
use tower_http::trace::TraceLayer;
use tracing::{error, info};

use crate::rpc_client::{Location, RpcClient};

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

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct AuthResponse {
    token: String,
}

#[derive(Clone)]
pub struct AppState {
    pub rpc_client: Arc<RpcClient>,
    pub jwt_secret: String,
}

#[derive(Serialize, Deserialize)]
struct Claims {
    sub: String,
    exp: usize,
}

#[derive(Clone, Debug)]
pub struct CurrentUser {
    pub username: String,
}

impl<S: Send + Sync> FromRequestParts<S> for CurrentUser {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        parts
            .extensions
            .get::<CurrentUser>()
            .cloned()
            .ok_or(ApiError::unauthorized("Missing authentication"))
    }
}

#[derive(Deserialize)]
struct LoginRequest {
    username: String,
    password: String,
}

pub fn router(state: AppState) -> Router {
    let protected = Router::new()
        .route("/auth/me", get(me))
        .route("/locations", get(get_locations))
        .route("/locations/{id}", get(get_location))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            auth_middleware,
        ));

    Router::new()
        .route("/auth/login", post(login))
        .merge(protected)
        .layer(TraceLayer::new_for_http())
        .layer(CorsLayer::permissive())
        .with_state(state)
}

async fn login(
    State(state): State<AppState>,
    Json(payload): Json<LoginRequest>,
) -> ApiResult<AuthResponse> {
    if payload.username != "admin" || payload.password != "password" {
        return Err(ApiError::unauthorized("Invalid credentials"));
    }

    let expiration = Utc::now() + Duration::hours(24);
    let claims = Claims {
        sub: payload.username,
        exp: expiration.timestamp() as usize,
    };

    match encode(
        &Header::default(),
        &claims,
        &EncodingKey::from_secret(state.jwt_secret.as_bytes()),
    ) {
        Ok(token) => Ok(Json(AuthResponse { token })),
        Err(_) => Err(ApiError::new("Failed to create token")),
    }
}

async fn auth_middleware(
    State(state): State<AppState>,
    TypedHeader(auth): TypedHeader<Authorization<Bearer>>,
    mut request: axum::http::Request<axum::body::Body>,
    next: middleware::Next,
) -> impl IntoResponse {
    let token_data = decode::<Claims>(
        auth.token(),
        &DecodingKey::from_secret(state.jwt_secret.as_bytes()),
        &Validation::default(),
    );

    match token_data {
        Ok(data) => {
            request.extensions_mut().insert(CurrentUser {
                username: data.claims.sub,
            });
            Ok(next.run(request).await)
        }
        Err(_) => Err(ApiError::unauthorized("Invalid or expired token")),
    }
}

async fn me(user: CurrentUser) -> impl IntoResponse {
    Json(serde_json::json!({ "username": user.username }))
}

async fn get_locations(
    user: CurrentUser,
    State(state): State<AppState>,
) -> ApiResult<Vec<Location>> {
    info!(username = %user.username, "Fetching all locations");

    match state.rpc_client.get_locations().await {
        Ok(locations) => Ok(Json(locations)),
        Err(e) => {
            error!(error = %e, "Failed to fetch locations");
            Err(ApiError::new("Failed to fetch locations"))
        }
    }
}

async fn get_location(
    user: CurrentUser,
    State(state): State<AppState>,
    Path(id): Path<i32>,
) -> ApiResult<Location> {
    info!(username = %user.username, location_id = id, "Fetching location");

    match state.rpc_client.get_location(id).await {
        Ok(location) => Ok(Json(location)),
        Err(e) => {
            error!(error = %e, location_id = id, "Failed to fetch location");
            Err(ApiError::bad_request("Location not found"))
        }
    }
}
