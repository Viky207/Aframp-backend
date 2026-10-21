use sqlx::PgPool;

use crate::auth::password;
use crate::models::{Merchant, User};

#[derive(Debug, thiserror::Error)]
pub enum UserError {
    #[error("invalid email or password")]
    InvalidCredentials,
    #[error(transparent)]
    Database(#[from] sqlx::Error),
}

/// A user row joined with their (optional) merchant row, fetched in a single
/// roundtrip via LEFT JOIN. Merchant columns are `None` when the user has no
/// merchant.
#[derive(Debug)]
pub struct UserWithMerchant {
    pub user: User,
    pub merchant: Option<Merchant>,
}

/// Materializes a brand-new account. The only caller is
/// `services::otp::verify`, once a signup's OTP challenge passes — never
/// called speculatively, so an unverified phone can never end up attached
/// to a real, loggable-in account. `password_hash` must already be hashed.
pub async fn create_verified(
    db: &PgPool,
    email: &str,
    password_hash: &str,
    name: &str,
    phone_number: &str,
) -> Result<(User, Merchant), sqlx::Error> {
    let mut tx = db.begin().await?;
    let user = sqlx::query_as::<_, User>(
        "INSERT INTO users (email, password_hash, name, phone_number, phone_verified)
         VALUES ($1, $2, $3, $4, true)
         RETURNING id, email, password_hash, name, is_admin, phone_number, phone_verified, created_at, updated_at",
    )
    .bind(email)
    .bind(password_hash)
    .bind(name)
    .bind(phone_number)
    .fetch_one(&mut *tx)
    .await?;

    let merchant = sqlx::query_as::<_, Merchant>(
        "INSERT INTO merchants (user_id, name)
         VALUES ($1, $2)
         RETURNING id, user_id, name, created_at",
    )
    .bind(user.id)
    .bind(name)
    .fetch_one(&mut *tx)
    .await?;

    tx.commit().await?;
    Ok((user, merchant))
}

/// Which unique constraint a `23505` violation hit, if any — lets a caller
/// distinguish "email taken" from "phone taken" from an unrelated conflict.
pub(crate) fn unique_violation_field(err: &sqlx::Error) -> Option<&str> {
    match err {
        sqlx::Error::Database(db) if db.code().as_deref() == Some("23505") => db.constraint(),
        _ => None,
    }
}

/// Fetches a user and their optional merchant in a single LEFT JOIN query.
pub async fn user_with_merchant_by_email(
    db: &PgPool,
    email: &str,
) -> Result<Option<UserWithMerchant>, sqlx::Error> {
    let row = sqlx::query_as::<_, (User, Option<Merchant>)>(
        "SELECT u.id, u.email, u.password_hash, u.name, u.is_admin, u.phone_number, u.phone_verified, u.created_at, u.updated_at,
                m.id, m.user_id, m.name, m.created_at
           FROM users u
           LEFT JOIN merchants m ON m.user_id = u.id
          WHERE u.email = $1
          LIMIT 1",
    )
    .bind(email)
    .fetch_optional(db)
    .await?;

    Ok(row.map(|(user, merchant)| UserWithMerchant { user, merchant }))
}

pub async fn login(db: &PgPool, email: &str, password_raw: &str) -> Result<(User, Option<Merchant>), UserError> {
    let UserWithMerchant { user, merchant } = user_with_merchant_by_email(db, email)
        .await?
        .ok_or(UserError::InvalidCredentials)?;

    if !password::verify(password_raw, &user.password_hash) {
        return Err(UserError::InvalidCredentials);
    }

    Ok((user, merchant))
}

pub async fn user_by_id(db: &PgPool, user_id: uuid::Uuid) -> Result<Option<User>, sqlx::Error> {
    sqlx::query_as::<_, User>(
        "SELECT id, email, password_hash, name, is_admin, phone_number, phone_verified, created_at, updated_at
           FROM users WHERE id = $1",
    )
    .bind(user_id)
    .fetch_optional(db)
    .await
}

pub async fn merchant_by_user(db: &PgPool, user_id: uuid::Uuid) -> Result<Option<Merchant>, sqlx::Error> {
    sqlx::query_as::<_, Merchant>(
        "SELECT id, user_id, name, created_at FROM merchants WHERE user_id = $1 LIMIT 1",
    )
    .bind(user_id)
    .fetch_optional(db)
    .await
}

pub async fn merchant_by_id(db: &PgPool, merchant_id: uuid::Uuid) -> Result<Option<Merchant>, sqlx::Error> {
    sqlx::query_as::<_, Merchant>(
        "SELECT id, user_id, name, created_at FROM merchants WHERE id = $1",
    )
    .bind(merchant_id)
    .fetch_optional(db)
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verifies the LEFT JOIN yields `merchant: None` for a user with no
    /// merchant row. Requires a live `DATABASE_URL`; skipped otherwise.
    #[tokio::test]
    async fn user_with_merchant_by_email_no_merchant() {
        let Ok(url) = std::env::var("DATABASE_URL") else {
            return;
        };
        let db = match PgPool::connect(&url).await {
            Ok(db) => db,
            Err(_) => return,
        };

        let email = format!("no-merchant-{}@example.test", uuid::Uuid::new_v4());
        let user = sqlx::query_as::<_, User>(
            "INSERT INTO users (email, password_hash, name, phone_number, phone_verified)
             VALUES ($1, $2, $3, $4, true)
             RETURNING id, email, password_hash, name, is_admin, phone_number, phone_verified, created_at, updated_at",
        )
        .bind(&email)
        .bind("x")
        .bind("No Merchant")
        .bind("+10000000000")
        .fetch_one(&db)
        .await
        .expect("insert user");

        let result = user_with_merchant_by_email(&db, &email)
            .await
            .expect("query")
            .expect("user present");

        assert_eq!(result.user.id, user.id);
        assert!(result.merchant.is_none());

        let _ = sqlx::query("DELETE FROM users WHERE id = $1")
            .bind(user.id)
            .execute(&db)
            .await;
    }
}
