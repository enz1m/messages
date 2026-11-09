use std::sync::Arc;

use enzui_services::credential_store::{
    CredentialKey, CredentialOutcome, CredentialStore, Secret, UnavailableCredentialStore,
    mutation_owner::CredentialMutationOwner, secret_service::SecretServiceTransport,
};
use messages_protocols::login::Secret as Token;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub struct CredentialRef {
    pub account: Uuid,
    pub key: Uuid,
}

impl CredentialRef {
    pub fn store_key(self) -> CredentialKey {
        CredentialKey::new(
            "io.enz.Messages",
            self.account.to_string(),
            format!("access-token-{}", self.key),
        )
    }
}

#[derive(Clone, Debug)]
pub struct CredentialIssue {
    pub summary: &'static str,
    pub silent: bool,
}

impl CredentialIssue {
    fn from_outcome(outcome: &CredentialOutcome) -> Self {
        Self {
            summary: outcome.user_summary(),
            silent: outcome.is_cancellation(),
        }
    }
}

pub struct Credentials {
    store: Arc<dyn CredentialStore>,
    owner: Option<CredentialMutationOwner>,
}

impl std::fmt::Debug for Credentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Credentials")
            .field("owned_mutations", &self.owner.is_some())
            .finish_non_exhaustive()
    }
}

impl Credentials {
    pub async fn connect() -> Arc<Self> {
        if std::env::var_os("MESSAGES_DISABLE_KEYRING").is_none()
            && let Ok((owner, store)) = SecretServiceTransport::connect_with_mutation_owner().await
        {
            return Arc::new(Self {
                store: Arc::new(store),
                owner: Some(owner),
            });
        }
        Arc::new(Self {
            store: Arc::new(UnavailableCredentialStore),
            owner: None,
        })
    }

    #[cfg(test)]
    pub fn with_store(store: Arc<dyn CredentialStore>) -> Arc<Self> {
        Arc::new(Self { store, owner: None })
    }

    pub async fn get(&self, key: CredentialRef) -> Result<Option<Token>, CredentialIssue> {
        match self.store.get(key.store_key()).await {
            CredentialOutcome::Retrieved(secret) => std::str::from_utf8(secret.expose_secret())
                .map(|token| Some(token.to_owned().into()))
                .map_err(|_| CredentialIssue {
                    summary: "The saved credential is invalid.",
                    silent: false,
                }),
            CredentialOutcome::NotFound => Ok(None),
            outcome => Err(CredentialIssue::from_outcome(&outcome)),
        }
    }

    pub async fn set(&self, key: CredentialRef, token: Token) -> Result<(), CredentialIssue> {
        match self
            .store
            .set(
                key.store_key(),
                Secret::new(token.expose().as_bytes().to_vec()),
            )
            .await
        {
            CredentialOutcome::Stored => Ok(()),
            outcome => Err(CredentialIssue::from_outcome(&outcome)),
        }
    }

    pub async fn delete(&self, key: CredentialRef) -> Result<(), CredentialIssue> {
        match self.store.delete(key.store_key()).await {
            CredentialOutcome::Deleted | CredentialOutcome::NotFound => Ok(()),
            outcome => Err(CredentialIssue::from_outcome(&outcome)),
        }
    }
}
