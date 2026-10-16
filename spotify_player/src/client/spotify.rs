use librespot_core::session::Session;
use maybe_async::maybe_async;
use rspotify::{
    clients::{BaseClient, OAuthClient},
    http::{HttpClient, HttpError, Query},
    sync::Mutex,
    AuthCodePkceSpotify, ClientError, ClientResult, Config, Credentials, OAuth, Token,
};
use serde_json::Value;
use std::{
    fmt,
    future::Future,
    sync::{Arc, LazyLock},
    time::Duration,
};

use super::middleware::SpotifyApiRequestManager;
use crate::token;

#[derive(Clone, Default)]
/// A custom Spotify client to interact with the official Spotify API server
pub struct Spotify {
    creds: Credentials,
    oauth: OAuth,
    config: Config,
    token: Arc<Mutex<Option<Token>>>,
    http: HttpClient,
    session: Arc<tokio::sync::Mutex<Option<Session>>>,
}

#[allow(clippy::missing_fields_in_debug)] // Seems like not all fields are necessary in debug
impl fmt::Debug for Spotify {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Spotify")
            .field("creds", &self.creds)
            .field("oauth", &self.oauth)
            .field("config", &self.config)
            .field("token", &self.token)
            .finish()
    }
}

impl Spotify {
    /// Create a new Spotify client
    pub fn new() -> Spotify {
        Self {
            creds: Credentials::default(),
            oauth: OAuth::default(),
            config: Config {
                token_refreshing: true,
                ..Default::default()
            },
            token: Arc::new(Mutex::new(None)),
            http: HttpClient::default(),
            session: Arc::new(tokio::sync::Mutex::new(None)),
        }
    }

    pub async fn set_session(&self, session: Session) {
        *self.session.lock().await = Some(session);
    }

    pub async fn session(&self) -> Session {
        self.session
            .lock()
            .await
            .clone()
            .expect("non-empty Spotify session")
    }
}

// TODO: remove the below uses of `maybe_async` crate once
// async trait is fully supported in stable Rust.

#[maybe_async]
impl BaseClient for Spotify {
    fn get_http(&self) -> &HttpClient {
        &self.http
    }

    fn get_token(&self) -> Arc<Mutex<Option<Token>>> {
        Arc::clone(&self.token)
    }

    fn get_creds(&self) -> &Credentials {
        &self.creds
    }

    fn get_config(&self) -> &Config {
        &self.config
    }

    async fn refetch_token(&self) -> ClientResult<Option<Token>> {
        let session = self.session().await;
        let old_token = self.token.lock().await.unwrap().clone();

        if session.is_invalid() {
            tracing::error!("Failed to get a new token: invalid session");
            return Ok(old_token);
        }

        match token::get_token_rspotify(&session).await {
            Ok(token) => Ok(Some(token)),
            Err(err) => {
                tracing::error!("Failed to get a new token: {err:#}");
                Ok(old_token)
            }
        }
    }
}

/// Implement `OAuthClient` trait for `Spotify` struct
/// to allow calling methods that get/modify user's data such as
/// `current_user_playlists`, `playlist_add_items`, etc.
///
/// Because the `Spotify` client interacts with Spotify APIs
/// using an access token that is manually retrieved by
/// the `librespot::get_token` function, implementing
/// `OAuthClient::get_oauth` and `OAuthClient::request_token` is unnecessary
#[maybe_async]
impl OAuthClient for Spotify {
    fn get_oauth(&self) -> &OAuth {
        panic!("`OAuthClient::get_oauth` should never be called!")
    }

    async fn request_token(&self, _code: &str) -> ClientResult<()> {
        panic!("`OAuthClient::request_token` should never be called!")
    }
}

/// Thin wrapper over [`rspotify::AuthCodePkceSpotify`] for the Spotify Web API.
///
/// It exists solely to override `refetch_token` so that a refresh grant which
/// omits `refresh_token` preserves the previously stored refresh token instead
/// of overwriting it with `None`. Spotify stopped rotating (and now expires)
/// refresh tokens, so the refresh response no longer echoes one back.
///
/// See:
/// - <https://developer.spotify.com/blog/2026-06-18-refresh-token-expiration>
/// - <https://github.com/aome510/spotify-player/issues/1040>
#[derive(Clone, Debug, Default)]
pub(crate) struct PkceWebApiClient(AuthCodePkceSpotify);

