use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use chacha20poly1305::{
    aead::{Aead, KeyInit},
    ChaCha20Poly1305, Key, Nonce,
};
use shake_entropy::HardwareEntropyPool;
use zeroize::Zeroize;

const MAGIC_HEADER: &[u8; 5] = b"SILO\x01";
const AUTO_KEY_MAGIC: &[u8; 12] = b"SILO_AUTO_V1";
const PASS_KEY_MAGIC: &[u8; 12] = b"SILO_PASS_V1";

#[derive(Clone)]
pub struct VaultKey {
    key_bytes: [u8; 32],
    entropy_pool: Arc<Mutex<HardwareEntropyPool>>,
}

impl Drop for VaultKey {
    fn drop(&mut self) {
        self.key_bytes.zeroize();
    }
}

impl VaultKey {
    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        Self {
            key_bytes: bytes,
            entropy_pool: Arc::new(Mutex::new(HardwareEntropyPool::new())),
        }
    }

    pub fn generate_random() -> Self {
        let mut pool = HardwareEntropyPool::new();
        let mut key = [0u8; 32];
        pool.fill_bytes(&mut key);
        Self {
            key_bytes: key,
            entropy_pool: Arc::new(Mutex::new(pool)),
        }
    }

    pub fn encrypt(&self, data: &[u8]) -> Result<Vec<u8>, String> {
        let mut nonce_bytes = [0u8; 12];
        {
            let mut pool = self.entropy_pool.lock().map_err(|e| format!("Entropy pool lock error: {}", e))?;
            pool.fill_bytes(&mut nonce_bytes);
        }

        let cipher = ChaCha20Poly1305::new(Key::from_slice(&self.key_bytes));
        let nonce = Nonce::from_slice(&nonce_bytes);

        let ciphertext = cipher
            .encrypt(nonce, data)
            .map_err(|e| format!("Vault encryption error: {}", e))?;

        let mut out = Vec::with_capacity(MAGIC_HEADER.len() + 12 + ciphertext.len());
        out.extend_from_slice(MAGIC_HEADER);
        out.extend_from_slice(&nonce_bytes);
        out.extend_from_slice(&ciphertext);
        Ok(out)
    }

    pub fn decrypt(&self, blob: &[u8]) -> Result<Vec<u8>, String> {
        if blob.len() < MAGIC_HEADER.len() + 12 + 16 {
            return Err("Encrypted blob is too small to be valid Silo vault file".to_string());
        }
        if &blob[..MAGIC_HEADER.len()] != MAGIC_HEADER {
            return Err("Invalid Silo vault file magic header".to_string());
        }

        let nonce_start = MAGIC_HEADER.len();
        let nonce_end = nonce_start + 12;
        let nonce = Nonce::from_slice(&blob[nonce_start..nonce_end]);
        let ciphertext = &blob[nonce_end..];

        let cipher = ChaCha20Poly1305::new(Key::from_slice(&self.key_bytes));
        cipher
            .decrypt(nonce, ciphertext)
            .map_err(|_| "Decryption failed: corrupted data or incorrect master key".to_string())
    }
}

pub struct VaultManager {
    key_file_path: PathBuf,
}

impl VaultManager {
    pub fn new() -> Self {
        let key_dir = dirs_key_dir();
        let _ = fs::create_dir_all(&key_dir);
        Self {
            key_file_path: key_dir.join("silo.key"),
        }
    }

    pub fn is_password_protected(&self) -> bool {
        if let Ok(bytes) = fs::read(&self.key_file_path) {
            if bytes.len() >= 12 && &bytes[..12] == PASS_KEY_MAGIC {
                return true;
            }
        }
        false
    }

    pub fn key_exists(&self) -> bool {
        self.key_file_path.exists()
    }

    /// Load or initialize automatic local key (Machine Key mode)
    pub fn get_or_create_auto_key(&self) -> Result<VaultKey, String> {
        if self.key_file_path.exists() {
            let bytes = fs::read(&self.key_file_path)
                .map_err(|e| format!("Failed to read key file: {}", e))?;
            
            if bytes.len() >= 12 && &bytes[..12] == PASS_KEY_MAGIC {
                return Err("Vault is protected by Master Password. Password required.".to_string());
            }

            if bytes.len() >= 12 + 32 && &bytes[..12] == AUTO_KEY_MAGIC {
                let mut key = [0u8; 32];
                key.copy_from_slice(&bytes[12..12 + 32]);
                return Ok(VaultKey::from_bytes(key));
            }
        }

        // Generate fresh automatic key using shake-entropy
        let vault_key = VaultKey::generate_random();
        let mut file_content = Vec::with_capacity(12 + 32);
        file_content.extend_from_slice(AUTO_KEY_MAGIC);
        file_content.extend_from_slice(&vault_key.key_bytes);

        fs::write(&self.key_file_path, &file_content)
            .map_err(|e| format!("Failed to write key file: {}", e))?;

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(&self.key_file_path, fs::Permissions::from_mode(0o600));
        }

