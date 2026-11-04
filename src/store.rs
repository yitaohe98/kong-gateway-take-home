use std::{collections::HashMap, time::Duration};

use redis::{AsyncCommands, Script, aio::ConnectionManager};
use tokio::time::timeout;

use crate::{
    admission::{
        AdmissionOutcome, AdmissionResponse, FailedDimension, RemainingTokens, TOKEN_SCALE,
    },
    config::{DimensionConfig, RateLimitConfig, VersionedConfig},
    error::ApiError,
};

const CONFIG_KEY: &str = "rl:v1:config";

const INITIALIZE_CONFIG_LUA: &str = r#"
if redis.call('EXISTS', KEYS[1]) == 1 then
  return 0
end

redis.call('HSET', KEYS[1],
  'generation', 1,
  'path_limit', ARGV[1],
  'path_window_seconds', ARGV[2],
  'client_limit', ARGV[3],
  'client_window_seconds', ARGV[4],
  'ip_limit', ARGV[5],
  'ip_window_seconds', ARGV[6])
return 1
"#;

const UPDATE_CONFIG_LUA: &str = r#"
if redis.call('EXISTS', KEYS[1]) == 0 then
  return redis.error_reply('CONFIG_NOT_INITIALIZED')
end

local generation = redis.call('HINCRBY', KEYS[1], 'generation', 1)
redis.call('HSET', KEYS[1],
  'path_limit', ARGV[1],
  'path_window_seconds', ARGV[2],
  'client_limit', ARGV[3],
  'client_window_seconds', ARGV[4],
  'ip_limit', ARGV[5],
  'ip_window_seconds', ARGV[6])
return generation
"#;

const ADMISSION_LUA: &str = r#"
local TOKEN_SCALE = 1000000

local config = redis.call('HMGET', KEYS[1],
  'generation',
  'path_limit', 'path_window_seconds',
  'client_limit', 'client_window_seconds',
  'ip_limit', 'ip_window_seconds')

if not config[1] then
  return {-2}
end

local generation = tonumber(config[1])
local expected_generation = tonumber(ARGV[1])
if generation ~= expected_generation then
  return {-1, generation}
end

local now_parts = redis.call('TIME')
local now_ms = tonumber(now_parts[1]) * 1000 + math.floor(tonumber(now_parts[2]) / 1000)

local function refill(key, limit, window_seconds)
  local capacity = limit * TOKEN_SCALE
  local window_ms = window_seconds * 1000
  local values = redis.call('HMGET', key, 'tokens_micro', 'last_refill_ms', 'refill_remainder')

  local tokens = tonumber(values[1])
  local last_ms = tonumber(values[2])
  local remainder = tonumber(values[3]) or 0

  if not tokens or not last_ms then
    return {capacity, now_ms, 0, window_ms, capacity}
  end

  local effective_now_ms = now_ms
  if effective_now_ms < last_ms then
    effective_now_ms = last_ms
  end
  local elapsed_ms = effective_now_ms - last_ms

  if elapsed_ms >= window_ms then
    return {capacity, effective_now_ms, 0, window_ms, capacity}
  end

  local whole_per_ms = math.floor(capacity / window_ms)
  local rate_remainder = capacity % window_ms
  local fractional = elapsed_ms * rate_remainder + remainder
  local added = elapsed_ms * whole_per_ms + math.floor(fractional / window_ms)
  local next_remainder = fractional % window_ms
  local next_tokens = tokens + added

  if next_tokens >= capacity then
    next_tokens = capacity
    next_remainder = 0
  end

  return {next_tokens, effective_now_ms, next_remainder, window_ms, capacity}
end

local function retry_after_ms(state)
  if state[1] >= TOKEN_SCALE then
    return 0
  end
  local deficit = TOKEN_SCALE - state[1]
  local needed_numerator = deficit * state[4] - state[3]
  if needed_numerator <= 0 then
    return 0
  end
  return math.floor((needed_numerator + state[5] - 1) / state[5])
end

local states = {
  refill(KEYS[2], tonumber(config[2]), tonumber(config[3])),
  refill(KEYS[3], tonumber(config[4]), tonumber(config[5])),
  refill(KEYS[4], tonumber(config[6]), tonumber(config[7]))
}

local allowed = 1
local retries = {0, 0, 0}
for i = 1, 3 do
  if states[i][1] < TOKEN_SCALE then
    allowed = 0
    retries[i] = retry_after_ms(states[i])
  end
end

if allowed == 1 then
  for i = 1, 3 do
    states[i][1] = states[i][1] - TOKEN_SCALE
  end
end

for i = 1, 3 do
  redis.call('HSET', KEYS[i + 1],
    'tokens_micro', states[i][1],
    'last_refill_ms', states[i][2],
    'refill_remainder', states[i][3])
  redis.call('PEXPIRE', KEYS[i + 1], states[i][4] * 2)
end

return {
  allowed,
  generation,
  states[1][1], states[2][1], states[3][1],
  retries[1], retries[2], retries[3]
}
"#;

#[derive(Clone)]
pub struct RedisStore {
    connection: ConnectionManager,
    operation_timeout: Duration,
}

impl RedisStore {
    pub async fn connect(redis_url: &str, operation_timeout: Duration) -> Result<Self, ApiError> {
        let client = redis::Client::open(redis_url)?;
        let connection = timeout(operation_timeout, client.get_connection_manager())
            .await
            .map_err(|_| ApiError::RedisTimeout)??;
        Ok(Self {
            connection,
            operation_timeout,
        })
    }

