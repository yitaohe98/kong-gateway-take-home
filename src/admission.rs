use std::net::IpAddr;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const TOKEN_SCALE: u64 = 1_000_000;

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdmissionRequest {
    pub path: String,
    pub client_id: String,
    pub source_ip: String,
    pub request_id: Option<String>,
}

impl AdmissionRequest {
    pub fn validate_and_normalize(mut self) -> Result<Self, String> {
        validate_non_empty_bounded("path", &self.path, 512)?;
        validate_non_empty_bounded("client_id", &self.client_id, 256)?;
        validate_non_empty_bounded("source_ip", &self.source_ip, 64)?;
        if let Some(request_id) = &self.request_id {
            if request_id.len() > 128 {
                return Err("request_id must not exceed 128 bytes".to_owned());
            }
        }

        let ip: IpAddr = self
            .source_ip
            .parse()
            .map_err(|_| "source_ip must be a valid IPv4 or IPv6 address".to_owned())?;
        self.source_ip = ip.to_string();
        Ok(self)
    }

    pub fn bucket_keys(&self, generation: u64) -> [String; 3] {
        [
            format!(
                "rl:v1:g:{generation}:bucket:path:{}",
                sha256_hex(&self.path)
            ),
            format!(
                "rl:v1:g:{generation}:bucket:client:{}",
                sha256_hex(&self.client_id)
            ),
            format!(
                "rl:v1:g:{generation}:bucket:ip:{}",
                sha256_hex(&self.source_ip)
            ),
        ]
    }
}

fn validate_non_empty_bounded(name: &str, value: &str, max_bytes: usize) -> Result<(), String> {
    if value.is_empty() {
        return Err(format!("{name} must not be empty"));
    }
    if value.len() > max_bytes {
        return Err(format!("{name} must not exceed {max_bytes} bytes"));
    }
    Ok(())
}

fn sha256_hex(value: &str) -> String {
    format!("{:x}", Sha256::digest(value.as_bytes()))
}

#[derive(Debug, Clone, Serialize)]
pub struct RemainingTokens {
    pub path: u64,
    pub client: u64,
    pub ip: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct FailedDimension {
    pub dimension: &'static str,
    pub retry_after_ms: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct AdmissionResponse {
    pub allowed: bool,
    pub generation: u64,
    pub remaining: RemainingTokens,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub failed_dimensions: Vec<FailedDimension>,
}

#[derive(Debug)]
pub enum AdmissionOutcome {
    Allowed(AdmissionResponse),
    Denied {
        response: AdmissionResponse,
        retry_after_ms: u64,
    },
    StaleGeneration,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonicalizes_ipv6_before_key_derivation() {
        let request = AdmissionRequest {
            path: "/payments/{id}".to_owned(),
            client_id: "merchant-123".to_owned(),
            source_ip: "2001:0db8:0:0:0:0:0:1".to_owned(),
            request_id: None,
        }
        .validate_and_normalize()
        .unwrap();

        assert_eq!(request.source_ip, "2001:db8::1");
        assert_eq!(request.bucket_keys(7), request.bucket_keys(7));
    }

    #[test]
    fn rejects_empty_and_malformed_identity_fields() {
        let request = AdmissionRequest {
            path: String::new(),
            client_id: "client".to_owned(),
            source_ip: "not-an-ip".to_owned(),
            request_id: None,
        };
        assert!(request.validate_and_normalize().is_err());
    }

    #[test]
    fn bucket_keys_are_generation_scoped_and_hide_raw_identity() {
        let request = AdmissionRequest {
            path: "/secret/path".to_owned(),
            client_id: "sensitive-client".to_owned(),
            source_ip: "203.0.113.10".to_owned(),
            request_id: None,
        }
        .validate_and_normalize()
        .unwrap();

        let generation_1 = request.bucket_keys(1);
        let generation_2 = request.bucket_keys(2);
        assert_ne!(generation_1, generation_2);
        assert!(generation_1.iter().all(|key| key.contains("rl:v1:g:1:")));
        assert!(generation_1.iter().all(|key| !key.contains("secret")));
        assert!(generation_1.iter().all(|key| !key.contains("sensitive")));
        assert!(generation_1.iter().all(|key| !key.contains("203.0.113.10")));
    }

    #[test]
    fn rejects_unknown_admission_fields() {
        let json = r#"{
            "path":"/x",
            "client_id":"client",
            "source_ip":"203.0.113.10",
            "cost":2
        }"#;
        assert!(serde_json::from_str::<AdmissionRequest>(json).is_err());
    }
}
