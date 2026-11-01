//! Loopback test server whose request log excludes unsolicited IDE port probes.
// test: port_probes_are_ignored_but_extra_client_requests_are_counted

/// IDEs probe newly listening ports with GET / and a foreign (or absent) User-Agent.
/// Keep the exclusion narrow: OAuth can omit a UA, and unexpected client requests
/// must still count, even at /. This matches the UA set by DriveClient and its socket.
pub fn is_port_probe(method: &str, target: &str, user_agent: Option<&str>) -> bool {
    method == "GET" && target == "/" && user_agent != Some(concat!("konedrive/", env!("CARGO_PKG_VERSION")))
}

/// The same predicate for wiremock's parsed request (including any query string).
pub fn is_probe(request: &wiremock::Request) -> bool {
    is_port_probe(request.method.as_str(), &request.url[url::Position::BeforePath..],
        request.headers.get("user-agent").and_then(|value| value.to_str().ok()))
}

/// Wiremock's controls, with the request recorder shared by Graph and daemon tests.
pub struct MockServer(wiremock::MockServer);

impl MockServer {
    /// Start a loopback fixture with wiremock's default request recording.
    pub async fn start() -> Self {
        Self(wiremock::MockServer::start().await)
    }

    pub async fn received_requests(&self) -> Option<Vec<wiremock::Request>> {
        self.0.received_requests().await.map(|mut requests| {
            requests.retain(|request| !is_probe(request));
            requests
        })
    }
}

impl std::ops::Deref for MockServer {
    type Target = wiremock::MockServer;

    fn deref(&self) -> &Self::Target { &self.0 }
}

#[cfg(test)]
mod tests;
