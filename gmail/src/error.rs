//! Typed local failures and the redacted Sloper error boundary.

use std::{
    fmt,
    io,
    num::ParseIntError,
};

use google_gmail1::{
    common,
    hyper::Error as BodyError,
    hyper_util::client::legacy::Error as TransportError,
};
use sloper_extension::Error as ExtensionError;
use thiserror::Error;
use time::OffsetDateTime;

/// Failures encountered while adapting a Gmail operation to Sloper.
#[derive(Error)]
#[non_exhaustive]
pub enum Error {
    /// A host capability failed.
    #[error("Sloper capability failed")]
    Extension(#[from] ExtensionError),
    /// An integer query parameter is malformed.
    #[error("invalid Gmail integer parameter")]
    Integer(#[from] ParseIntError),
    /// An authored parameter cannot be represented by this Gmail operation.
    #[error("{0}")]
    Invalid(&'static str),
    /// The generated client failed to complete the operation.
    #[error("Gmail request failed")]
    Google(#[source] Box<common::Error>),
    /// Input does not match the generated request type.
    #[error("invalid Gmail request body")]
    Request(#[source] serde_json::Error),
    /// A provider response is not valid JSON.
    #[error("invalid Gmail response")]
    Response(#[source] serde_json::Error),
    /// Serializing generated request data failed.
    #[error("Gmail request serialization failed")]
    Serialize(#[from] serde_json::Error),
    /// A source stream failed.
    #[error("source stream failed")]
    Io(#[from] io::Error),
    /// A MIME content type could not be parsed.
    #[error("invalid MIME content type")]
    Mime(#[from] mime::FromStrError),
    /// A direct request could not be constructed.
    #[error("Gmail request construction failed")]
    Http(#[from] http::Error),
    /// The HTTP client could not complete an exchange.
    #[error("Gmail connection failed")]
    Transport(#[from] TransportError),
    /// A response stream failed.
    #[error("Gmail response stream failed")]
    Body(#[from] BodyError),
    /// A declared source or response size limit was exceeded.
    #[error("Gmail data exceeds the declared size limit")]
    TooLarge,
    /// Gmail temporarily limited the account request rate.
    #[error("Gmail rate limit exceeded")]
    RateLimited {
        /// Provider retry time when present.
        not_before: Option<OffsetDateTime>,
    },
    /// Gmail returned a non-success status.
    #[error("Gmail refused the request")]
    Status {
        /// Provider HTTP status.
        status: http::StatusCode,
        /// Provider's earliest retry time.
        not_before: Option<OffsetDateTime>,
    },
}

impl fmt::Debug for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Error")
            .field("message", &format_args!("{self}"))
            .finish_non_exhaustive()
    }
}

impl Error {
    pub(super) const fn invalid(message: &'static str) -> Self {
        Self::Invalid(message)
    }

    pub(super) fn request(source: serde_json::Error) -> Self {
        Self::Request(source)
    }

    pub(super) fn response(source: serde_json::Error) -> Self {
        Self::Response(source)
    }

    pub(super) const fn too_large() -> Self {
        Self::TooLarge
    }

    pub(super) fn google(source: common::Error) -> Self {
        Self::Google(Box::new(source))
    }

    pub(super) const fn rate_limited(not_before: Option<OffsetDateTime>) -> Self {
        Self::RateLimited {
            not_before,
        }
    }

    pub(super) const fn status(status: http::StatusCode, not_before: Option<OffsetDateTime>) -> Self {
        Self::Status {
            status,
            not_before,
        }
    }
}

impl From<Error> for ExtensionError {
    fn from(error: Error) -> Self {
        // Provider diagnostics may include message contents or credentials. The
        // guest boundary deliberately exposes only authored static diagnostics.
        match error {
            Error::Extension(error) => error,
            Error::Invalid(message) => Self::invalid_parameters(message),
            Error::Integer(_) => Self::invalid_parameters("Gmail history IDs must be unsigned decimal strings."),
            Error::Request(_) => Self::invalid_parameters("Body does not match the generated Gmail request type."),
            Error::TooLarge => Self::TooLarge,
            Error::RateLimited {
                not_before,
            } => Self::unavailable("Gmail temporarily limited this account. Retry later.", not_before),
            Error::Io(error) => Self::from(error),
            Error::Status {
                status,
                not_before,
            } => {
                match status.as_u16() {
                    401 => Self::not_connected("Reconnect the Gmail account."),
                    408 | 429 | 500..=599 => Self::unavailable("Gmail is temporarily unavailable.", not_before),
                    _ => Self::rejected("Gmail refused the operation; check the account permissions and request."),
                }
            },
            Error::Google(error) => {
                match *error {
                    common::Error::MissingAPIKey | common::Error::MissingToken(_) => {
                        Self::not_connected("Reconnect the Gmail account.")
                    },
                    common::Error::UploadSizeLimitExceeded(_, _) => Self::TooLarge,
                    common::Error::Cancelled => Self::Stopped,
                    common::Error::FieldClash(_) => Self::internal("Generated Gmail query parameters conflict."),
                    common::Error::BadRequest(_) | common::Error::Failure(_) => {
                        Self::rejected("Gmail refused the operation.")
                    },
                    common::Error::HttpError(_) | common::Error::Io(_) | common::Error::JsonDecodeError(_, _) => {
                        Self::unavailable("Gmail could not complete the operation.", None)
                    },
                }
            },
            Error::Response(_) | Error::Body(_) | Error::Transport(_) => {
                Self::unavailable("Gmail returned an incomplete or invalid response.", None)
            },
            Error::Serialize(_) | Error::Http(_) | Error::Mime(_) => {
                Self::internal("Gmail request construction failed.")
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_diagnostics_are_redacted_at_the_guest_boundary() {
        let error = Error::google(common::Error::BadRequest(
            serde_json::json!({"secret":"private-message"}),
        ));
        let failure = ExtensionError::from(error);
        assert!(matches!(failure, ExtensionError::Rejected(_)));
        assert!(!format!("{failure:?}").contains("private-message"));
    }

    #[test]
    fn status_mapping_retains_retry_time_and_connection_repair() {
        let retry = OffsetDateTime::UNIX_EPOCH;
        assert_eq!(
            ExtensionError::from(Error::status(http::StatusCode::TOO_MANY_REQUESTS, Some(retry))),
            ExtensionError::unavailable("Gmail is temporarily unavailable.", Some(retry))
        );
        assert!(matches!(
            ExtensionError::from(Error::status(http::StatusCode::UNAUTHORIZED, None)),
            ExtensionError::NotConnected(_)
        ));
    }
}
