use std::net::SocketAddr;
use std::path::PathBuf;

use anyhow::{Context, bail};
use secrecy::{ExposeSecret, SecretString};

/// Minimum HS256 secret length; shorter secrets are brute-forceable.
pub const MIN_JWT_SECRET_BYTES: usize = 32;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EmbedderKind {
    /// Local bge-small-en-v1.5 via fastembed.
    FastEmbed,
    /// Deterministic lexical hashing; tests and offline development only.
    Hashing,
}

#[derive(Debug)]
pub struct Config {
    pub bind: SocketAddr,
    pub database_url: SecretString,
    pub qdrant_url: String,
    pub jwt_secret: SecretString,
    pub jwt_ttl_secs: u64,
    pub embedder: EmbedderKind,
    pub fastembed_cache_dir: PathBuf,
    pub dev_login: bool,
    pub run_migrations: bool,
}

impl Config {
    pub fn from_env() -> anyhow::Result<Self> {
        Self::from_lookup(|key| std::env::var(key).ok())
    }

    pub fn from_lookup(get: impl Fn(&str) -> Option<String>) -> anyhow::Result<Self> {
        let required = |key: &str| get(key).with_context(|| format!("{key} must be set"));
        let flag = |key: &str, default: bool| -> anyhow::Result<bool> {
            match get(key).as_deref() {
                None => Ok(default),
                Some("1" | "true") => Ok(true),
                Some("0" | "false") => Ok(false),
                Some(other) => bail!("{key} must be true or false, got `{other}`"),
            }
        };

        let jwt_secret = SecretString::from(required("NANACE_JWT_SECRET")?);
        if jwt_secret.expose_secret().len() < MIN_JWT_SECRET_BYTES {
            bail!("NANACE_JWT_SECRET must be at least {MIN_JWT_SECRET_BYTES} bytes");
        }

        let embedder = match get("NANACE_EMBEDDER").as_deref() {
            None | Some("fastembed") => EmbedderKind::FastEmbed,
            Some("hashing") => EmbedderKind::Hashing,
            Some(other) => bail!("NANACE_EMBEDDER must be `fastembed` or `hashing`, got `{other}`"),
        };

        let jwt_ttl_secs = match get("NANACE_JWT_TTL_SECS") {
            None => 3600,
            Some(raw) => raw
                .parse::<u64>()
                .ok()
                .filter(|ttl| (60..=86_400).contains(ttl))
                .context("NANACE_JWT_TTL_SECS must be an integer between 60 and 86400")?,
        };

        Ok(Self {
            bind: get("NANACE_BIND")
                .unwrap_or_else(|| "127.0.0.1:8080".to_owned())
                .parse()
                .context("NANACE_BIND must be a socket address")?,
            database_url: SecretString::from(required("DATABASE_URL")?),
            qdrant_url: get("QDRANT_URL").unwrap_or_else(|| "http://127.0.0.1:6334".to_owned()),
            jwt_secret,
            jwt_ttl_secs,
            embedder,
            fastembed_cache_dir: get("FASTEMBED_CACHE_DIR")
                .unwrap_or_else(|| ".fastembed_cache".to_owned())
                .into(),
            dev_login: flag("NANACE_DEV_LOGIN", false)?,
            run_migrations: flag("NANACE_RUN_MIGRATIONS", true)?,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    fn config(pairs: &[(&str, &str)]) -> anyhow::Result<Config> {
        let mut env: HashMap<String, String> = [
            ("DATABASE_URL", "postgres://localhost/nanace"),
            ("NANACE_JWT_SECRET", "0123456789abcdef0123456789abcdef"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_owned(), v.to_owned()))
        .collect();
        for (k, v) in pairs {
            env.insert((*k).to_owned(), (*v).to_owned());
        }
        Config::from_lookup(|key| env.get(key).cloned())
    }

    #[test]
    fn defaults_are_safe() {
        let cfg = config(&[]).unwrap();
        assert!(!cfg.dev_login);
        assert!(cfg.run_migrations);
        assert_eq!(cfg.embedder, EmbedderKind::FastEmbed);
        assert_eq!(cfg.jwt_ttl_secs, 3600);
        assert!(cfg.bind.ip().is_loopback());
    }

    #[test]
    fn rejects_weak_or_malformed_settings() {
        assert!(config(&[("NANACE_JWT_SECRET", "short")]).is_err());
        assert!(config(&[("NANACE_DEV_LOGIN", "yes")]).is_err());
        assert!(config(&[("NANACE_EMBEDDER", "openai")]).is_err());
        assert!(config(&[("NANACE_JWT_TTL_SECS", "59")]).is_err());
        assert!(config(&[("NANACE_JWT_TTL_SECS", "86401")]).is_err());
        assert!(config(&[("NANACE_BIND", "localhost")]).is_err());
    }

    #[test]
    fn debug_output_redacts_secrets() {
        let rendered = format!("{:?}", config(&[]).unwrap());
        assert!(!rendered.contains("0123456789abcdef"));
        assert!(!rendered.contains("postgres://"));
    }
}
