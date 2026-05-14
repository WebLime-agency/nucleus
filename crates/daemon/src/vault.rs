use anyhow::{Result, anyhow, bail};
use argon2::{Algorithm, Argon2, Params, Version};
use chacha20poly1305::{
    KeyInit, XChaCha20Poly1305, XNonce,
    aead::{Aead, Payload},
};
use nucleus_storage::{StateStore, VaultScopeKeyRecord, VaultSecretRecord, VaultStateRecord};
use rand_core::{OsRng, RngCore};
use serde::{Deserialize, Serialize};
use std::time::{SystemTime, UNIX_EPOCH};
use zeroize::Zeroize;

const ROOT_CHECK_PLAINTEXT: &[u8] = b"nucleus-vault-root-check-v1";
const ROOT_CHECK_AAD: &[u8] = b"nucleus:vault:v1:root-check";
#[cfg(not(test))]
const KDF_MEMORY_KIB: u32 = 19_456;
#[cfg(test)]
const KDF_MEMORY_KIB: u32 = 256;
#[cfg(not(test))]
const KDF_TIME_COST: u32 = 2;
#[cfg(test)]
const KDF_TIME_COST: u32 = 1;
const KDF_PARALLELISM: u32 = 1;
const DEFAULT_IDLE_TIMEOUT_SECONDS: i64 = 30 * 60;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaultKdfParams {
    pub memory_kib: u32,
    pub time_cost: u32,
    pub parallelism: u32,
    pub output_len: usize,
}

impl Default for VaultKdfParams {
    fn default() -> Self {
        Self {
            memory_kib: KDF_MEMORY_KIB,
            time_cost: KDF_TIME_COST,
            parallelism: KDF_PARALLELISM,
            output_len: 32,
        }
    }
}

#[derive(Default)]
pub struct VaultRuntime {
    root_key: Option<[u8; 32]>,
    vault_id: String,
    unlocked_at: Option<i64>,
    last_access_at: Option<i64>,
}

impl VaultRuntime {
    pub fn is_unlocked(&mut self) -> bool {
        self.lock_if_idle();
        self.root_key.is_some()
    }
    pub fn lock(&mut self) {
        if let Some(mut key) = self.root_key.take() {
            key.zeroize();
        }
        self.vault_id.clear();
        self.unlocked_at = None;
        self.last_access_at = None;
    }

    pub fn initialize(&mut self, store: &StateStore, passphrase: &str) -> Result<VaultStateRecord> {
        if store.load_vault_state()?.is_some() {
            bail!("vault is already initialized");
        }
        validate_passphrase(passphrase)?;
        let vault_id = uuid::Uuid::new_v4().to_string();
        let mut salt = vec![0u8; 32];
        OsRng.fill_bytes(&mut salt);
        let key = derive_key(passphrase, &salt, &VaultKdfParams::default())?;
        let root_check_nonce = random_nonce();
        let cipher = cipher(&key);
        let encrypted_root_check = cipher
            .encrypt(
                XNonce::from_slice(&root_check_nonce),
                Payload {
                    msg: ROOT_CHECK_PLAINTEXT,
                    aad: ROOT_CHECK_AAD,
                },
            )
            .map_err(|_| anyhow!("failed to encrypt vault root check"))?;
        let record = VaultStateRecord {
            id: "default".to_string(),
            version: 1,
            vault_id: vault_id.clone(),
            status: "locked".to_string(),
            kdf_algorithm: "argon2id".to_string(),
            kdf_params_json: serde_json::to_string(&VaultKdfParams::default())?,
            salt,
            cipher: "xchacha20poly1305".to_string(),
            encrypted_root_check,
            root_check_nonce: root_check_nonce.to_vec(),
            created_at: 0,
            updated_at: 0,
        };
        let saved = store.upsert_vault_state(&record)?;
        self.mark_unlocked(key, vault_id);
        Ok(saved)
    }

