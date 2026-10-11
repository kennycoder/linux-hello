use serde::{Deserialize, Serialize};
use std::fs::{self, File};
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use thiserror::Error;

#[derive(Error, Debug)]
pub enum StorageError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Serialization error: {0}")]
    Serialization(#[from] serde_json::Error),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserProfile {
    pub username: String,
    pub enrolled_embeddings: Vec<Vec<f32>>,
    pub labels: Vec<String>,
    pub updated_at: u64,
}

impl UserProfile {
    pub fn new(username: String) -> Self {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        UserProfile {
            username,
            enrolled_embeddings: Vec::new(),
            labels: Vec::new(),
            updated_at: now,
        }
    }

    pub fn add_embedding(&mut self, embedding: Vec<f32>, label: String) {
        self.enrolled_embeddings.push(embedding);
        self.labels.push(label);
        self.updated_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
    }

    pub fn upsert_embedding(&mut self, embedding: Vec<f32>, label: String) {
        if let Some(pos) = self.labels.iter().position(|l| l == &label) {
            self.enrolled_embeddings[pos] = embedding;
        } else {
            self.enrolled_embeddings.push(embedding);
            self.labels.push(label);
        }
        self.updated_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
    }
}

pub struct Storage {
    base_dir: PathBuf,
}

impl Storage {
    pub fn new(base_dir: impl AsRef<Path>) -> Result<Self, StorageError> {
        let base_dir = base_dir.as_ref().to_path_buf();
        if !base_dir.exists() {
            let _ = fs::create_dir_all(&base_dir);
        }
        // Ensure directory is world-readable/traversable (0755) so users and PAM can read profiles
        let _ = fs::set_permissions(&base_dir, fs::Permissions::from_mode(0o755));

        Ok(Storage { base_dir })
    }

    pub fn user_path(&self, username: &str) -> PathBuf {
        self.base_dir.join(format!("{}.json", username))
    }

    pub fn save_profile(&self, profile: &UserProfile) -> Result<(), StorageError> {
        if !self.base_dir.exists() {
            fs::create_dir_all(&self.base_dir)?;
            let _ = fs::set_permissions(&self.base_dir, fs::Permissions::from_mode(0o755));
        }
        let path = self.user_path(&profile.username);
        let data = serde_json::to_string_pretty(profile)?;

        let mut file = File::create(&path)?;
        file.write_all(data.as_bytes())?;
        // Set mode 0644 so users and PAM can read the biometric profile
        let _ = fs::set_permissions(&path, fs::Permissions::from_mode(0o644));

        Ok(())
    }

    pub fn load_profile(&self, username: &str) -> Result<Option<UserProfile>, StorageError> {
        let path = self.user_path(username);
        let content = match fs::read_to_string(&path) {
            Ok(c) => c,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(StorageError::Io(e)),
        };

        let profile: UserProfile = serde_json::from_str(&content)?;
        Ok(Some(profile))
    }

    pub fn list_users(&self) -> Result<Vec<String>, StorageError> {
        let mut users = Vec::new();
        if !self.base_dir.exists() {
            return Ok(users);
        }

        for entry in fs::read_dir(&self.base_dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().map_or(false, |ext| ext == "json") {
                if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                    users.push(stem.to_string());
                }
            }
        }

        users.sort();
        Ok(users)
    }

    pub fn delete_user(&self, username: &str) -> Result<bool, StorageError> {
        let path = self.user_path(username);
        match fs::remove_file(path) {
            Ok(_) => Ok(true),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(StorageError::Io(e)),
        }
    }
}
