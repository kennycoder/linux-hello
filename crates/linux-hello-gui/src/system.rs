use std::env;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use linux_hello_core::{validate_username, Storage, UserProfile};

#[derive(Debug, Clone, Default)]
pub struct DaemonStatus {
    pub socket_exists: bool,
    pub daemon_reachable: bool,
    pub systemd_active: bool,
    pub camera_found: bool,
    pub camera_path: String,
    pub enrolled_users: Vec<String>,
}

#[derive(Debug, Clone, Default)]
pub struct PamStatus {
    pub system_wide_config_exists: bool,
    pub common_auth_has_hello: bool,
    pub is_authselect: bool,
    pub is_arch: bool,
    pub sudo_only_pam_exists: bool,
    pub sudo_only_sudoers_exists: bool,
}

impl PamStatus {
    pub fn is_system_wide_enabled(&self) -> bool {
        self.system_wide_config_exists && self.common_auth_has_hello
    }

    pub fn is_sudo_only_enabled(&self) -> bool {
        self.sudo_only_pam_exists && self.sudo_only_sudoers_exists
    }
}

pub fn get_current_username() -> String {
    env::var("SUDO_USER")
        .or_else(|_| env::var("USER"))
        .unwrap_or_else(|_| "user".to_string())
}

pub fn resolve_models_dir() -> PathBuf {
    if Path::new("models/face_detection_yunet.onnx").exists() {
        PathBuf::from("models")
    } else if Path::new("/usr/share/linux-hello/models/face_detection_yunet.onnx").exists() {
        PathBuf::from("/usr/share/linux-hello/models")
    } else {
        PathBuf::from("models")
    }
}

pub fn resolve_storage_dir() -> PathBuf {
    let sys_dir = Path::new("/var/lib/linux-hello/models");
    if sys_dir.exists() {
        return sys_dir.to_path_buf();
    }
    if nix::unistd::Uid::effective().is_root() {
        return sys_dir.to_path_buf();
    }
    sys_dir.to_path_buf()
}

/// Resolves the path to the companion `linux-hello` CLI executable for Polkit elevation.
pub fn resolve_cli_binary() -> PathBuf {
    if let Ok(exe) = env::current_exe() {
        if let Some(parent) = exe.parent() {
            let sibling = parent.join("linux-hello");
            if sibling.exists() {
                return sibling;
            }
        }
    }
    if Path::new("/usr/bin/linux-hello").exists() {
        return PathBuf::from("/usr/bin/linux-hello");
    }
    PathBuf::from("linux-hello")
}

/// Validates a custom template label against a strict allow-list.
pub fn validate_label(label: &str) -> Result<(), String> {
    let trimmed = label.trim();
    if trimmed.is_empty() || trimmed.len() > 32 {
        return Err("Label must be between 1 and 32 characters.".to_string());
    }
    if !trimmed
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return Err("Label may only contain letters, numbers, '_', or '-'.".to_string());
    }
    Ok(())
}