    pub fn unlock(&mut self, store: &StateStore, passphrase: &str) -> Result<VaultStateRecord> {
        let state = store
            .load_vault_state()?
            .ok_or_else(|| anyhow!("vault is not initialized"))?;
        let params: VaultKdfParams = serde_json::from_str(&state.kdf_params_json)?;
        let key = derive_key(passphrase, &state.salt, &params)?;
        cipher(&key)
            .decrypt(
                XNonce::from_slice(&state.root_check_nonce),
                Payload {
                    msg: &state.encrypted_root_check,
                    aad: ROOT_CHECK_AAD,
                },
            )
            .map_err(|_| anyhow!("invalid vault passphrase"))?;
        self.mark_unlocked(key, state.vault_id.clone());
        Ok(state)
    }

    pub fn create_or_update_secret(
        &mut self,
        store: &StateStore,
        request: VaultSecretInput,
    ) -> Result<VaultSecretRecord> {
        self.lock_if_idle();
        self.touch();
        let root_key = self
            .root_key
            .as_ref()
            .ok_or_else(|| anyhow!("vault is locked"))?;
        let state = store
            .load_vault_state()?
            .ok_or_else(|| anyhow!("vault is not initialized"))?;
        let scope_key = self.scope_key(
            store,
            &state.vault_id,
            &request.scope_kind,
            &request.scope_id,
            root_key,
        )?;
        let existing = request
            .id
            .as_deref()
            .and_then(|id| store.load_vault_secret(id).ok());
        let id = existing.as_ref().map(|s| s.id.clone()).unwrap_or_else(|| {
            request
                .id
                .unwrap_or_else(|| uuid::Uuid::new_v4().to_string())
        });
        let version = existing.as_ref().map(|s| s.version + 1).unwrap_or(1);
        let nonce = random_nonce();
        let aad = secret_aad(
            &state.vault_id,
            &request.scope_kind,
            &request.scope_id,
            &id,
            &request.name,
            version,
        );
        let scope_plain = decrypt_scope_key(root_key, &scope_key)?;
        let ciphertext = cipher(&scope_plain)
            .encrypt(
                XNonce::from_slice(&nonce),
                Payload {
                    msg: request.secret.as_bytes(),
                    aad: aad.as_bytes(),
                },
            )
            .map_err(|_| anyhow!("failed to encrypt vault secret"))?;
        let record = VaultSecretRecord {
            id,
            scope_key_id: scope_key.id,
            scope_kind: request.scope_kind,
            scope_id: request.scope_id,
            name: request.name,
            description: request.description,
            ciphertext,
            nonce: nonce.to_vec(),
            aad,
            version,
            created_at: existing.as_ref().map(|s| s.created_at).unwrap_or(0),
            updated_at: 0,
            last_used_at: existing.and_then(|s| s.last_used_at),
        };
        store.upsert_vault_secret(&record)
    }

    fn mark_unlocked(&mut self, key: [u8; 32], vault_id: String) {
        let now = now_seconds();
        self.root_key = Some(key);
        self.vault_id = vault_id;
        self.unlocked_at = Some(now);
        self.last_access_at = Some(now);
    }

    fn touch(&mut self) {
        if self.root_key.is_some() {
            self.last_access_at = Some(now_seconds());
        }
    }

    fn lock_if_idle(&mut self) {
        let Some(last_access_at) = self.last_access_at else {
            return;
        };
        if now_seconds().saturating_sub(last_access_at) >= DEFAULT_IDLE_TIMEOUT_SECONDS {
            self.lock();
        }
    }

