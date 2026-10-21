mod api;
mod auth;
pub mod blockchain;
mod config;
mod error;
mod middleware;
mod models;
mod pagination;
pub mod otp;
pub mod payments;
pub mod services;
mod validation;

pub use auth::cookie::{CookieConfig, SameSite};
pub use config::{AppConfig, OtpProviderKind, SecretString};

use sqlx::{postgres::PgPoolOptions, PgPool};

#[derive(Clone)]
pub struct AppState {
    pub db: PgPool,
    pub jwt_secret: SecretString,
    pub webhook_secret: SecretString,
    pub wallet_encryption_key: std::sync::Arc<[u8; 32]>,
    pub payment_provider: std::sync::Arc<dyn payments::PaymentProvider>,
    pub otp_provider: std::sync::Arc<dyn otp::OtpProvider>,
    pub otp_hmac_secret: SecretString,
    pub cookie: CookieConfig,
}

/// Minimum allowed value for `STELLAR_POLL_INTERVAL_SECS`.
const MIN_STELLAR_POLL_INTERVAL_SECS: u64 = 10;

/// Parse an optional numeric env var, failing fast on invalid values.
///
/// Returns `Ok(None)` when the variable is unset, `Ok(Some(v))` when it parses,
/// and a descriptive `Err` when it is set but unparseable or below `min`.
fn parse_numeric_env<T>(name: &str, min: Option<T>) -> Result<Option<T>, String>
where
    T: std::str::FromStr + PartialOrd + std::fmt::Display + Copy,
{
    match std::env::var(name) {
        Ok(raw) => {
            let value = raw
                .trim()
                .parse::<T>()
                .map_err(|_| format!("invalid value for {name}: {raw:?} is not a valid number"))?;
            if let Some(min) = min {
                if value < min {
                    return Err(format!(
                        "invalid value for {name}: {value} is below the minimum of {min}"
                    ));
                }
            }
            Ok(Some(value))
        }
        Err(std::env::VarError::NotPresent) => Ok(None),
        Err(std::env::VarError::NotUnicode(_)) => {
            Err(format!("invalid value for {name}: not valid unicode"))
        }
    }
}

/// Resolve the Stellar poll interval, failing fast on invalid configuration.
pub fn stellar_poll_interval_secs() -> Result<u64, String> {
    Ok(parse_numeric_env("STELLAR_POLL_INTERVAL_SECS", Some(MIN_STELLAR_POLL_INTERVAL_SECS))?
        .unwrap_or(60))
}

/// Resolve the database max connections, failing fast on invalid configuration.
pub fn db_max_connections() -> Result<u32, String> {
    Ok(parse_numeric_env("DB_MAX_CONNECTIONS", Some(1u32))?.unwrap_or(5))
}

pub async fn build_state(config: &AppConfig) -> Result<AppState, Box<dyn std::error::Error>> {
    let max_connections = db_max_connections()?;
    let db = PgPoolOptions::new()
        .max_connections(max_connections)
        .connect(&config.database_url)
        .await?;
    let wallet_encryption_key = blockchain::wallet_crypto::parse_key(config.wallet_encryption_key.as_str())?;
    let otp_provider: std::sync::Arc<dyn otp::OtpProvider> = match config.otp_provider {
        OtpProviderKind::Termii => std::sync::Arc::new(otp::termii::TermiiProvider::new(
            config
                .termii_api_key
                .as_ref()
                .expect("TERMII_API_KEY is required when OTP_PROVIDER=termii")
                .as_str()
                .to_string(),
            config
                .termii_sender_id
                .clone()
                .expect("TERMII_SENDER_ID is required when OTP_PROVIDER=termii"),
        )),
        OtpProviderKind::Mock => std::sync::Arc::new(otp::mock::MockOtpProvider),
    };
    Ok(AppState {
        db,
        jwt_secret: config.jwt_secret.clone(),
        webhook_secret: config.webhook_secret.clone(),
        wallet_encryption_key: std::sync::Arc::new(wallet_encryption_key),
        payment_provider: std::sync::Arc::new(payments::paystack::PaystackProvider::new(
            config.paystack_secret_key.as_str().to_string(),
        )),
        otp_provider,
        otp_hmac_secret: config.otp_hmac_secret.clone(),
        cookie: config.cookie,
    })
}

pub fn router(state: AppState) -> axum::Router {
    axum::Router::new()
        .route("/", axum::routing::get(|| async { "aframp" }))
        .route(
            "/health",
            axum::routing::get(|| async { axum::http::StatusCode::NO_CONTENT }),
        )
        .route("/signup", axum::routing::post(api::auth::signup))
        .route("/login", axum::routing::post(api::auth::login))
        .route("/verify-otp", axum::routing::post(api::auth::verify_otp))
        .route("/logout", axum::routing::post(api::auth::logout))
        .route("/webhooks/termii", axum::routing::post(api::webhooks::termii))
        .route("/me", axum::routing::get(api::me::get))
        .route("/wallet/create", axum::routing::post(api::wallets::create))
        .route("/wallet", axum::routing::get(api::wallets::get))
        .route("/balance", axum::routing::get(api::balances::get))
        .route("/transactions", axum::routing::get(api::transactions::list))
        .route("/withdraw", axum::routing::post(api::withdrawals::create))
        .route("/withdrawals", axum::routing::get(api::withdrawals::list))
        .route(
            "/payment-requests",
            axum::routing::post(api::payment_requests::create)
                .get(api::payment_requests::list),
        )
        .route(
            "/payment-requests/{id}",
            axum::routing::get(api::payment_requests::get),
        )
        .route("/admin", axum::routing::get(api::admin::dashboard))
        .route("/admin/overview", axum::routing::get(api::admin::overview))
        .route("/admin/merchants", axum::routing::get(api::admin::merchants))
        .route("/admin/users", axum::routing::get(api::admin::users))
        .route("/admin/wallets", axum::routing::get(api::admin::wallets))
        .route("/admin/transactions", axum::routing::get(api::admin::transactions))
        .route("/admin/withdrawals", axum::routing::get(api::admin::withdrawals))
        .route(
            "/admin/payment-requests",
            axum::routing::get(api::admin::payment_requests),
        )
        .with_state(state)
        .layer(axum::middleware::from_fn(middleware::require_json_content_type))
}

#[cfg(test)]
mod config_env_tests {
    use super::*;

    #[test]
    fn invalid_stellar_poll_interval_returns_err() {
        std::env::set_var("STELLAR_POLL_INTERVAL_SECS", "abc");
        let result = stellar_poll_interval_secs();
        std::env::remove_var("STELLAR_POLL_INTERVAL_SECS");
        assert!(result.is_err(), "expected Err for unparseable poll interval");
        assert!(result.unwrap_err().contains("STELLAR_POLL_INTERVAL_SECS"));
    }

    #[test]
    fn too_low_stellar_poll_interval_returns_err() {
        std::env::set_var("STELLAR_POLL_INTERVAL_SECS", "1");
        let result = stellar_poll_interval_secs();
        std::env::remove_var("STELLAR_POLL_INTERVAL_SECS");
        assert!(result.is_err(), "expected Err for poll interval below minimum");
    }

    #[test]
    fn invalid_db_max_connections_returns_err() {
        std::env::set_var("DB_MAX_CONNECTIONS", "not-a-number");
        let result = db_max_connections();
        std::env::remove_var("DB_MAX_CONNECTIONS");
        assert!(result.is_err(), "expected Err for unparseable DB_MAX_CONNECTIONS");
    }
}
