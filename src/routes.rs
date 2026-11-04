use axum::{
    Json, Router,
    extract::{FromRef, State, rejection::JsonRejection},
    http::{HeaderValue, StatusCode, header::RETRY_AFTER},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde::Serialize;

use crate::{
    admission::{AdmissionOutcome, AdmissionRequest},
    config::{RateLimitConfig, VersionedConfig},
    error::ApiError,
    store::RedisStore,
};

#[derive(Clone)]
pub struct AppState {
    pub store: RedisStore,
}

impl FromRef<AppState> for RedisStore {
    fn from_ref(state: &AppState) -> Self {
        state.store.clone()
    }
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .route("/readyz", get(readyz))
        .route("/v1/config", get(get_config).put(put_config))
        .route("/v1/admissions", post(post_admission))
        .with_state(state)
}

#[derive(Serialize)]
struct StatusBody {
    status: &'static str,
}

async fn healthz() -> impl IntoResponse {
    (StatusCode::OK, Json(StatusBody { status: "ok" }))
}

async fn readyz(State(store): State<RedisStore>) -> Result<impl IntoResponse, ApiError> {
    store.ping().await?;
    store.get_config().await?;
    Ok((StatusCode::OK, Json(StatusBody { status: "ready" })))
}

async fn get_config(State(store): State<RedisStore>) -> Result<Json<VersionedConfig>, ApiError> {
    Ok(Json(store.get_config().await?))
}

async fn put_config(
    State(store): State<RedisStore>,
    payload: Result<Json<RateLimitConfig>, JsonRejection>,
) -> Result<Json<VersionedConfig>, ApiError> {
    let Json(config) = payload.map_err(|rejection| ApiError::BadRequest(rejection.body_text()))?;
    config.validate().map_err(ApiError::BadRequest)?;
    Ok(Json(store.update_config(&config).await?))
}

async fn post_admission(
    State(store): State<RedisStore>,
    payload: Result<Json<AdmissionRequest>, JsonRejection>,
) -> Result<Response, ApiError> {
    let Json(request) = payload.map_err(|rejection| ApiError::BadRequest(rejection.body_text()))?;
    let request = request
        .validate_and_normalize()
        .map_err(ApiError::BadRequest)?;

    for attempt in 0..2 {
        let config = store.get_config().await?;
        let bucket_keys = request.bucket_keys(config.generation);
        match store.admit(config.generation, &bucket_keys).await? {
            AdmissionOutcome::Allowed(response) => {
                return Ok((StatusCode::OK, Json(response)).into_response());
            }
            AdmissionOutcome::Denied {
                response,
                retry_after_ms,
            } => {
                let retry_after_seconds = retry_after_ms.div_ceil(1000);
                let mut http_response =
                    (StatusCode::TOO_MANY_REQUESTS, Json(response)).into_response();
                http_response.headers_mut().insert(
                    RETRY_AFTER,
                    HeaderValue::from_str(&retry_after_seconds.to_string()).map_err(|_| {
                        ApiError::IndeterminateAdmission(
                            "failed to construct Retry-After header".to_owned(),
                        )
                    })?,
                );
                return Ok(http_response);
            }
            AdmissionOutcome::StaleGeneration if attempt == 0 => continue,
            AdmissionOutcome::StaleGeneration => {
                return Err(ApiError::IndeterminateAdmission(
                    "configuration changed during both admission attempts".to_owned(),
                ));
            }
        }
    }

    unreachable!("admission loop always returns")
}
