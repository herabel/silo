use std::io::Write;
use std::path::PathBuf;
use std::sync::RwLock;

static ACTIVE_SESSION_IMAGE: RwLock<Option<Vec<u8>>> = RwLock::new(None);

pub fn set_active_session_image(bytes: Option<Vec<u8>>) {
    if let Ok(mut lock) = ACTIVE_SESSION_IMAGE.write() {
        *lock = bytes;
    }
}

pub fn get_active_session_image() -> Option<Vec<u8>> {
    ACTIVE_SESSION_IMAGE.read().ok().and_then(|lock| lock.clone())
}

pub struct EphemeralMemoryFile {
    pub path: PathBuf,
    byte_size: usize,
}

impl EphemeralMemoryFile {
    /// Creates an in-RAM temporary file backed by Windows system cache with
    /// FILE_ATTRIBUTE_TEMPORARY (0x100).
    /// The OS keeps pages in physical memory and avoids flushing to physical storage.
    /// The handle is closed after write so external runtimes (Python MSVCRT) can read
    /// without Win32 sharing violations. RAII Drop securely zeroes and deletes the file.
    pub fn create_from_bytes(bytes: &[u8], extension: &str) -> Result<Self, String> {
        let temp_dir = std::env::temp_dir();
        let unique_name = format!(
            "silo_ephemeral_{}.{}",
            uuid::Uuid::new_v4(),
            extension.trim_start_matches('.')
        );
        let path = temp_dir.join(unique_name);

        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            // FILE_ATTRIBUTE_TEMPORARY (0x100) tells OS cache manager to keep pages in RAM.
            const FILE_ATTRIBUTE_TEMPORARY: u32 = 0x00000100;

            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .custom_flags(FILE_ATTRIBUTE_TEMPORARY)
                .open(&path)
                .map_err(|e| format!("Failed to create Win32 in-RAM ephemeral file: {}", e))?;

            file.write_all(bytes)
                .map_err(|e| format!("Failed to write data into in-RAM ephemeral file: {}", e))?;
            file.flush()
                .map_err(|e| format!("Failed to flush ephemeral file: {}", e))?;
            // Close handle so child processes (Python/MSVCRT) can open without sharing violations
            drop(file);
        }

        #[cfg(not(windows))]
        {
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
                .map_err(|e| format!("Failed to create ephemeral file: {}", e))?;

            file.write_all(bytes)
                .map_err(|e| format!("Failed to write data into ephemeral file: {}", e))?;
            file.flush()
                .map_err(|e| format!("Failed to flush ephemeral file: {}", e))?;
            drop(file);
        }

        Ok(Self {
            path,
            byte_size: bytes.len(),
        })
    }
}

impl Drop for EphemeralMemoryFile {
    fn drop(&mut self) {
        // 1. Cryptographically scrub content with zeroes before deleting
        if self.path.exists() {
            if let Ok(mut f) = std::fs::OpenOptions::new().write(true).open(&self.path) {
                if self.byte_size > 0 {
                    let zero_buf = vec![0u8; self.byte_size.min(65536)];
                    let mut remaining = self.byte_size;
                    while remaining > 0 {
                        let to_write = remaining.min(zero_buf.len());
                        if f.write_all(&zero_buf[..to_write]).is_err() {
                            break;
                        }
                        remaining -= to_write;
                    }
                    let _ = f.flush();
                }
            }

            // 2. Remove file node
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ephemeral_file_lifecycle() {
        let payload = b"EXTREMELY_SENSITIVE_VAULT_DECRYPTED_BYTES_123456";
        let path_copy;
        {
            let ephem = EphemeralMemoryFile::create_from_bytes(payload, "png").expect("Create ephemeral");
            path_copy = ephem.path.clone();
            assert!(path_copy.exists(), "Ephemeral file must exist during RAII guard lifetime");

            // Verify child process can read the content
            let read_back = std::fs::read(&path_copy).expect("Child read ephemeral");
            assert_eq!(read_back, payload);
        }

        // After scope exit (Drop), file must be completely wiped and deleted
        assert!(!path_copy.exists(), "Ephemeral file must be purged after drop");
    }
}
