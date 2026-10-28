use std::sync::Arc;

use actix_web::{App, HttpServer, middleware, web};
use anyhow::{Context, bail};
use nanace_embed::{Embedder, FastEmbedder, HashingEmbedder};
use nanace_server::auth::JwtKeys;
use nanace_server::config::{Config, EmbedderKind};
use nanace_server::oauth::GoogleOAuth;
use nanace_server::upgrades::{self, Status};
use nanace_server::vectors::VectorStore;
use nanace_server::{AppState, LOG_COMPONENT, api, db};
use secrecy::ExposeSecret;
use sqlx::postgres::PgPoolOptions;
use tracing_subscriber::EnvFilter;
use uuid::Uuid;

#[actix_web::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
        .init();

    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        None | Some("serve") => {}
        // Downloads the embedding model into FASTEMBED_CACHE_DIR and exits;
        // container builds use it to ship the model.
        Some("fetch-model") => {
            let cache_dir = std::env::var("FASTEMBED_CACHE_DIR").unwrap_or_else(|_| ".fastembed_cache".to_owned());
            tokio::task::spawn_blocking(move || FastEmbedder::load(cache_dir.into()))
                .await
                .context("embedding model loader panicked")??;
            tracing::info!(
                event = "model_fetched",
                component = LOG_COMPONENT,
                "embedding model is cached"
            );
            return Ok(());
        }
        Some("reindex") => return reindex(&args[1..]).await,
        Some("migrate") => return migrate(&args[1..]).await,
        Some(other) => bail!(
            "unknown command `{other}`; commands: serve (default), migrate [--check], reindex [--org <id>], fetch-model"
        ),
    }

    let config = Config::from_env()?;

    let pool = PgPoolOptions::new()
        .max_connections(20)
        .connect(config.database_url.expose_secret())
        .await
        .context("failed to connect to PostgreSQL")?;
    if !config.run_migrations {
        // Upgrades run separately (`nanace-server migrate`); refuse to serve a
        // schema this binary does not match, before loading the model.
        let status = upgrades::status(&pool, &config.embedder.model_key()).await?;
        if !status.is_current() {
            bail!("{}", describe(&status));
        }
    }
    if db::bypasses_rls(&pool).await? {
        tracing::warn!(
            event = "rls_bypassed",
            component = LOG_COMPONENT,
            "database role is superuser or BYPASSRLS; row-level security is not enforced"
        );
    }

    let embedder = load_embedder(&config).await?;
    let vectors = VectorStore::connect(&config.qdrant_url, embedder.dimensions())?;
    vectors.health().await?;

    if config.run_migrations {
        upgrades::run(&pool, &vectors, embedder.as_ref())
            .await
            .context("failed to upgrade the database")?;
    }

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

async fn load_embedder(config: &Config) -> anyhow::Result<Arc<dyn Embedder>> {
    Ok(match config.embedder {
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
    })
}

/// `nanace-server reindex [--org <id>]`: rebuilds Qdrant from PostgreSQL with
/// the configured embedding model, for one org or all of them.
async fn reindex(args: &[String]) -> anyhow::Result<()> {
    let org = match args {
        [] => None,
        [flag, id] if flag == "--org" => Some(id.parse::<Uuid>().context("--org must be an organization id")?),
        _ => bail!("usage: nanace-server reindex [--org <organization id>]"),
    };
    let config = Config::from_env()?;
    let pool = PgPoolOptions::new()
        .max_connections(4)
        .connect(config.database_url.expose_secret())
        .await
        .context("failed to connect to PostgreSQL")?;
    if config.run_migrations {
        db::MIGRATOR.run(&pool).await.context("failed to run migrations")?;
    }
    let embedder = load_embedder(&config).await?;
    let vectors = VectorStore::connect(&config.qdrant_url, embedder.dimensions())?;
    vectors.health().await?;

    let report = nanace_server::reindex::reindex(&pool, &vectors, embedder.as_ref(), org).await?;
    if org.is_none() {
        // Every org now has vectors from the configured model.
        upgrades::record_embedding_model(&pool, &format!("{}:{}", embedder.model_id(), embedder.dimensions())).await?;
    }
    tracing::info!(
        event = "reindex_done",
        component = LOG_COMPONENT,
        orgs = report.orgs,
        contexts = report.contexts,
        chunks = report.chunks,
        skipped_teams = report.skipped_teams.len(),
        model = embedder.model_id(),
        "reindex finished"
    );
    println!(
        "Reindexed {} organization(s): {} contexts, {} chunks ({}).",
        report.orgs,
        report.contexts,
        report.chunks,
        embedder.model_id()
    );
    for (org_id, team_id) in &report.skipped_teams {
        println!("Skipped team {team_id} in org {org_id}: it has no members, so its contexts cannot be read.");
    }
    Ok(())
}

/// Human-readable summary of what `migrate` would do.
fn describe(status: &Status) -> String {
    if status.is_current() {
        return "The database is up to date.".to_owned();
    }
    let mut lines = Vec::new();
    if status.is_newer() {
        lines.push(format!(
            "The database is newer than this nanace-server (unknown migrations {:?}, unknown upgrades {:?}); \
             run a newer version.",
            status.unknown_migrations, status.unknown_upgrades
        ));
    }
    if !status.pending_migrations.is_empty() {
        lines.push(format!("Pending SQL migrations: {:?}", status.pending_migrations));
    }
    if !status.pending_upgrades.is_empty() {
        let names: Vec<String> = status
            .pending_upgrades
            .iter()
            .filter_map(|v| upgrades::UPGRADES.iter().find(|(u, _)| u == v))
            .map(|(v, name)| format!("{v} ({name})"))
            .collect();
        lines.push(format!("Pending application upgrades: {}", names.join(", ")));
    }
    if let Some((from, to)) = &status.model_change {
        lines.push(format!(
            "Embedding model changed from {from} to {to}: every organization will be reindexed."
        ));
    }
    if !status.is_newer() {
        lines.push("Run `nanace-server migrate` to apply.".to_owned());
    }
    lines.join("\n")
}

/// `nanace-server migrate [--check]`: applies SQL migrations and application
/// upgrades, then exits. `--check` only reports and exits with status 1 when
/// anything is pending.
async fn migrate(args: &[String]) -> anyhow::Result<()> {
    let check = match args {
        [] => false,
        [flag] if flag == "--check" => true,
        _ => bail!("usage: nanace-server migrate [--check]"),
    };
    let config = Config::from_env()?;
    let pool = PgPoolOptions::new()
        .max_connections(4)
        .connect(config.database_url.expose_secret())
        .await
        .context("failed to connect to PostgreSQL")?;
    if check {
        let status = upgrades::status(&pool, &config.embedder.model_key()).await?;
        println!("{}", describe(&status));
        if !status.is_current() {
            std::process::exit(1);
        }
        return Ok(());
    }
    let embedder = load_embedder(&config).await?;
    let vectors = VectorStore::connect(&config.qdrant_url, embedder.dimensions())?;
    vectors.health().await?;
    let report = upgrades::run(&pool, &vectors, embedder.as_ref()).await?;
    println!(
        "Database up to date. Applied upgrades: {:?}{}",
        report.upgrades,
        if report.reindexed {
            "; reindexed every organization for the new embedding model"
        } else {
            ""
        }
    );
    Ok(())
}