        Ok(vault_key)
    }

    /// Unlock vault with Master Password
    pub fn unlock_with_password(&self, password: &str) -> Result<VaultKey, String> {
        let bytes = fs::read(&self.key_file_path)
            .map_err(|e| format!("Failed to read key file: {}", e))?;

        if bytes.len() < 12 + 32 + 12 + 48 || &bytes[..12] != PASS_KEY_MAGIC {
            return Err("Invalid or corrupted password-protected key file".to_string());
        }

        let salt = &bytes[12..12 + 32];
        let nonce = Nonce::from_slice(&bytes[12 + 32..12 + 32 + 12]);
        let enc_key = &bytes[12 + 32 + 12..];

        // Derive KEK via Argon2id
        let kek = derive_argon2_key(password, salt)?;
        let cipher = ChaCha20Poly1305::new(Key::from_slice(&kek));

        let master_key_bytes = cipher
            .decrypt(nonce, enc_key)
            .map_err(|_| "Incorrect master password".to_string())?;

        if master_key_bytes.len() != 32 {
            return Err("Decrypted key has invalid length".to_string());
        }

        let mut key = [0u8; 32];
        key.copy_from_slice(&master_key_bytes);
        Ok(VaultKey::from_bytes(key))
    }

    /// Convert vault to use Master Password
    pub fn set_master_password(&self, current_key: &VaultKey, password: &str) -> Result<(), String> {
        let mut salt = [0u8; 32];
        let mut nonce_bytes = [0u8; 12];
        {
            let mut pool = current_key.entropy_pool.lock().map_err(|e| format!("Lock error: {}", e))?;
            pool.fill_bytes(&mut salt);
            pool.fill_bytes(&mut nonce_bytes);
        }

        let kek = derive_argon2_key(password, &salt)?;
        let cipher = ChaCha20Poly1305::new(Key::from_slice(&kek));
        let nonce = Nonce::from_slice(&nonce_bytes);

        let enc_master_key = cipher
            .encrypt(nonce, current_key.key_bytes.as_ref())
            .map_err(|e| format!("Failed to encrypt master key: {}", e))?;

        let mut file_content = Vec::with_capacity(12 + 32 + 12 + enc_master_key.len());
        file_content.extend_from_slice(PASS_KEY_MAGIC);
        file_content.extend_from_slice(&salt);
        file_content.extend_from_slice(&nonce_bytes);
        file_content.extend_from_slice(&enc_master_key);

        fs::write(&self.key_file_path, &file_content)
            .map_err(|e| format!("Failed to write key file: {}", e))?;

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(&self.key_file_path, fs::Permissions::from_mode(0o600));
        }

        Ok(())
    }

    /// Remove Master Password, reverting to local machine key
    pub fn remove_master_password(&self, current_key: &VaultKey) -> Result<(), String> {
        let mut file_content = Vec::with_capacity(12 + 32);
        file_content.extend_from_slice(AUTO_KEY_MAGIC);
        file_content.extend_from_slice(&current_key.key_bytes);

        fs::write(&self.key_file_path, &file_content)
            .map_err(|e| format!("Failed to write key file: {}", e))?;

        Ok(())
    }
}

fn dirs_key_dir() -> PathBuf {
    if let Ok(home) = std::env::var("USERPROFILE").or_else(|_| std::env::var("HOME")) {
        let p = PathBuf::from(home).join(".silo");
        return p;
    }
    PathBuf::from(".silo")
}

fn derive_argon2_key(password: &str, salt: &[u8]) -> Result<[u8; 32], String> {
    use argon2::{Algorithm, Argon2, Params, Version};
    let params = Params::new(65536, 3, 1, Some(32))
        .map_err(|e| format!("Argon2 params error: {}", e))?;
    let argon2 = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);

    let mut out = [0u8; 32];
    argon2
        .hash_password_into(password.as_bytes(), salt, &mut out)
        .map_err(|e| format!("Argon2 key derivation error: {}", e))?;

    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_vault_roundtrip() {
        let key = VaultKey::generate_random();
        let sample = b"Secret thoughts of Silo AI Agent";
        let encrypted = key.encrypt(sample).expect("encrypt ok");
        assert_ne!(&encrypted[..], sample);
        assert_eq!(&encrypted[..5], MAGIC_HEADER);

        let decrypted = key.decrypt(&encrypted).expect("decrypt ok");
        assert_eq!(decrypted, sample);
    }
}
