use axum::{extract::{FromRequest, Request}, Json};
use serde::de::DeserializeOwned;
use validator::Validate;
use crate::error::ApiError;

pub struct ValidJson<T>(pub T);

impl<T, S> FromRequest<S> for ValidJson<T>
where
    T: DeserializeOwned + Validate + Send + 'static,
    S: Send + Sync,
{
    type Rejection = ApiError;

    async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
        let Json(value) = Json::<T>::from_request(req, state)
            .await
            .map_err(|e| ApiError::Validation(e.to_string()))?;
        value.validate().map_err(ApiError::from)?;
        Ok(ValidJson(value))
    }
}

#[cfg(test)]
mod tests {
    use validator::Validate;
    use serde::Deserialize;

    #[derive(Deserialize, Validate)]
    struct TestInput {
        #[validate(length(min = 1))]
        name: String,
    }

    #[test]
    fn test_valid_input_passes() {
        let input = TestInput { name: "hello".to_string() };
        assert!(input.validate().is_ok());
    }

    #[test]
    fn test_invalid_input_fails() {
        let input = TestInput { name: "".to_string() };
        assert!(input.validate().is_err());
    }
}
