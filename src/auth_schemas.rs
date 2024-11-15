use utoipa::openapi::security::{ApiKey, ApiKeyValue, Http, HttpAuthScheme, SecurityScheme};
use utoipa::Modify;
pub struct SecurityAddon;
impl Modify for SecurityAddon {
    fn modify(&self, openapi: &mut utoipa::openapi::OpenApi) {
        // Safely get the components
        let components = openapi.components.as_mut().unwrap();

        // Add a Bearer token security scheme globally
        components.add_security_scheme(
            "bearerAuth", // Name of the security scheme
            SecurityScheme::Http(
                Http::new(HttpAuthScheme::Bearer), // Define it as a Bearer token type
            ),
        );
        // Create the ApiKeyValue instance using the non-exhaustive struct's field(s)
        let api_key_value = ApiKeyValue::new("X-API-Authorization".to_string());

        // Using `ApiKey` enum to specify it as a header
        let api_key_scheme = SecurityScheme::ApiKey(ApiKey::Header(api_key_value));

        // Add the API key security scheme to the components
        components.add_security_scheme("apiBearerAuth", api_key_scheme);

        let api_isc_key_value = ApiKeyValue::new("X-ISC-API-Authorization".to_string());
        let api_isc_key_scheme = SecurityScheme::ApiKey(ApiKey::Header(api_isc_key_value));
        components.add_security_scheme("apiISCBearerAuth", api_isc_key_scheme);
    }
}
