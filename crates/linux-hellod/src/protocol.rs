use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "action")]
pub enum Request {
    #[serde(rename = "authenticate")]
    Authenticate {
        username: String,
        #[serde(default = "default_timeout")]
        timeout_ms: u64,
    },
    #[serde(rename = "enroll")]
    Enroll {
        username: String,
        label: String,
    },
    #[serde(rename = "status")]
    Status,
}

fn default_timeout() -> u64 {
    3500
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum Response {
    #[serde(rename = "auth")]
    AuthResult {
        success: bool,
        score: f32,
        message: String,
    },
    #[serde(rename = "enroll")]
    EnrollResult {
        success: bool,
        score: f32,
        total_enrolled: usize,
        message: String,
    },
    #[serde(rename = "status")]
    StatusResult {
        ready: bool,
        camera_found: bool,
        camera_path: String,
        enrolled_users: Vec<String>,
    },
    #[serde(rename = "error")]
    Error {
        message: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_auth_request_serde() {
        let json = r#"{"action":"authenticate","username":"testuser","timeout_ms":3000}"#;
        let req: Request = serde_json::from_str(json).unwrap();
        match req {
            Request::Authenticate { username, timeout_ms } => {
                assert_eq!(username, "testuser");
                assert_eq!(timeout_ms, 3000);
            }
            _ => panic!("Expected Authenticate variant"),
        }
    }

    #[test]
    fn test_auth_response_serde() {
        let resp = Response::AuthResult {
            success: true,
            score: 0.85,
            message: "Success".into(),
        };
        let s = serde_json::to_string(&resp).unwrap();
        assert!(s.contains("\"type\":\"auth\""));
        assert!(s.contains("\"success\":true"));
    }
}