/// Saves a UserProfile to `storage_dir`. If direct write fails with PermissionDenied,
/// writes to a temporary file and elevates privileges via `pkexec linux-hello import-profile`.
pub fn save_profile_privileged(storage_dir: &Path, profile: &UserProfile) -> Result<String, String> {
    validate_username(&profile.username).map_err(|e| e.to_string())?;

    if let Ok(storage) = Storage::new(storage_dir) {
        match storage.save_profile(profile) {
            Ok(()) => {
                return Ok(format!(
                    "Saved profile for '{}' ({} templates) to {:?}",
                    profile.username,
                    profile.enrolled_embeddings.len(),
                    storage.user_path(&profile.username)
                ));
            }
            Err(linux_hello_core::StorageError::Io(ref e))
                if e.kind() == std::io::ErrorKind::PermissionDenied =>
            {
                // Fall through to Polkit elevation
            }
            Err(e) => return Err(format!("Failed to save profile: {}", e)),
        }
    }

    // Elevate via pkexec + linux-hello import-profile
    let mut temp_file = tempfile::NamedTempFile::new()
        .map_err(|e| format!("Failed to create temporary profile file: {}", e))?;
    let json = serde_json::to_string_pretty(profile)
        .map_err(|e| format!("Failed to serialize profile: {}", e))?;
    temp_file
        .write_all(json.as_bytes())
        .map_err(|e| format!("Failed to write temporary profile: {}", e))?;
    let _ = fs::set_permissions(temp_file.path(), fs::Permissions::from_mode(0o644));

    let cli_bin = resolve_cli_binary();
    let output = Command::new("pkexec")
        .arg(&cli_bin)
        .arg("--storage-dir")
        .arg(storage_dir)
        .arg("import-profile")
        .arg("--user")
        .arg(&profile.username)
        .arg("--file")
        .arg(temp_file.path())
        .output()
        .map_err(|e| format!("Failed to launch pkexec: {}", e))?;

    if output.status.success() {
        Ok(format!(
            "Saved profile for '{}' ({} templates) via Polkit",
            profile.username,
            profile.enrolled_embeddings.len()
        ))
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        Err(format!(
            "Polkit authorization or profile import failed: {} {}",
            stdout.trim(),
            stderr.trim()
        ))
    }
}

/// Removes either a single template label or the entire profile for `username`.
pub fn remove_profile_or_label_privileged(
    storage_dir: &Path,
    username: &str,
    label: Option<&str>,
) -> Result<String, String> {
    validate_username(username).map_err(|e| e.to_string())?;
    if let Some(lbl) = label {
        validate_label(lbl)?;
    }

    // Attempt unprivileged first if directory/file is writable by current user
    if let Ok(storage) = Storage::new(storage_dir) {
        if let Some(lbl) = label {
            if let Ok(Some(mut profile)) = storage.load_profile(username) {
                if profile.remove_label(lbl) {
                    let res = if profile.enrolled_embeddings.is_empty() {
                        storage.delete_user(username).map(|_| ())
                    } else {
                        storage.save_profile(&profile)
                    };
                    if res.is_ok() {
                        return Ok(format!("Removed template '{}' for '{}'", lbl, username));
                    }
                }
            }
        } else if let Ok(true) = storage.delete_user(username) {
            return Ok(format!("Deleted biometric profile for '{}'", username));
        }
    }

    let cli_bin = resolve_cli_binary();
    let mut cmd = Command::new("pkexec");
    cmd.arg(&cli_bin)
        .arg("--storage-dir")
        .arg(storage_dir)
        .arg("remove")
        .arg(username);
    if let Some(lbl) = label {
        cmd.arg("--label").arg(lbl);
    }

    let output = cmd
        .output()
        .map_err(|e| format!("Failed to execute pkexec: {}", e))?;

    if output.status.success() {
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        Ok(if stdout.is_empty() {
            "Profile updated successfully.".to_string()
        } else {
            stdout
        })
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        Err(format!("Failed to remove template/profile: {}", stderr.trim()))
    }
}

