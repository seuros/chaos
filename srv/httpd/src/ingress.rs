//! Shared JSON ingress boundaries. Callers own authentication/admission order
//! and wire responses; body collection always uses the caller's deadline.
use chaos_snitch::tracing::error;
use rama::bytes::Bytes;
use rama::http::body::util::{BodyExt, LengthLimitError};
use rama::http::{Body, Request, StatusCode};
use tokio::time::Instant;

pub(crate) enum BodyError {
    ContentType,
    TooLarge,
    ReadFailed,
    Deadline,
}

impl BodyError {
    pub(crate) fn status(&self) -> StatusCode {
        match self {
            Self::ContentType | Self::ReadFailed => StatusCode::BAD_REQUEST,
            Self::TooLarge => StatusCode::PAYLOAD_TOO_LARGE,
            Self::Deadline => StatusCode::REQUEST_TIMEOUT,
        }
    }

    pub(crate) fn message(&self) -> &'static str {
        match self {
            Self::ContentType => "Content-Type must be application/json",
            Self::TooLarge => "request body too large",
            Self::ReadFailed => "failed to read request body",
            Self::Deadline => "request body deadline exceeded",
        }
    }
}

pub(crate) fn validate_json_headers(request: &Request, limit: usize) -> Result<(), BodyError> {
    let content_type = request
        .headers()
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("");
    // Accept MIME parameters, not JSON-like subtypes.
    if !content_type
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .eq_ignore_ascii_case("application/json")
    {
        return Err(BodyError::ContentType);
    }
    if request
        .headers()
        .get("content-length")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<usize>().ok())
        .is_some_and(|len| len > limit)
    {
        return Err(BodyError::TooLarge);
    }
    Ok(())
}

pub(crate) async fn read_body(
    body: Body,
    limit: usize,
    deadline: Instant,
) -> Result<Bytes, BodyError> {
    // Stop polling on the first overflowing frame, including chunked bodies.
    match tokio::time::timeout_at(deadline, body.limited(limit).collect()).await {
        Ok(Ok(collected)) => Ok(collected.to_bytes()),
        Ok(Err(error))
            if matches!(
                error.kind(),
                rama::http::body::CollectErrorKind::Stream(source)
                    if source.is::<LengthLimitError>()
            ) =>
        {
            Err(BodyError::TooLarge)
        }
        Ok(Err(error)) => {
            error!(%error, "failed to read request body");
            Err(BodyError::ReadFailed)
        }
        Err(_) => Err(BodyError::Deadline),
    }
}
