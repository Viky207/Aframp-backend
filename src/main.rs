use std::net::SocketAddr;
use std::sync::Arc;

use aframp::{build_state, router, AppConfig};
use axum::http::{header, HeaderValue, Method};
use tower_http::cors::CorsLayer;
use tower_http::limit::RequestBodyLimitLayer;
use tower_http::trace::TraceLayer;

/// Minimum poll interval (seconds) accepted for `STELLAR_POLL_INTERVAL_SECS`.
/// A very low value would hammer Horizon with requests.
const MIN_STELLAR_POLL_INTERVAL_SECS: u64 = 10;

/// Parse a numeric env var, failing fast on an unparseable value instead of
/// silently falling back to a default.
fn parse_env_u64(name: &str, default: u64) -> Result<u64, Box<dyn std::error::Error>> {
    match std::env::var(name) {
        Ok(raw) => raw.parse::<u64>().map_err(|e| {
            format!("invalid value for {name}: {raw:?} is not a valid u64 ({e})").into()
        }),
        Err(std::env::VarError::NotPresent) => Ok(default),
        Err(e) => Err(format!("could not read {name}: {e}").into()),
    }
}

/// Resolve the Stellar poll interval, validating that it is set to a sane value.
fn stellar_poll_interval_secs() -> Result<u64, Box<dyn std::error::Error>> {
    let secs = parse_env_u64("STELLAR_POLL_INTERVAL_SECS", 60)?;
    if secs < MIN_STELLAR_POLL_INTERVAL_SECS {
        return Err(format!(
            "STELLAR_POLL_INTERVAL_SECS={secs} is too low; must be >= {MIN_STELLAR_POLL_INTERVAL_SECS} seconds"
        )
        .into());
    }
    Ok(secs)
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".into()),
        )
        .init();

    let config = AppConfig::from_env()?;
    let poll_interval_secs = stellar_poll_interval_secs()?;
    let state = Arc::new(build_state(&config).await?);

    let listener = aframp::blockchain::worker::run(
        state.clone(),
        config.stellar_horizon_url.clone(),
        poll_interval_secs,
    );
    tokio::spawn(listener);

    // Auth travels as an HttpOnly cookie for browsers, so credentials are on —
    // which means origins must be listed explicitly, never mirrored back.
    let origins = config
        .cors_allowed_origins
        .iter()
        .map(|origin| origin.parse::<HeaderValue>())
        .collect::<Result<Vec<_>, _>>()?;
    tracing::info!(?origins, "cors allowed origins");

    if config.cookie.same_site == aframp::SameSite::None {
        tracing::warn!(
            "COOKIE_SAME_SITE=none sends the session on cross-site requests; \
             serve the frontend same-origin instead if you can, or add CSRF tokens"
        );
    }

    let cors = CorsLayer::new()
        .allow_origin(origins)
        .allow_credentials(true)
        .allow_methods([Method::GET, Method::POST])
        .allow_headers([header::AUTHORIZATION, header::CONTENT_TYPE]);

    let app = router((*state).clone())
        .layer(cors)
        .layer(TraceLayer::new_for_http())
        .layer(RequestBodyLimitLayer::new(1024 * 1024));

    let address: SocketAddr = config.bind_addr.parse()?;
    tracing::info!(%address, "aframp started");
    axum::serve(tokio::net::TcpListener::bind(address).await?, app).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_poll_interval_is_rejected() {
        std::env::set_var("STELLAR_POLL_INTERVAL_SECS", "abc");
        let result = stellar_poll_interval_secs();
        std::env::remove_var("STELLAR_POLL_INTERVAL_SECS");
        let err = result.expect_err("unparseable poll interval must be an error");
        assert!(err.to_string().contains("STELLAR_POLL_INTERVAL_SECS"));
    }

    #[test]
    fn too_low_poll_interval_is_rejected() {
        std::env::set_var("STELLAR_POLL_INTERVAL_SECS", "1");
        let result = stellar_poll_interval_secs();
        std::env::remove_var("STELLAR_POLL_INTERVAL_SECS");
        assert!(result.is_err());
    }

    #[test]
    fn valid_poll_interval_is_accepted() {
        std::env::set_var("STELLAR_POLL_INTERVAL_SECS", "30");
        let result = stellar_poll_interval_secs();
        std::env::remove_var("STELLAR_POLL_INTERVAL_SECS");
        assert_eq!(result.unwrap(), 30);
    }
}