    pub async fn initialize_defaults(&self) -> Result<bool, ApiError> {
        let defaults = RateLimitConfig::defaults();
        let mut connection = self.connection.clone();
        let initialized: i64 = timeout(
            self.operation_timeout,
            Script::new(INITIALIZE_CONFIG_LUA)
                .key(CONFIG_KEY)
                .arg(defaults.path.limit)
                .arg(defaults.path.window_seconds)
                .arg(defaults.client.limit)
                .arg(defaults.client.window_seconds)
                .arg(defaults.ip.limit)
                .arg(defaults.ip.window_seconds)
                .invoke_async(&mut connection),
        )
        .await
        .map_err(|_| ApiError::RedisTimeout)??;
        Ok(initialized == 1)
    }

    pub async fn ping(&self) -> Result<(), ApiError> {
        let mut connection = self.connection.clone();
        let response: String = timeout(
            self.operation_timeout,
            redis::cmd("PING").query_async(&mut connection),
        )
        .await
        .map_err(|_| ApiError::RedisTimeout)??;
        if response == "PONG" {
            Ok(())
        } else {
            Err(ApiError::InvalidStoredConfig(
                "unexpected Redis PING response".to_owned(),
            ))
        }
    }

    pub async fn get_config(&self) -> Result<VersionedConfig, ApiError> {
        let mut connection = self.connection.clone();
        let query = connection.hgetall(CONFIG_KEY);
        let values: HashMap<String, String> = timeout(self.operation_timeout, query)
            .await
            .map_err(|_| ApiError::RedisTimeout)??;
        if values.is_empty() {
            return Err(ApiError::InvalidStoredConfig(
                "global configuration is missing".to_owned(),
            ));
        }
        parse_config(values)
    }

    pub async fn update_config(
        &self,
        config: &RateLimitConfig,
    ) -> Result<VersionedConfig, ApiError> {
        let mut connection = self.connection.clone();
        let generation: u64 = timeout(
            self.operation_timeout,
            Script::new(UPDATE_CONFIG_LUA)
                .key(CONFIG_KEY)
                .arg(config.path.limit)
                .arg(config.path.window_seconds)
                .arg(config.client.limit)
                .arg(config.client.window_seconds)
                .arg(config.ip.limit)
                .arg(config.ip.window_seconds)
                .invoke_async(&mut connection),
        )
        .await
        .map_err(|_| ApiError::RedisTimeout)??;

        Ok(VersionedConfig {
            generation,
            config: config.clone(),
        })
    }

    pub async fn admit(
        &self,
        generation: u64,
        bucket_keys: &[String; 3],
    ) -> Result<AdmissionOutcome, ApiError> {
        let mut connection = self.connection.clone();
        let values: Vec<i64> = timeout(
            self.operation_timeout,
            Script::new(ADMISSION_LUA)
                .key(CONFIG_KEY)
                .key(&bucket_keys[0])
                .key(&bucket_keys[1])
                .key(&bucket_keys[2])
                .arg(generation)
                .invoke_async(&mut connection),
        )
        .await
        .map_err(|_| ApiError::RedisTimeout)??;

        match values.first().copied() {
            Some(-1) => return Ok(AdmissionOutcome::StaleGeneration),
            Some(-2) => {
                return Err(ApiError::InvalidStoredConfig(
                    "global configuration is missing".to_owned(),
                ));
            }
            Some(0 | 1) if values.len() == 8 => {}
            _ => {
                return Err(ApiError::IndeterminateAdmission(
                    "unexpected Lua response".to_owned(),
                ));
            }
        }

        let allowed = values[0] == 1;
        let response = AdmissionResponse {
            allowed,
            generation: values[1] as u64,
            remaining: RemainingTokens {
                path: (values[2] as u64) / TOKEN_SCALE,
                client: (values[3] as u64) / TOKEN_SCALE,
                ip: (values[4] as u64) / TOKEN_SCALE,
            },
            failed_dimensions: ["path", "client", "ip"]
                .into_iter()
                .zip(values[5..8].iter().copied())
                .filter(|(_, retry_after_ms)| *retry_after_ms > 0)
                .map(|(dimension, retry_after_ms)| FailedDimension {
                    dimension,
                    retry_after_ms: retry_after_ms as u64,
                })
                .collect(),
        };

        if allowed {
            Ok(AdmissionOutcome::Allowed(response))
        } else {
            let retry_after_ms = response
                .failed_dimensions
                .iter()
                .map(|failure| failure.retry_after_ms)
                .max()
                .ok_or_else(|| {
                    ApiError::IndeterminateAdmission(
                        "denied Lua response had no failed dimensions".to_owned(),
                    )
                })?;
            Ok(AdmissionOutcome::Denied {
                response,
                retry_after_ms,
            })
        }
    }
}

fn parse_config(values: HashMap<String, String>) -> Result<VersionedConfig, ApiError> {
    fn field(values: &HashMap<String, String>, name: &str) -> Result<u64, ApiError> {
        values
            .get(name)
            .ok_or_else(|| ApiError::InvalidStoredConfig(format!("missing field {name}")))?
            .parse::<u64>()
            .map_err(|_| ApiError::InvalidStoredConfig(format!("invalid integer field {name}")))
    }

    let config = RateLimitConfig {
        path: DimensionConfig {
            limit: field(&values, "path_limit")?,
            window_seconds: field(&values, "path_window_seconds")?,
        },
        client: DimensionConfig {
            limit: field(&values, "client_limit")?,
            window_seconds: field(&values, "client_window_seconds")?,
        },
        ip: DimensionConfig {
            limit: field(&values, "ip_limit")?,
            window_seconds: field(&values, "ip_window_seconds")?,
        },
    };
    config.validate().map_err(ApiError::InvalidStoredConfig)?;

    Ok(VersionedConfig {
        generation: field(&values, "generation")?,
        config,
    })
}