pub fn query_daemon_status(socket_path: &Path) -> DaemonStatus {
    let socket_exists = socket_path.exists();
    let systemd_active = Command::new("systemctl")
        .args(["is-active", "--quiet", "linux-hello.service"])
        .status()
        .map(|s| s.success())
        .unwrap_or(false);

    let mut status = DaemonStatus {
        socket_exists,
        daemon_reachable: false,
        systemd_active,
        camera_found: false,
        camera_path: String::new(),
        enrolled_users: Vec::new(),
    };

    if !socket_exists {
        return status;
    }

    if let Ok(mut stream) = UnixStream::connect(socket_path) {
        let _ = stream.set_read_timeout(Some(Duration::from_millis(800)));
        let _ = stream.set_write_timeout(Some(Duration::from_millis(800)));
        if stream.write_all(b"{\"action\":\"status\"}\n").is_ok() {
            let mut reader = BufReader::new(stream);
            let mut line = String::new();
            if reader.read_line(&mut line).is_ok() && !line.is_empty() {
                if let Ok(val) = serde_json::from_str::<serde_json::Value>(&line) {
                    if val.get("type").and_then(|t| t.as_str()) == Some("status") {
                        status.daemon_reachable = true;
                        status.camera_found = val
                            .get("camera_found")
                            .and_then(|v| v.as_bool())
                            .unwrap_or(false);
                        status.camera_path = val
                            .get("camera_path")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        if let Some(arr) = val.get("enrolled_users").and_then(|v| v.as_array()) {
                            status.enrolled_users = arr
                                .iter()
                                .filter_map(|u| u.as_str().map(|s| s.to_string()))
                                .collect();
                        }
                    }
                }
            }
        }
    }

    status
}

pub fn run_daemon_auth_test(socket_path: &Path, username: &str) -> Result<(bool, f32, String), String> {
    validate_username(username).map_err(|e| e.to_string())?;
    let mut stream = UnixStream::connect(socket_path)
        .map_err(|e| format!("Could not connect to {:?}: {}", socket_path, e))?;
    let _ = stream.set_read_timeout(Some(Duration::from_millis(5000)));
    let _ = stream.set_write_timeout(Some(Duration::from_millis(1000)));

    let req = format!(
        "{{\"action\":\"authenticate\",\"username\":\"{}\",\"timeout_ms\":3500}}\n",
        username
    );
    stream
        .write_all(req.as_bytes())
        .map_err(|e| format!("Failed to send request to daemon: {}", e))?;

    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader
        .read_line(&mut line)
        .map_err(|e| format!("Failed to read response from daemon: {}", e))?;

    let val: serde_json::Value =
        serde_json::from_str(&line).map_err(|e| format!("Invalid daemon JSON: {}", e))?;
    if val.get("type").and_then(|t| t.as_str()) == Some("error") {
        let msg = val
            .get("message")
            .and_then(|m| m.as_str())
            .unwrap_or("Daemon error");
        return Err(msg.to_string());
    }

    let success = val.get("success").and_then(|v| v.as_bool()).unwrap_or(false);
    let score = val.get("score").and_then(|v| v.as_f64()).unwrap_or(0.0) as f32;
    let message = val
        .get("message")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    Ok((success, score, message))
}

pub fn query_pam_status() -> PamStatus {
    let custom_authselect_dir = Path::new("/etc/authselect/custom/linux-hello");
    let has_authselect = Command::new("which")
        .arg("authselect")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
        || Path::new("/usr/bin/authselect").exists()
        || custom_authselect_dir.exists();

    let has_pam_configs = Path::new("/usr/share/pam-configs/linux-hello").exists();
    let is_arch = Path::new("/etc/arch-release").exists()
        || (Path::new("/etc/pam.d/system-auth").exists() && !has_authselect && !has_pam_configs);

    let (system_wide_config_exists, common_auth_has_hello, is_authselect) = if has_authselect && !has_pam_configs {
        let exists = custom_authselect_dir.exists();
        let in_system_auth = fs::read_to_string("/etc/pam.d/system-auth")
            .map(|c| c.contains("pam_linux_hello.so"))
            .unwrap_or(false);
        (exists, in_system_auth, true)
    } else if is_arch {
        let in_system_auth = fs::read_to_string("/etc/pam.d/system-auth")
            .map(|c| c.contains("pam_linux_hello.so"))
            .unwrap_or(false);
        (in_system_auth, in_system_auth, false)
    } else {
        let debian_config = has_pam_configs;
        let in_common_auth = fs::read_to_string("/etc/pam.d/common-auth")
            .map(|c| c.contains("pam_linux_hello.so"))
            .unwrap_or(false);
        (debian_config, in_common_auth, false)
    };

    let sudo_only_pam_exists = Path::new("/etc/pam.d/sudo-hello").exists();
    let sudo_only_sudoers_exists = Path::new("/etc/sudoers.d/linux-hello").exists()
        || Path::new("/etc/sudoers.d/custom_etc_sudoers_d").exists();

    PamStatus {
        system_wide_config_exists,
        common_auth_has_hello,
        is_authselect,
        is_arch,
        sudo_only_pam_exists,
        sudo_only_sudoers_exists,
    }
}