    fn scope_key(
        &self,
        store: &StateStore,
        vault_id: &str,
        scope_kind: &str,
        scope_id: &str,
        root_key: &[u8; 32],
    ) -> Result<VaultScopeKeyRecord> {
        if let Some(existing) = store.load_vault_scope_key(scope_kind, scope_id)? {
            return Ok(existing);
        }
        let mut plain = [0u8; 32];
        OsRng.fill_bytes(&mut plain);
        let nonce = random_nonce();
        let aad = format!("nucleus:vault:v1:{vault_id}:scope-key:{scope_kind}:{scope_id}:1");
        let encrypted_key = cipher(root_key)
            .encrypt(
                XNonce::from_slice(&nonce),
                Payload {
                    msg: &plain,
                    aad: aad.as_bytes(),
                },
            )
            .map_err(|_| anyhow!("failed to encrypt vault scope key"))?;
        plain.zeroize();
        store.upsert_vault_scope_key(&VaultScopeKeyRecord {
            id: uuid::Uuid::new_v4().to_string(),
            vault_id: vault_id.to_string(),
            scope_kind: scope_kind.to_string(),
            scope_id: scope_id.to_string(),
            encrypted_key,
            nonce: nonce.to_vec(),
            aad,
            key_version: 1,
            created_at: 0,
            rotated_at: None,
        })
    }
}

pub struct VaultSecretInput {
    pub id: Option<String>,
    pub scope_kind: String,
    pub scope_id: String,
    pub name: String,
    pub description: String,
    pub secret: String,
}

#[cfg(test)]
pub fn decrypt_secret_for_test(
    root: &mut VaultRuntime,
    store: &StateStore,
    secret: &VaultSecretRecord,
) -> Result<String> {
    let root_key = root
        .root_key
        .as_ref()
        .ok_or_else(|| anyhow!("vault is locked"))?;
    let scope = store
        .load_vault_scope_key(&secret.scope_kind, &secret.scope_id)?
        .ok_or_else(|| anyhow!("vault scope key was not found"))?;
    let scope_key = decrypt_scope_key(root_key, &scope)?;
    let bytes = cipher(&scope_key)
        .decrypt(
            XNonce::from_slice(&secret.nonce),
            Payload {
                msg: &secret.ciphertext,
                aad: secret.aad.as_bytes(),
            },
        )
        .map_err(|_| anyhow!("failed to decrypt vault secret"))?;
    String::from_utf8(bytes).map_err(Into::into)
}

fn decrypt_scope_key(root_key: &[u8; 32], scope: &VaultScopeKeyRecord) -> Result<[u8; 32]> {
    let bytes = cipher(root_key)
        .decrypt(
            XNonce::from_slice(&scope.nonce),
            Payload {
                msg: &scope.encrypted_key,
                aad: scope.aad.as_bytes(),
            },
        )
        .map_err(|_| anyhow!("failed to decrypt vault scope key"))?;
    let array: [u8; 32] = bytes
        .as_slice()
        .try_into()
        .map_err(|_| anyhow!("invalid scope key length"))?;
    Ok(array)
}

fn validate_passphrase(passphrase: &str) -> Result<()> {
    if passphrase.chars().count() < 8 {
        bail!("vault passphrase must be at least 8 characters");
    }
    Ok(())
}

fn derive_key(passphrase: &str, salt: &[u8], params: &VaultKdfParams) -> Result<[u8; 32]> {
    let params = Params::new(
        params.memory_kib,
        params.time_cost,
        params.parallelism,
        Some(params.output_len),
    )
    .map_err(|error| anyhow!("invalid vault KDF parameters: {error:?}"))?;
    let argon2 = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);
    let mut key = [0u8; 32];
    argon2
        .hash_password_into(passphrase.as_bytes(), salt, &mut key)
        .map_err(|error| anyhow!("vault KDF failed: {error:?}"))?;
    Ok(key)
}

fn cipher(key: &[u8; 32]) -> XChaCha20Poly1305 {
    XChaCha20Poly1305::new(key.into())
}
fn random_nonce() -> [u8; 24] {
    let mut nonce = [0u8; 24];
    OsRng.fill_bytes(&mut nonce);
    nonce
}

fn now_seconds() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or_default()
}

fn secret_aad(
    vault_id: &str,
    scope_kind: &str,
    scope_id: &str,
    secret_id: &str,
    name: &str,
    version: i64,
) -> String {
    format!("nucleus:vault:v1:{vault_id}:{scope_kind}:{scope_id}:{secret_id}:{name}:{version}")
}
