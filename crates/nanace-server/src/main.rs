use std::sync::Arc;

use actix_web::{App, HttpServer, middleware, web};
use anyhow::Context;
use nanace_embed::{Embedder, FastEmbedder, HashingEmbedder};
use nanace_server::auth::JwtKeys;
use nanace_server::config::{Config, EmbedderKind};
use nanace_server::oauth::GoogleOAuth;
use nanace_server::vectors::VectorStore;
use nanace_server::{AppState, LOG_COMPONENT, api, db};
use secrecy::ExposeSecret;
use sqlx::postgres::PgPoolOptions;
use tracing_subscriber::EnvFilter;

#[actix_web::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
        .init();

    let config = Config::from_env()?;

    let pool = PgPoolOptions::new()
        .max_connections(20)
        .connect(config.database_url.expose_secret())
        .await
        .context("failed to connect to PostgreSQL")?;
    if config.run_migrations {
        db::MIGRATOR.run(&pool).await.context("failed to run migrations")?;
    }
    if db::bypasses_rls(&pool).await? {
        tracing::warn!(
            event = "rls_bypassed",
            component = LOG_COMPONENT,
            "database role is superuser or BYPASSRLS; row-level security is not enforced"
        );
    }

    let embedder: Arc<dyn Embedder> = match config.embedder {
        EmbedderKind::FastEmbed => {
            let cache_dir = config.fastembed_cache_dir.clone();
            Arc::new(
                tokio::task::spawn_blocking(move || FastEmbedder::load(cache_dir))
                    .await
                    .context("embedding model loader panicked")??,
            )
        }
        EmbedderKind::Hashing => {
            tracing::warn!(
                event = "embedder_selected",
                component = LOG_COMPONENT,
                embedder = "hashing",
                "lexical hashing embedder in use; not for production"
            );
            Arc::new(HashingEmbedder::new(384))
        }
    };

    let vectors = VectorStore::connect(&config.qdrant_url, embedder.dimensions())?;
    vectors.health().await?;

    if config.dev_login {
        tracing::warn!(
            event = "dev_login_enabled",
            component = LOG_COMPONENT,
            "dev login issues tokens without verification; never enable in production"
        );
    }

    let google = config
        .google
        .clone()
        .map(|google| GoogleOAuth::new(google, &config.public_url, &config.jwt_secret))
        .transpose()?;
    if google.is_none() {
        tracing::info!(
            event = "google_login_disabled",
            component = LOG_COMPONENT,
            "Google sign-in not configured"
        );
    }

    let state = web::Data::new(AppState {
        pool,
        vectors,
        embedder,
        jwt: JwtKeys::new(&config.jwt_secret, config.jwt_ttl_secs),
        dev_login: config.dev_login,
        google,
        frontend_url: config.frontend_url.clone(),
        secure_cookies: config.public_url.starts_with("https://"),
    });

    tracing::info!(event = "server_start", component = LOG_COMPONENT, bind = %config.bind, "listening");
    HttpServer::new(move || {
        App::new()
            .app_data(state.clone())
            .wrap(middleware::Logger::default())
            .configure(api::configure)
    })
    .bind(config.bind)?
    .run()
    .await?;
    Ok(())
}