pub fn set_pam_mode_privileged(enable: bool, sudo_only: bool) -> Result<String, String> {
    let cli_bin = resolve_cli_binary();
    let mut cmd = Command::new("pkexec");
    cmd.arg(&cli_bin);
    if enable {
        cmd.arg("enable");
    } else {
        cmd.arg("disable");
    }
    if sudo_only {
        cmd.arg("--sudo-only");
    }

    let output = cmd
        .output()
        .map_err(|e| format!("Failed to invoke pkexec: {}", e))?;

    if output.status.success() {
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        Ok(if stdout.is_empty() {
            "PAM configuration updated.".to_string()
        } else {
            stdout
        })
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        Err(format!("{} {}", stdout.trim(), stderr.trim()))
    }
}

pub fn control_systemd_service_privileged(action: &str) -> Result<String, String> {
    // Strict allow-list for systemctl actions
    let valid_action = match action {
        "start" | "stop" | "restart" | "enable" => action,
        _ => return Err(format!("Unsupported service action: {}", action)),
    };

    let mut cmd = Command::new("pkexec");
    cmd.arg("systemctl");
    if valid_action == "enable" {
        cmd.args(["enable", "--now", "linux-hello.service"]);
    } else {
        cmd.args([valid_action, "linux-hello.service"]);
    }

    let output = cmd
        .output()
        .map_err(|e| format!("Failed to execute pkexec systemctl: {}", e))?;

    if output.status.success() {
        Ok(format!(
            "Service linux-hello.service '{}' succeeded.",
            valid_action
        ))
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        Err(format!(
            "systemctl {} failed: {}",
            valid_action,
            stderr.trim()
        ))
    }
}

pub fn friendly_pose_name(label: &str) -> &str {
    match label {
        "center" => "Center (Straight Ahead)",
        "look_left" => "Turn Left (15-20°)",
        "look_right" => "Turn Right (15-20°)",
        "look_up" => "Tilt Up (10-15°)",
        "look_down" => "Tilt Down (10-15°)",
        other => other,
    }
}

pub fn average_embeddings(embeddings: &[Vec<f32>]) -> Vec<f32> {
    if embeddings.is_empty() {
        return Vec::new();
    }
    let dim = embeddings[0].len();
    let mut avg = vec![0.0f32; dim];
    for emb in embeddings {
        for (i, &val) in emb.iter().enumerate() {
            avg[i] += val;
        }
    }
    let n = embeddings.len() as f32;
    let mut norm_sq = 0.0f32;
    for val in &mut avg {
        *val /= n;
        norm_sq += *val * *val;
    }
    let norm = norm_sq.sqrt();
    if norm > 1e-6 {
        for val in &mut avg {
            *val /= norm;
        }
    }
    avg
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_validate_label() {
        assert!(validate_label("glasses").is_ok());
        assert!(validate_label("look_left-2").is_ok());
        assert!(validate_label("").is_err());
        assert!(validate_label("bad/label").is_err());
        assert!(validate_label("bad..label").is_err());
        assert!(validate_label("bad label").is_err());
    }

    #[test]
    fn test_average_embeddings_normalized() {
        let e1 = vec![1.0, 0.0, 0.0];
        let e2 = vec![0.0, 1.0, 0.0];
        let avg = average_embeddings(&[e1, e2]);
        let norm: f32 = avg.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-4);
    }
}
