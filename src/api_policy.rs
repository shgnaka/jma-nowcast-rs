//! Optional REST admission controls, required by the production configuration.

use axum::{
    extract::{Request, State},
    http::{header, HeaderValue, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};
use std::{
    env,
    num::{NonZeroU32, NonZeroUsize},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use subtle::ConstantTimeEq;
use tokio::sync::Semaphore;

use crate::api::ApiError;

pub struct ApiPolicy {
    api_key: Option<String>,
    rate: Option<Arc<RateLimit>>,
    concurrency: Option<Arc<Semaphore>>,
}

const RATE_WINDOW: Duration = Duration::from_secs(60);

struct RateLimit {
    maximum: u32,
    window: Mutex<(Instant, u32)>,
}

impl RateLimit {
    fn admit(&self) -> Result<(), u64> {
        let mut window = self.window.lock().expect("rate limit mutex poisoned");
        let elapsed = window.0.elapsed();
        if elapsed >= RATE_WINDOW {
            *window = (Instant::now(), 0);
        }
        if window.1 >= self.maximum {
            return Err((RATE_WINDOW - elapsed).as_secs() + 1);
        }
        window.1 += 1;
        Ok(())
    }
}

fn setting(name: &str) -> Result<Option<String>, String> {
    match env::var(name) {
        Ok(value) => Ok(Some(value)),
        Err(env::VarError::NotPresent) => Ok(None),
        Err(env::VarError::NotUnicode(_)) => Err(format!("Invalid {name}")),
    }
}

fn limit<T: std::str::FromStr>(name: &str, default: Option<T>) -> Result<Option<T>, String> {
    match setting(name)? {
        Some(value) => value
            .parse()
            .map(Some)
            .map_err(|_| format!("Invalid {name}")),
        None => Ok(default),
    }
}

impl ApiPolicy {
    pub fn from_env(production: bool) -> Result<Arc<Self>, String> {
        let api_key = setting("JMA_API_KEY")?;
        if api_key
            .as_ref()
            .is_some_and(|key| key.is_empty() || !key.bytes().all(|byte| byte.is_ascii_graphic()))
        {
            return Err("Invalid JMA_API_KEY".into());
        }
        if production && api_key.is_none() {
            return Err("JMA_API_KEY is required in production".into());
        }

        let rate = limit(
            "JMA_RATE_LIMIT_PER_MINUTE",
            production.then(|| NonZeroU32::new(60).unwrap()),
        )?;
        let concurrency = limit(
            "JMA_MAX_CONCURRENT_REQUESTS",
            production.then(|| NonZeroUsize::new(4).unwrap()),
        )?;
        if concurrency.is_some_and(|value| value.get() > Semaphore::MAX_PERMITS) {
            return Err("Invalid JMA_MAX_CONCURRENT_REQUESTS".into());
        }

        Ok(Arc::new(Self {
            api_key,
            rate: rate.map(|value| {
                Arc::new(RateLimit {
                    maximum: value.get(),
                    window: Mutex::new((Instant::now(), 0)),
                })
            }),
            concurrency: concurrency.map(|value| Arc::new(Semaphore::new(value.get()))),
        }))
    }

    pub fn mcp_from_env(rest: &Arc<Self>) -> Result<Option<Arc<Self>>, String> {
        let key = setting("JMA_MCP_API_KEY")?.filter(|key| !key.is_empty());
        let anonymous = match setting("JMA_MCP_ALLOW_ANONYMOUS")?.as_deref() {
            None | Some("false") => false,
            Some("true") => true,
            _ => return Err("JMA_MCP_ALLOW_ANONYMOUS must be true or false".into()),
        };
        if key.is_none() && !anonymous {
            return Ok(None);
        }
        if key
            .as_ref()
            .is_some_and(|key| key.is_empty() || !key.bytes().all(|b| b.is_ascii_graphic()))
        {
            return Err("Invalid JMA_MCP_API_KEY".into());
        }
        if key.is_some() && anonymous {
            return Err("Set either JMA_MCP_API_KEY or JMA_MCP_ALLOW_ANONYMOUS".into());
        }
        Ok(Some(Arc::new(Self {
            api_key: key,
            rate: rest.rate.clone(),
            concurrency: rest.concurrency.clone(),
        })))
    }

    fn authorized(&self, request: &Request) -> bool {
        let Some(expected) = &self.api_key else {
            return true;
        };
        let mut values = request.headers().get_all(header::AUTHORIZATION).iter();
        let Some((scheme, key)) = values
            .next()
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.split_once(' '))
        else {
            return false;
        };
        values.next().is_none()
            && scheme.eq_ignore_ascii_case("Bearer")
            && bool::from(key.as_bytes().ct_eq(expected.as_bytes()))
    }
}

pub async fn admit(State(policy): State<Arc<ApiPolicy>>, request: Request, next: Next) -> Response {
    if !policy.authorized(&request) {
        let mut response = ApiError(
            "valid bearer token required".into(),
            "unauthorized",
            StatusCode::UNAUTHORIZED,
        )
        .into_response();
        response
            .headers_mut()
            .insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
        return response;
    }

    let _permit = if let Some(concurrency) = &policy.concurrency {
        match concurrency.try_acquire() {
            Ok(permit) => Some(permit),
            Err(_) => return limited("too many concurrent requests", "concurrency_limit", 1),
        }
    } else {
        None
    };

    if let Some(rate) = &policy.rate {
        if let Err(retry_after) = rate.admit() {
            return limited("request rate exceeded", "rate_limit", retry_after);
        }
    }

    next.run(request).await
}

fn limited(message: &str, code: &'static str, retry_after: u64) -> Response {
    let mut response =
        ApiError(message.into(), code, StatusCode::TOO_MANY_REQUESTS).into_response();
    response
        .headers_mut()
        .insert(header::RETRY_AFTER, HeaderValue::from(retry_after));
    response
}