impl PkceWebApiClient {
    pub(crate) fn new(inner: AuthCodePkceSpotify) -> Self {
        Self(inner)
    }

    pub(crate) fn get_authorize_url(
        &mut self,
        verifier_bytes: Option<usize>,
    ) -> ClientResult<String> {
        self.0.get_authorize_url(verifier_bytes)
    }
}

#[maybe_async]
impl BaseClient for PkceWebApiClient {
    fn get_http(&self) -> &HttpClient {
        self.0.get_http()
    }

    fn get_token(&self) -> Arc<Mutex<Option<Token>>> {
        self.0.get_token()
    }

    fn get_creds(&self) -> &Credentials {
        self.0.get_creds()
    }

    fn get_config(&self) -> &Config {
        self.0.get_config()
    }

    async fn refetch_token(&self) -> ClientResult<Option<Token>> {
        // Capture the current refresh token before refreshing
        let previous_refresh_token = self
            .0
            .get_token()
            .lock()
            .await
            .unwrap()
            .as_ref()
            .and_then(|token| token.refresh_token.clone());

        // Spotify's refresh response no longer includes `refresh_token`; carry the
        // previous one forward so it is not lost on the round-trip and nulled in the
        // token cache.
        let refreshed = self.0.refetch_token().await?;
        Ok(refreshed.map(|mut token| {
            if token.refresh_token.is_none() {
                token.refresh_token = previous_refresh_token;
            }
            token
        }))
    }
}

#[maybe_async]
impl OAuthClient for PkceWebApiClient {
    fn get_oauth(&self) -> &OAuth {
        self.0.get_oauth()
    }

    async fn request_token(&self, code: &str) -> ClientResult<()> {
        self.0.request_token(code).await
    }
}

/// Spotify Web API client with an optional fallback identity.
#[derive(Clone, Debug)]
pub struct WebApiClient {
    primary: PkceWebApiClient,
    fallback: Option<PkceWebApiClient>,
    /// Rate-limit state of `primary` and `fallback`, used to route requests away from a client
    /// that is waiting out a `Retry-After` delay.
    rate_limits: Option<(SpotifyApiRequestManager, SpotifyApiRequestManager)>,
    ncspot_only_get_endpoints: Vec<String>,
}

/// Whether a request should go to the other client instead of the preferred one, given the
/// remaining `Retry-After` delay of each.
fn should_reroute(preferred: Option<Duration>, other: Option<Duration>) -> bool {
    match (preferred, other) {
        (None, _) => false,
        (Some(_), None) => true,
        (Some(preferred), Some(other)) => other < preferred,
    }
}

/// Advice attached to a rate-limit warning when the app has only the shared ncspot client ID.
static ONE_CLIENT_RATE_LIMIT_HINT: LazyLock<String> = LazyLock::new(|| {
    format!(
        "requests are paused until the Retry-After delay elapses; setting `client_id` in {} \
         gives the app its own Spotify API quota",
        crate::config::app_config_path()
    )
});
/// Advice attached to a rate-limit warning when neither client ID can serve the request.
const BOTH_CLIENTS_RATE_LIMIT_HINT: &str = "both Spotify client IDs are rate limited; requests \
     are paused until a Retry-After delay elapses";

/// One of the two Spotify clients a request can be routed to.
#[derive(Clone, Copy)]
struct Route<'a> {
    name: &'static str,
    client: &'a PkceWebApiClient,
    limits: Option<&'a SpotifyApiRequestManager>,
}

impl Route<'_> {
    /// How long this client must still wait out a `Retry-After` delay, if any.
    async fn cooldown(&self) -> Option<Duration> {
        match self.limits {
            Some(limits) => limits.retry_after_remaining().await,
            None => None,
        }
    }
}

impl Default for WebApiClient {
    fn default() -> Self {
        Self::new(AuthCodePkceSpotify::default(), None)
    }
}

impl WebApiClient {
    pub fn new(primary: AuthCodePkceSpotify, fallback: Option<AuthCodePkceSpotify>) -> Self {
        Self {
            primary: PkceWebApiClient::new(primary),
            fallback: fallback.map(PkceWebApiClient::new),
            rate_limits: None,
            ncspot_only_get_endpoints: crate::config::DEFAULT_NCSPOT_ONLY_GET_ENDPOINTS
                .iter()
                .map(ToString::to_string)
                .collect(),
        }
    }

