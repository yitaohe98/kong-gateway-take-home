use serde::{Deserialize, Serialize};

pub const MIN_LIMIT: u64 = 1;
pub const MAX_LIMIT: u64 = 1_000_000;
pub const MIN_WINDOW_SECONDS: u64 = 1;
pub const MAX_WINDOW_SECONDS: u64 = 86_400;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DimensionConfig {
    pub limit: u64,
    pub window_seconds: u64,
}

impl DimensionConfig {
    fn validate(&self, dimension: &str) -> Result<(), String> {
        if !(MIN_LIMIT..=MAX_LIMIT).contains(&self.limit) {
            return Err(format!(
                "{dimension}.limit must be between {MIN_LIMIT} and {MAX_LIMIT}"
            ));
        }
        if !(MIN_WINDOW_SECONDS..=MAX_WINDOW_SECONDS).contains(&self.window_seconds) {
            return Err(format!(
                "{dimension}.window_seconds must be between {MIN_WINDOW_SECONDS} and {MAX_WINDOW_SECONDS}"
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RateLimitConfig {
    pub path: DimensionConfig,
    pub client: DimensionConfig,
    pub ip: DimensionConfig,
}

impl RateLimitConfig {
    pub fn defaults() -> Self {
        Self {
            path: DimensionConfig {
                limit: 100,
                window_seconds: 60,
            },
            client: DimensionConfig {
                limit: 50,
                window_seconds: 60,
            },
            ip: DimensionConfig {
                limit: 20,
                window_seconds: 10,
            },
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        self.path.validate("path")?;
        self.client.validate("client")?;
        self.ip.validate("ip")?;
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct VersionedConfig {
    pub generation: u64,
    #[serde(flatten)]
    pub config: RateLimitConfig,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_valid() {
        assert_eq!(RateLimitConfig::defaults().validate(), Ok(()));
    }

    #[test]
    fn rejects_out_of_range_values() {
        let mut config = RateLimitConfig::defaults();
        config.ip.limit = 0;
        assert!(config.validate().unwrap_err().contains("ip.limit"));

        config.ip.limit = 1;
        config.path.window_seconds = MAX_WINDOW_SECONDS + 1;
        assert!(
            config
                .validate()
                .unwrap_err()
                .contains("path.window_seconds")
        );
    }

    #[test]
    fn rejects_unknown_json_fields() {
        let json = r#"{
            "path":{"limit":100,"window_seconds":60,"enabled":true},
            "client":{"limit":50,"window_seconds":60},
            "ip":{"limit":20,"window_seconds":10}
        }"#;
        assert!(serde_json::from_str::<RateLimitConfig>(json).is_err());
    }

    #[test]
    fn accepts_exact_numeric_boundaries() {
        let config = RateLimitConfig {
            path: DimensionConfig {
                limit: MIN_LIMIT,
                window_seconds: MIN_WINDOW_SECONDS,
            },
            client: DimensionConfig {
                limit: MAX_LIMIT,
                window_seconds: MAX_WINDOW_SECONDS,
            },
            ip: DimensionConfig {
                limit: 1,
                window_seconds: 1,
            },
        };
        assert_eq!(config.validate(), Ok(()));
    }

    #[test]
    fn rejects_fractional_and_string_numbers() {
        let fractional = r#"{
            "path":{"limit":1.5,"window_seconds":60},
            "client":{"limit":50,"window_seconds":60},
            "ip":{"limit":20,"window_seconds":10}
        }"#;
        let string_number = r#"{
            "path":{"limit":"100","window_seconds":60},
            "client":{"limit":50,"window_seconds":60},
            "ip":{"limit":20,"window_seconds":10}
        }"#;
        assert!(serde_json::from_str::<RateLimitConfig>(fractional).is_err());
        assert!(serde_json::from_str::<RateLimitConfig>(string_number).is_err());
    }
}
