use argon2::{
    Argon2, Params, PasswordHash, PasswordHasher, PasswordVerifier, Version,
    password_hash::{SaltString, rand_core::OsRng},
};
use axum::{
    Json, Router,
    extract::{FromRequestParts, Path, State},
    http::request::Parts,
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
use sqlx::Row;
use tower_http::cors::CorsLayer;
use tower_http::trace::TraceLayer;
use tracing::{error, info};

use crate::{
    api::{ApiError, ApiResult, AppState},
    rpc_client::Location,
};

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct AuthResponse {
    token: String,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Claims {
    user_id: i32,
    email: String,
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
struct RegisterRequest {
    email: String,
    password: String,
}

#[derive(Deserialize)]
struct LoginRequest {
    email: String,
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
        .route("/auth/register", post(register))
        .route("/auth/login", post(login))
        .merge(protected)
        .layer(TraceLayer::new_for_http())
        .layer(CorsLayer::permissive())
        .with_state(state)
}

pub fn hash_password(pass: &str) -> Option<String> {
    let arg2 = Argon2::new(
        argon2::Algorithm::Argon2id,
        argon2::Version::V0x13,
        Params::default(),
    );
    let salt = SaltString::generate(&mut OsRng);

    arg2.hash_password(pass.as_bytes(), &salt)
        .ok()
        .map(|hash| hash.to_string())
}

pub fn verify_password(pass: &str, hashed_password: &str) -> bool {
    let arg2 = Argon2::new(
        argon2::Algorithm::Argon2id,
        Version::V0x13,
        Params::default(),
    );
    let Ok(hash) = PasswordHash::new(hashed_password) else {
        return false;
    };

    arg2.verify_password(pass.as_bytes(), &hash).is_ok()
}

async fn register(
    State(state): State<AppState>,
    Json(request): Json<RegisterRequest>,
) -> ApiResult<()> {
    let email = request.email.trim();
    let password = request.password.trim();

    if email.is_empty() {
        return Err(ApiError::bad_request("Email required"));
    }

    if password.is_empty() {
        return Err(ApiError::bad_request("Password required"));
    }

    let Ok(row_opt) = sqlx::query("SELECT id FROM users WHERE email = $1")
        .bind(email)
        .fetch_optional(&state.pool)
        .await
    else {
        return Err(ApiError::internal());
    };

    if row_opt.is_some() {
        return Err(ApiError::bad_request("User exists"));
    }

    let Some(pwd_hash) = hash_password(password) else {
        return Err(ApiError::internal());
    };

    if sqlx::query("INSERT INTO users (email, password) VALUES ($1, $2)")
        .bind(email)
        .bind(pwd_hash)
        .execute(&state.pool)
        .await
        .is_err()
    {
        return Err(ApiError::internal());
    }

    Ok(Json(()))
}

async fn login(
    State(state): State<AppState>,
    Json(request): Json<LoginRequest>,
) -> ApiResult<AuthResponse> {
    let email = request.email.trim();
    let password = request.password.trim();

    if email.is_empty() {
        return Err(ApiError::bad_request("Email required"));
    }

    if password.is_empty() {
        return Err(ApiError::bad_request("Password required"));
    }

    let Ok(row_opt) = sqlx::query("SELECT id, email, password FROM users WHERE email = $1")
        .bind(email)
        .fetch_optional(&state.pool)
        .await
    else {
        return Err(ApiError::internal());
    };

    let Some(row) = row_opt else {
        return Err(ApiError::bad_request("Wrong email/password"));
    };

    let user_id: i32 = row.get("id");
    let email: &str = row.get("email");
    let hashed_password: &str = row.get("password");

    if !verify_password(password, hashed_password) {
        return Err(ApiError::bad_request("Wrong email/password"));
    }

    let expiration = Utc::now() + Duration::hours(24);
    let claims = Claims {
        user_id,
        email: email.into(),
        exp: expiration.timestamp() as usize,
    };

    match encode(
        &Header::default(),
        &claims,
        &EncodingKey::from_secret(state.jwt_secret.as_bytes()),
    ) {
        Ok(token) => Ok(Json(AuthResponse { token })),
        Err(_) => Err(ApiError::internal()),
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
                username: data.claims.email,
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