    pub fn with_ncspot_only_get_endpoints(
        mut self,
        ncspot_only_get_endpoints: Vec<String>,
    ) -> Self {
        self.ncspot_only_get_endpoints = ncspot_only_get_endpoints;
        self
    }

    pub(super) fn with_rate_limits(
        mut self,
        primary: SpotifyApiRequestManager,
        fallback: SpotifyApiRequestManager,
    ) -> Self {
        self.rate_limits = Some((primary, fallback));
        self
    }

    pub(crate) fn primary_mut(&mut self) -> &mut PkceWebApiClient {
        &mut self.primary
    }

    pub(crate) fn fallback_mut(&mut self) -> Option<&mut PkceWebApiClient> {
        self.fallback.as_mut()
    }

    fn is_ncspot_only_get(&self, url: &str) -> bool {
        self.ncspot_only_get_endpoints
            .iter()
            .any(|endpoint| url.starts_with(endpoint))
    }

    fn fallback_status(error: &ClientError) -> Option<reqwest::StatusCode> {
        match error {
            ClientError::Http(error) => match error.as_ref() {
                HttpError::StatusCode(response) if response.status().is_client_error() => {
                    Some(response.status())
                }
                HttpError::Client(_) | HttpError::StatusCode(_) => None,
            },
            _ => None,
        }
    }

    /// Sends a request through the preferred client and retries it once through the other
    /// client on a `4xx` response.
    ///
    /// The custom (primary) client is preferred unless `prefer_ncspot` is set. Either way, the
    /// other client goes first while it would be available sooner than a rate-limited preferred one.
    /// A `429` is reported instead of being retried through a client that is itself rate limited,
    /// which would only wait out a second delay before failing as well.
    ///
    /// A rate limit is warned about, and so raises an alert, only once no client ID is left to
    /// serve the request; every `429` on its own is recorded in the log by the middleware.
    async fn route_request<'a, F, Fut>(
        &'a self,
        method: &str,
        url: &str,
        prefer_ncspot: bool,
        request: F,
    ) -> ClientResult<String>
    where
        F: Fn(&'a PkceWebApiClient) -> Fut,
        Fut: Future<Output = ClientResult<String>>,
    {
        let Some(fallback) = &self.fallback else {
            let result = request(&self.primary).await;
            if Self::is_rate_limited(&result) {
                Self::warn_rate_limited(method, url, "ncspot", &ONE_CLIENT_RATE_LIMIT_HINT);
            }
            return result;
        };

        let (custom_limits, ncspot_limits) = match &self.rate_limits {
            Some((custom, ncspot)) => (Some(custom), Some(ncspot)),
            None => (None, None),
        };
        let custom = Route {
            name: "custom",
            client: &self.primary,
            limits: custom_limits,
        };
        let ncspot = Route {
            name: "ncspot",
            client: fallback,
            limits: ncspot_limits,
        };
        let (mut first, mut second) = if prefer_ncspot {
            (ncspot, custom)
        } else {
            (custom, ncspot)
        };

        let preferred_cooldown = first.cooldown().await;
        if should_reroute(preferred_cooldown, second.cooldown().await) {
            tracing::info!(
                method,
                url,
                client = first.name,
                remaining_secs = preferred_cooldown.unwrap_or_default().as_secs(),
                "Spotify client is rate limited; routing request to the {} client",
                second.name
            );
            std::mem::swap(&mut first, &mut second);
        }

        let error = match request(first.client).await {
            Err(error) => error,
            result => return result,
        };
        let Some(status) = Self::fallback_status(&error) else {
            return Err(error);
        };
        let rate_limited = status == reqwest::StatusCode::TOO_MANY_REQUESTS;
        let clients = format!("{} and {}", first.name, second.name);

        let fallback_cooldown = if rate_limited {
            second.cooldown().await
        } else {
            None
        };
        if let Some(remaining) = fallback_cooldown {
            tracing::info!(
                method,
                url,
                client = second.name,
                remaining_secs = remaining.as_secs(),
                "Both Spotify clients are rate limited; not retrying the request"
            );
            Self::warn_rate_limited(method, url, &clients, BOTH_CLIENTS_RATE_LIMIT_HINT);
            return Err(error);
        }

        // `4xx` responses are expected for endpoints a client ID is not allowed to access, and
        // a `429` is still served by the other client, so neither is worth an alert yet
        tracing::info!(
            method,
            url,
            %status,
            "Spotify Web API request through the {} client failed; retrying with the {} client",
            first.name,
            second.name
        );
        let result = request(second.client).await;
        if result.is_err() && (rate_limited || Self::is_rate_limited(&result)) {
            Self::warn_rate_limited(method, url, &clients, BOTH_CLIENTS_RATE_LIMIT_HINT);
        }
        result
    }

    /// Whether a failed request was rejected with `429 Too Many Requests`.
    fn is_rate_limited(result: &ClientResult<String>) -> bool {
        result
            .as_ref()
            .err()
            .and_then(Self::fallback_status)
            .is_some_and(|status| status == reqwest::StatusCode::TOO_MANY_REQUESTS)
    }

    /// Warn that a rate-limited request has no client ID left to serve it.
    ///
    /// Rate limiting is otherwise invisible - requests are delayed and retried in the
    /// background, which looks like an unresponsive application - so this raises an alert.
    fn warn_rate_limited(method: &str, url: &str, clients: &str, hint: &str) {
        tracing::warn!(
            method,
            url,
            clients,
            hint,
            "Spotify Web API rate limit encountered"
        );
    }
}

