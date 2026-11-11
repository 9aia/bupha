//! Registration: turn a short-lived registration token (or a PAT
//! that mints one) into credentials that last — the equivalent of
//! what `config.sh` writes to `.runner`.
//!
//! Three calls, then the private half of a generated RSA key stays
//! in the credentials file: it signs the JWT assertions that buy
//! access tokens and unwraps session keys, so the registration token
//! is needed once and can expire.

use serde::{Deserialize, Serialize};

use crate::client::Client;
use crate::crypto;
use crate::types::*;
use crate::{Config, Error, Registration};

/// What `config.sh` persists in `.runner`, under our own file name.
/// Contains the runner's *private* key — keep it 0600.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Credentials {
    pub service_url: String,
    pub pool_id: i64,
    pub agent_id: i64,
    #[serde(default)]
    pub agent_name: String,
    pub client_id: String,
    pub token_url: String,
    pub private_key: String,
}

impl Credentials {
    pub fn read(path: &std::path::Path) -> Result<Self, Error> {
        crate::read_json(path).map_err(Error::Io)
    }

    pub fn write(&self, path: &std::path::Path) -> Result<(), Error> {
        crate::write_json(path, self).map_err(Error::Io)
    }
}

/// Mint a registration token with a PAT — `POST
/// /repos/{repo}/actions/runners/registration-token`.
pub fn mint_registration_token(
    client: &mut Client,
    api_base: &str,
    pat: &str,
    repo_slug: &str,
) -> Result<String, Error> {
    client.registration_token(api_base, pat, repo_slug)
}

/// Full registration flow → credentials. `client` is used for the
/// whole exchange; its auth is left as the tenant bearer.
pub fn register(client: &mut Client, reg: &Registration) -> Result<Credentials, Error> {
    let tenant = client.tenant(&reg.url, &reg.token)?;
    client.bearer(&tenant.token);

    let pool = client.default_pool(&tenant.url)?;
    let key = crypto::generate_key()?;
    let agent = client.create_agent(
        &tenant.url,
        pool,
        &AgentRequest {
            name: reg.name.clone(),
            version: RUNNER_VERSION,
            os_description: format!("{} {}", crate::host_os(), std::env::consts::ARCH),
            max_parallelism: 1,
            ephemeral: reg.ephemeral,
            disable_update: true,
            provisioning_state: "Provisioned",
            labels: reg.labels.clone(),
            authorization: PublicKeyRequest {
                public_key: crypto::public_key(&key),
            },
        },
    )?;

    Ok(Credentials {
        service_url: tenant.url,
        pool_id: pool,
        agent_id: agent.id,
        agent_name: reg.name.clone(),
        client_id: agent.authorization.client_id,
        token_url: agent.authorization.authorization_url,
        private_key: crypto::private_key_pem(&key)?,
    })
}

/// Resolve credentials for `cfg`: load the persisted file when it
/// exists; otherwise register (token from env/file, or minted from a
/// PAT) and persist the result.
pub fn ensure_credentials(client: &mut Client, cfg: &Config) -> Result<Credentials, Error> {
    if cfg.credentials.exists() {
        return Credentials::read(&cfg.credentials);
    }
    let token = resolve_registration_token(client, cfg)?;
    let creds = register(
        client,
        &Registration {
            url: cfg.repo_url(),
            token,
            name: cfg.runner_name(),
            labels: cfg.labels(),
            ephemeral: cfg.ephemeral,
        },
    )?;
    creds.write(&cfg.credentials)?;
    Ok(creds)
}

fn resolve_registration_token(client: &mut Client, cfg: &Config) -> Result<String, Error> {
    if let Some(t) = crate::resolve_token(cfg) {
        return Ok(t);
    }
    if !cfg.pat_env.is_empty() {
        if let Ok(pat) = std::env::var(&cfg.pat_env) {
            let pat = pat.trim().to_string();
            if !pat.is_empty() {
                return client.registration_token(&cfg.api_base(), &pat, &cfg.repo_slug());
            }
        }
    }
    Err(Error::Protocol(format!(
        "no registration token — set ${} (or ${} to mint one from a PAT), \
         or drop a token in {}",
        cfg.token_env,
        cfg.pat_env,
        cfg.token_file.display()
    )))
}
