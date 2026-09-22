use std::time::{Duration, SystemTime};

pub(crate) fn parse_retry_after(value: Option<&reqwest::header::HeaderValue>) -> Option<Duration> {
    let raw = value?.to_str().ok()?.trim();
    if let Ok(seconds) = raw.parse::<u64>() {
        return Some(Duration::from_secs(seconds));
    }
    let date = httpdate::parse_http_date(raw).ok()?;
    date.duration_since(SystemTime::now()).ok()
}