#[maybe_async]
impl BaseClient for WebApiClient {
    fn get_http(&self) -> &HttpClient {
        self.primary.get_http()
    }

    fn get_token(&self) -> Arc<Mutex<Option<Token>>> {
        self.primary.get_token()
    }

    fn get_creds(&self) -> &Credentials {
        self.primary.get_creds()
    }

    fn get_config(&self) -> &Config {
        self.primary.get_config()
    }

    async fn refetch_token(&self) -> ClientResult<Option<Token>> {
        self.primary.refetch_token().await
    }

    async fn api_get(&self, url: &str, payload: &Query<'_>) -> ClientResult<String> {
        self.route_request("GET", url, self.is_ncspot_only_get(url), |client| {
            client.api_get(url, payload)
        })
        .await
    }

    async fn api_post(&self, url: &str, payload: &Value) -> ClientResult<String> {
        self.route_request("POST", url, false, |client| client.api_post(url, payload))
            .await
    }

    async fn api_put(&self, url: &str, payload: &Value) -> ClientResult<String> {
        self.route_request("PUT", url, false, |client| client.api_put(url, payload))
            .await
    }

    async fn api_delete(&self, url: &str, payload: &Value) -> ClientResult<String> {
        self.route_request("DELETE", url, false, |client| {
            client.api_delete(url, payload)
        })
        .await
    }
}

#[maybe_async]
impl OAuthClient for WebApiClient {
    fn get_oauth(&self) -> &OAuth {
        self.primary.get_oauth()
    }

    async fn request_token(&self, code: &str) -> ClientResult<()> {
        self.primary.request_token(code).await
    }
}

#[cfg(test)]
mod tests {
    use super::{should_reroute, WebApiClient, ONE_CLIENT_RATE_LIMIT_HINT};
    use crate::{
        client::middleware::{SpotifyApiMiddleware, SpotifyApiRequestManager},
        config::AlertLevel,
        log_layer::AlertLayer,
        state::AlertQueue,
    };
    use parking_lot::Mutex;
    use rspotify::{
        clients::BaseClient, http::Query, AuthCodePkceSpotify, Config, Credentials, OAuth, Token,
    };
    use std::{collections::HashSet, future::Future, sync::Arc, time::Duration};
    use tracing_subscriber::layer::SubscriberExt;
    use wiremock::{
        matchers::{header, method, path},
        Mock, MockServer, Request, Respond, ResponseTemplate,
    };

    #[derive(Clone)]
    struct RateLimitThenOk {
        calls: Arc<std::sync::atomic::AtomicUsize>,
    }

