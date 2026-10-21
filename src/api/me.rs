use axum::extract::State;
use axum::Json;
use chrono::{DateTime, Utc};
use serde::Serialize;
use uuid::Uuid;

use crate::auth::extractor::AuthUser;
use crate::error::{internal, not_found, ApiResult, ErrorCode};
use crate::services::users;
use crate::AppState;

/// The authenticated merchant's own profile. The JWT only carries ids, so a
/// frontend that reloads with a stored token needs this to render anything
/// human-readable ("signed in as …") without forcing a re-login.
#[derive(Serialize)]
pub struct MeView {
    pub user_id: Uuid,
    pub email: String,
    pub name: String,
    pub is_admin: bool,
    pub created_at: DateTime<Utc>,
    pub merchant_id: Option<Uuid>,
    pub merchant_name: Option<String>,
}

pub async fn get(State(state): State<AppState>, auth: AuthUser) -> ApiResult<Json<MeView>> {
    let row = users::user_with_merchant_by_id(&state.db, auth.user_id)
        .await
        .map_err(internal)?
        .ok_or_else(|| not_found(ErrorCode::UserNotFound, "user not found"))?;

    Ok(Json(MeView {
        user_id: row.user_id,
        email: row.email,
        name: row.name,
        is_admin: row.is_admin,
        created_at: row.created_at,
        merchant_id: row.merchant_id,
        merchant_name: row.merchant_name,
    }))
}