    impl Respond for RateLimitThenOk {
        fn respond(&self, _request: &Request) -> ResponseTemplate {
            if self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
                ResponseTemplate::new(429).insert_header("retry-after", "0")
            } else {
                ResponseTemplate::new(200).set_body_string("fallback")
            }
        }
    }

    fn middleware(server: &MockServer, client: &str, retries: usize) -> SpotifyApiMiddleware {
        SpotifyApiMiddleware::new(&format!("{}/v1", server.uri()), retries, client).unwrap()
    }

    async fn client_with_token(
        server: &MockServer,
        with_middleware: bool,
        access_token: &str,
    ) -> AuthCodePkceSpotify {
        let middleware = with_middleware.then(|| middleware(server, "ncspot", 1));
        client_with_middleware(server, middleware, access_token).await
    }

    async fn client_with_middleware(
        server: &MockServer,
        middleware: Option<SpotifyApiMiddleware>,
        access_token: &str,
    ) -> AuthCodePkceSpotify {
        let config = Config {
            api_base_url: format!("{}/v1", server.uri()),
            ..Default::default()
        };
        let client = AuthCodePkceSpotify::with_config(
            Credentials::new_pkce("client-id"),
            OAuth::default(),
            config,
        );
        let client = match middleware {
            Some(middleware) => client.with_middleware(middleware),
            None => client,
        };
        *client.get_token().lock().await.unwrap() = Some(Token {
            access_token: access_token.to_string(),
            expires_in: chrono::Duration::hours(1),
            expires_at: Some(chrono::Utc::now() + chrono::Duration::hours(1)),
            scopes: HashSet::new(),
            refresh_token: Some("refresh-token".to_string()),
        });
        client
    }

    async fn client(server: &MockServer, with_middleware: bool) -> AuthCodePkceSpotify {
        client_with_token(server, with_middleware, "access-token").await
    }

    /// Builds a client whose primary and fallback track each other's rate limits, as in
    /// production, along with the request state of each.
    async fn rate_limit_aware_client(
        primary_server: &MockServer,
        fallback_server: &MockServer,
        retries: usize,
    ) -> (
        WebApiClient,
        SpotifyApiRequestManager,
        SpotifyApiRequestManager,
    ) {
        let primary_middleware = middleware(primary_server, "custom", retries);
        let fallback_middleware = middleware(fallback_server, "ncspot", retries);
        let primary_requests = primary_middleware.requests();
        let fallback_requests = fallback_middleware.requests();

        let client = WebApiClient::new(
            client_with_middleware(
                primary_server,
                Some(primary_middleware.with_peer(fallback_requests.clone())),
                "access-token",
            )
            .await,
            Some(
                client_with_middleware(
                    fallback_server,
                    Some(fallback_middleware.with_peer(primary_requests.clone())),
                    "access-token",
                )
                .await,
            ),
        )
        .with_rate_limits(primary_requests.clone(), fallback_requests.clone());
        (client, primary_requests, fallback_requests)
    }

    #[test]
    fn reroutes_only_when_other_client_is_available_sooner() {
        let short = Some(Duration::from_secs(5));
        let long = Some(Duration::from_secs(30));

        assert!(!should_reroute(None, None));
        assert!(!should_reroute(None, short));
        assert!(should_reroute(short, None));
        assert!(should_reroute(long, short));
        assert!(!should_reroute(short, long));
    }

    #[tokio::test]
    async fn rate_limited_primary_is_skipped_until_retry_after_elapses() {
        let primary_server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "60"))
            .expect(1)
            .mount(&primary_server)
            .await;
        let fallback_server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_string("fallback"))
            .expect(2)
            .mount(&fallback_server)
            .await;

        let (client, ..) = rate_limit_aware_client(&primary_server, &fallback_server, 1).await;

        assert_eq!(
            client.api_get("first", &Query::new()).await.unwrap(),
            "fallback"
        );
        assert_eq!(
            client.api_get("second", &Query::new()).await.unwrap(),
            "fallback"
        );
    }

    #[tokio::test]
    async fn ncspot_only_rate_limit_falls_back_to_primary_without_waiting() {
        let primary_server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/me/playlists"))
            .respond_with(ResponseTemplate::new(200).set_body_string("primary"))
            .expect(1)
            .mount(&primary_server)
            .await;
        let fallback_server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/me/playlists"))
            .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "60"))
            .expect(1)
            .mount(&fallback_server)
            .await;

        let (client, ..) = rate_limit_aware_client(&primary_server, &fallback_server, 1).await;

        assert_eq!(
            client.api_get("me/playlists", &Query::new()).await.unwrap(),
            "primary"
        );
    }

    /// Run `request` against a subscriber that collects the alerts it raises, as the
    /// application's alert popup would show them.
    async fn alerts_from<T>(request: impl Future<Output = T>) -> (T, Arc<Mutex<AlertQueue>>) {
        let alerts = Arc::new(Mutex::new(AlertQueue::default()));
        let dispatch = tracing::Dispatch::new(
            tracing_subscriber::registry()
                .with(AlertLayer::new(Arc::clone(&alerts), AlertLevel::Warn)),
        );

        let guard = tracing::dispatcher::set_default(&dispatch);
        let result = request.await;
        drop(guard);

        (result, alerts)
    }

    #[test]
    fn the_rate_limit_hint_names_the_config_file() {
        let hint = ONE_CLIENT_RATE_LIMIT_HINT.as_str();

        assert!(hint.contains("client_id"), "{hint}");
        // the path is the file's name only while the configurations are not initialized
        assert!(hint.contains(&crate::config::app_config_path()), "{hint}");
    }

    #[tokio::test]
    async fn a_rate_limit_the_other_client_serves_raises_no_alert() {
        let primary_server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "60"))
            .mount(&primary_server)
            .await;
        let fallback_server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_string("fallback"))
            .mount(&fallback_server)
            .await;
        let (client, ..) = rate_limit_aware_client(&primary_server, &fallback_server, 0).await;

        let (result, alerts) = alerts_from(client.api_get("first", &Query::new())).await;

        assert_eq!(result.unwrap(), "fallback");
        assert!(alerts.lock().is_empty());
    }

    #[tokio::test]
    async fn a_rate_limit_on_both_clients_raises_one_alert() {
        let primary_server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "60"))
            .mount(&primary_server)
            .await;
        let fallback_server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "60"))
            .mount(&fallback_server)
            .await;
        let (client, ..) = rate_limit_aware_client(&primary_server, &fallback_server, 0).await;

        let (result, alerts) = alerts_from(client.api_get("first", &Query::new())).await;

        assert!(result.is_err());
        let alerts = alerts.lock();
        assert_eq!(alerts.len(), 1);
        let alert = alerts.current().expect("one alert");
        assert_eq!(alert.message, "Spotify Web API rate limit encountered");
        assert!(
            alert
                .fields
                .iter()
                .any(|(name, value)| name == "clients" && value == "custom and ncspot"),
            "{:?}",
            alert.fields
        );
    }

    #[tokio::test]
    async fn rate_limited_request_is_not_retried_while_the_other_client_waits() {
        let primary_server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "60"))
            .expect(1)
            .mount(&primary_server)
            .await;
        let fallback_server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_string("fallback"))
            .expect(0)
            .mount(&fallback_server)
            .await;

        // without retries the primary reports its `429` without waiting out the delay first
        let (client, _, fallback_requests) =
            rate_limit_aware_client(&primary_server, &fallback_server, 0).await;
        fallback_requests
            .store_retry_after(Duration::from_secs(60))
            .await;

        assert!(client.api_get("first", &Query::new()).await.is_err());
    }

    #[tokio::test]
    async fn ncspot_only_client_error_falls_back_to_primary() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/me/playlists"))
            .and(header("authorization", "Bearer fallback-token"))
            .respond_with(ResponseTemplate::new(403))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/v1/me/playlists"))
            .and(header("authorization", "Bearer primary-token"))
            .respond_with(ResponseTemplate::new(200).set_body_string("primary"))
            .expect(1)
            .mount(&server)
            .await;

        let client = WebApiClient::new(
            client_with_token(&server, false, "primary-token").await,
            Some(client_with_token(&server, true, "fallback-token").await),
        );

        assert_eq!(
            client.api_get("me/playlists", &Query::new()).await.unwrap(),
            "primary"
        );
    }

    #[tokio::test]
    async fn successful_primary_request_does_not_use_fallback() {
        let primary_server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/test"))
            .respond_with(ResponseTemplate::new(200).set_body_string("primary"))
            .expect(1)
            .mount(&primary_server)
            .await;
        let fallback_server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/test"))
            .respond_with(ResponseTemplate::new(200).set_body_string("fallback"))
            .expect(0)
            .mount(&fallback_server)
            .await;

        let client = WebApiClient::new(
            client(&primary_server, false).await,
            Some(client(&fallback_server, true).await),
        );

        assert_eq!(
            client.api_get("test", &Query::new()).await.unwrap(),
            "primary"
        );
    }

    #[tokio::test]
    async fn server_error_does_not_use_fallback() {
        let primary_server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/test"))
            .respond_with(ResponseTemplate::new(500))
            .expect(1)
            .mount(&primary_server)
            .await;
        let fallback_server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/test"))
            .respond_with(ResponseTemplate::new(200))
            .expect(0)
            .mount(&fallback_server)
            .await;

        let client = WebApiClient::new(
            client(&primary_server, false).await,
            Some(client(&fallback_server, true).await),
        );

        assert!(client.api_get("test", &Query::new()).await.is_err());
    }

    #[tokio::test]
    async fn custom_rate_limit_falls_back_before_ncspot_retries() {
        let primary_server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/test"))
            .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "0"))
            .expect(1)
            .mount(&primary_server)
            .await;
        let fallback_server = MockServer::start().await;
        let fallback_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        Mock::given(method("GET"))
            .and(path("/v1/test"))
            .respond_with(RateLimitThenOk {
                calls: Arc::clone(&fallback_calls),
            })
            .mount(&fallback_server)
            .await;

        let client = WebApiClient::new(
            client(&primary_server, false).await,
            Some(client(&fallback_server, true).await),
        );

        assert_eq!(
            client.api_get("test", &Query::new()).await.unwrap(),
            "fallback"
        );
        assert_eq!(fallback_calls.load(std::sync::atomic::Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn client_error_on_mutation_uses_fallback_once() {
        let primary_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/test"))
            .respond_with(ResponseTemplate::new(403))
            .expect(1)
            .mount(&primary_server)
            .await;
        let fallback_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/test"))
            .respond_with(ResponseTemplate::new(200).set_body_string("fallback"))
            .expect(1)
            .mount(&fallback_server)
            .await;

        let client = WebApiClient::new(
            client(&primary_server, false).await,
            Some(client(&fallback_server, true).await),
        );

        assert_eq!(
            client
                .api_post("test", &serde_json::json!({ "name": "playlist" }))
                .await
                .unwrap(),
            "fallback"
        );
    }

    #[tokio::test]
    async fn api_get_uses_fallback_token() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/test"))
            .and(header("authorization", "Bearer primary-token"))
            .respond_with(ResponseTemplate::new(403))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/v1/test"))
            .and(header("authorization", "Bearer fallback-token"))
            .respond_with(ResponseTemplate::new(200).set_body_string("fallback"))
            .expect(1)
            .mount(&server)
            .await;

        let client = WebApiClient::new(
            client_with_token(&server, false, "primary-token").await,
            Some(client_with_token(&server, true, "fallback-token").await),
        );

        assert_eq!(
            client.api_get("test", &Query::new()).await.unwrap(),
            "fallback"
        );
    }

    #[tokio::test]
    async fn configured_endpoint_prefix_uses_ncspot_without_calling_primary() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/artists/artist-id"))
            .and(header("authorization", "Bearer primary-token"))
            .respond_with(ResponseTemplate::new(200).set_body_string("primary"))
            .expect(0)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/v1/artists/artist-id"))
            .and(header("authorization", "Bearer fallback-token"))
            .respond_with(ResponseTemplate::new(200).set_body_string("ncspot"))
            .expect(1)
            .mount(&server)
            .await;

        let client = WebApiClient::new(
            client_with_token(&server, false, "primary-token").await,
            Some(client_with_token(&server, true, "fallback-token").await),
        )
        .with_ncspot_only_get_endpoints(vec!["artists/".to_string()]);

        assert_eq!(
            client
                .api_get("artists/artist-id", &Query::new())
                .await
                .unwrap(),
            "ncspot"
        );
    }

    #[tokio::test]
    async fn current_user_playlists_uses_ncspot_without_calling_primary() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/me/playlists"))
            .and(header("authorization", "Bearer primary-token"))
            .respond_with(ResponseTemplate::new(200).set_body_string("primary"))
            .expect(0)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/v1/me/playlists"))
            .and(header("authorization", "Bearer fallback-token"))
            .respond_with(ResponseTemplate::new(200).set_body_string("ncspot"))
            .expect(1)
            .mount(&server)
            .await;

        let client = WebApiClient::new(
            client_with_token(&server, false, "primary-token").await,
            Some(client_with_token(&server, true, "fallback-token").await),
        );

        assert_eq!(
            client.api_get("me/playlists", &Query::new()).await.unwrap(),
            "ncspot"
        );
    }
}
