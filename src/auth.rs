use crate::api::TC_ROOT_URL;
use crate::util::{run_hint, scope_satisfies, write_atomic};
use anyhow::{anyhow, bail, Context, Result};
use jiff::{tz::Offset, SignedDuration, Timestamp};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::ffi::OsStr;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use url::Url;

const LEVEL_1_SCOPE: &str = "hooks:trigger-hook:project-gecko/in-tree-action-1-generic/*";
const LEVEL_3_SCOPE: &str = "hooks:trigger-hook:project-gecko/in-tree-action-3-generic/*";
const LOGIN_TIMEOUT: Duration = Duration::from_secs(5 * 60);
const CALLBACK_IO_TIMEOUT: Duration = Duration::from_secs(2);
const HOURS_PER_DAY: i64 = 24;
const EXPIRY_WARNING: SignedDuration = SignedDuration::from_hours(7 * HOURS_PER_DAY);
const MAX_REQUEST_HEAD: usize = 8 * 1024;
const SUCCESS_PAGE: &str = "<!DOCTYPE html><title>treeherder-cli</title>\
<script>history.replaceState(null, '', '/')</script>\
<p>Signed in to Taskcluster, you can close this window.</p>";

pub enum ActiveCredentials {
    /// From TASKCLUSTER_* environment variables: level and expiry are unknown.
    Env(taskcluster::Credentials),
    Stored(StoredCredentials),
}

#[derive(Serialize, Deserialize)]
pub struct StoredCredentials {
    client_id: String,
    access_token: String,
    level: u8,
    expires_at: Timestamp,
}

impl ActiveCredentials {
    fn client_id(&self) -> &str {
        match self {
            Self::Env(credentials) => &credentials.client_id,
            Self::Stored(stored) => &stored.client_id,
        }
    }

    pub fn into_credentials(self) -> taskcluster::Credentials {
        match self {
            Self::Env(credentials) => credentials,
            Self::Stored(stored) => {
                taskcluster::Credentials::new(stored.client_id, stored.access_token)
            }
        }
    }

    /// How to fix credentials that Taskcluster rejected, or that lack the scopes for `level`.
    pub fn fix_hint(&self, level: u8) -> String {
        match self {
            Self::Env(_) => "TASKCLUSTER_CLIENT_ID and TASKCLUSTER_ACCESS_TOKEN take precedence over the stored login: fix or unset them.".to_string(),
            Self::Stored(stored) => run_hint(login_command(stored.level.max(level))),
        }
    }
}

pub fn load_credentials() -> Result<Option<ActiveCredentials>> {
    if let Some(credentials) = env_credentials(|name| std::env::var(name).ok()) {
        return Ok(Some(ActiveCredentials::Env(credentials)));
    }
    Ok(read_stored(&credentials_path()?)?.map(ActiveCredentials::Stored))
}

/// Returns the credentials to act with, or an error telling the reader how to fix the login.
pub fn require_credentials(level_needed: u8) -> Result<ActiveCredentials> {
    let Some(active) = load_credentials()? else {
        bail!(
            "Not logged in to Taskcluster. {}",
            run_hint(login_command(level_needed))
        );
    };
    if let ActiveCredentials::Stored(stored) = &active {
        if stored.level < level_needed {
            bail!(
                "This action needs level-{level_needed} access but you are logged in at level {}. {}",
                stored.level,
                active.fix_hint(level_needed)
            );
        }
        let now = Timestamp::now();
        if stored.expires_at <= now {
            bail!(
                "Taskcluster login expired on {}. {}",
                format_date(stored.expires_at),
                active.fix_hint(level_needed)
            );
        }
        if expires_soon(stored.expires_at, now) {
            eprintln!(
                "warning: Taskcluster login expires on {}. {}",
                format_date(stored.expires_at),
                active.fix_hint(level_needed)
            );
        }
    }
    Ok(active)
}

pub async fn login(level: u8, days: u32) -> Result<()> {
    let path = credentials_path()?;
    let previous = read_stored(&path).ok().flatten();
    Timestamp::now()
        .checked_add(SignedDuration::from_hours(i64::from(days) * HOURS_PER_DAY))
        .context("--days is too large")?;

    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .context("binding a local port for the sign-in callback")?;
    let callback_port = listener.local_addr()?.port();
    let client_name = format!("treeherder-cli-{}", &slugid::v4()[..6]);
    let callback_path = format!("/{}", slugid::v4());
    let hostname = gethostname::gethostname().to_string_lossy().into_owned();
    let description = format!("treeherder-cli on {hostname}");
    let callback_url = format!("http://localhost:{callback_port}{callback_path}");
    let url = login_url(
        TC_ROOT_URL,
        level,
        days,
        &client_name,
        &description,
        &callback_url,
    )?;

    println!("Open this URL to sign in: {url}");
    if std::env::var_os("SSH_CONNECTION").is_some() {
        println!(
            "Over SSH, the browser must reach port {callback_port} of this machine. From the machine running the browser, run: ssh -L {callback_port}:127.0.0.1:{callback_port} <this host>"
        );
    }
    if let Err(err) = open::that_detached(url.as_str()) {
        println!("Could not open a browser ({err}); open the URL above manually.");
    }
    println!("Waiting for sign-in (up to 5 minutes)...");
    let (credentials, mut browser) =
        tokio::time::timeout(LOGIN_TIMEOUT, wait_for_callback(&listener, &callback_path))
            .await
            .map_err(|_| {
                anyhow!(
                "Timed out after 5 minutes waiting for the Taskcluster sign-in; run login again"
            )
            })??;

    let client_id = credentials.client_id.clone();
    let result = tokio::time::timeout(Duration::from_secs(60), verify_login(credentials, level))
        .await
        .context("Timed out verifying the new Taskcluster client")
        .and_then(|result| result)
        .and_then(|stored| {
            write_stored(&path, &stored)?;
            Ok(stored)
        });
    match &result {
        Ok(_) => respond(&mut browser, "200 OK", SUCCESS_PAGE).await,
        Err(_) => {
            respond(
                &mut browser,
                "400 Bad Request",
                "<script>history.replaceState(null, '', '/')</script><p>Sign-in failed. See the terminal.</p>",
            )
            .await
        }
    }
    let stored = match result {
        Ok(stored) => stored,
        Err(err) => {
            let client_url = client_ui_url(&client_id)?;
            return Err(err).with_context(|| {
                format!(
                    "Could not save the new login. Review or revoke Taskcluster client {client_id} at {client_url}"
                )
            });
        }
    };
    println!(
        "Logged in as {} (level {level}, expires {})",
        stored.client_id,
        format_date(stored.expires_at)
    );
    if let Some(previous) = previous {
        let validity = if previous.expires_at > Timestamp::now() {
            format!("is still valid until {}", format_date(previous.expires_at))
        } else {
            format!("expired on {}", format_date(previous.expires_at))
        };
        println!(
            "Previous client {} {validity}; delete it at {}",
            previous.client_id,
            client_ui_url(&previous.client_id)?
        );
    }
    Ok(())
}

#[derive(Deserialize)]
struct ClientDetails {
    expires: Timestamp,
    scopes: Vec<String>,
}

async fn verify_login(
    credentials: taskcluster::Credentials,
    level: u8,
) -> Result<StoredCredentials> {
    let auth = taskcluster::Auth::new(
        taskcluster::ClientBuilder::new(TC_ROOT_URL).credentials(credentials.clone()),
    )?;
    let details: ClientDetails = serde_json::from_value(auth.client(&credentials.client_id).await?)
        .context("reading the new Taskcluster client's details")?;
    let effective: CurrentScopes = serde_json::from_value(auth.currentScopes().await?)
        .context("checking the new Taskcluster client's credentials")?;
    anyhow::ensure!(
        details.expires > Timestamp::now(),
        "The new Taskcluster client has already expired"
    );
    let requested = scopes_for_level(level);
    for required in &requested {
        anyhow::ensure!(
            effective
                .scopes
                .iter()
                .any(|scope| scope_satisfies(scope, required)),
            "The new Taskcluster client lacks the requested level-{level} scope {required}"
        );
    }
    anyhow::ensure!(
        details.scopes.iter().all(|scope| requested
            .iter()
            .any(|allowed| scope_satisfies(allowed, scope))),
        "The new Taskcluster client has scopes outside the requested action permissions"
    );
    Ok(StoredCredentials {
        client_id: credentials.client_id,
        access_token: credentials.access_token,
        level,
        expires_at: details.expires,
    })
}

pub fn logout() -> Result<()> {
    let path = credentials_path()?;
    let env_set = env_credentials(|name| std::env::var(name).ok()).is_some();
    match read_stored(&path)? {
        Some(stored) => {
            fs::remove_file(&path).with_context(|| format!("removing {}", path.display()))?;
            println!(
                "Logged out. Delete client {} at {} to revoke it.",
                stored.client_id,
                client_ui_url(&stored.client_id)?
            );
        }
        None => println!("Not logged in."),
    }
    if env_set {
        println!("TASKCLUSTER_* environment variables are still set and still apply.");
    }
    Ok(())
}

pub async fn status() -> Result<()> {
    let Some(active) = load_credentials()? else {
        println!(
            "Not logged in to Taskcluster. {}",
            run_hint(login_command(1))
        );
        return Ok(());
    };
    match &active {
        ActiveCredentials::Env(_) => {
            println!("Source: TASKCLUSTER_* environment variables");
            println!("Client: {}", active.client_id());
            println!("Level: unknown");
            println!("Expires: unknown");
        }
        ActiveCredentials::Stored(stored) => {
            let now = Timestamp::now();
            println!("Source: {}", credentials_path()?.display());
            println!("Client: {}", stored.client_id);
            println!("Level: {}", stored.level);
            if stored.expires_at <= now {
                println!("Expires: {} (expired)", format_date(stored.expires_at));
            } else {
                println!(
                    "Expires: {} ({} days left)",
                    format_date(stored.expires_at),
                    stored.expires_at.duration_since(now).as_hours() / HOURS_PER_DAY
                );
            }
        }
    }

    let fix_hint = active.fix_hint(1);
    let auth = taskcluster::Auth::new(
        taskcluster::ClientBuilder::new(TC_ROOT_URL).credentials(active.into_credentials()),
    )?;
    match auth.currentScopes().await {
        Ok(value) => {
            let scopes: CurrentScopes = serde_json::from_value(value)?;
            // The rest are the read-only scopes everyone gets from the anonymous role.
            println!("Action scopes:");
            for scope in scopes
                .scopes
                .iter()
                .filter(|scope| scope.starts_with("hooks:trigger-hook:"))
            {
                println!("  {scope}");
            }
            Ok(())
        }
        Err(err)
            if taskcluster::err_status_code(&err)
                == Some(taskcluster::StatusCode::UNAUTHORIZED) =>
        {
            bail!(
                "Taskcluster rejected this client (401): it has expired, been deleted or been disabled. {fix_hint}"
            )
        }
        Err(err) => Err(err),
    }
}

#[derive(Deserialize)]
struct CurrentScopes {
    scopes: Vec<String>,
}

pub fn login_command(level_needed: u8) -> &'static str {
    if level_needed == 3 {
        "treeherder-cli login --level 3"
    } else {
        "treeherder-cli login"
    }
}

fn format_date(time: Timestamp) -> String {
    Offset::UTC.to_datetime(time).date().to_string()
}

/// True when less than the warning window remains before `expires_at`.
fn expires_soon(expires_at: Timestamp, now: Timestamp) -> bool {
    expires_at.duration_since(now) < EXPIRY_WARNING
}

fn scopes_for_level(level: u8) -> Vec<&'static str> {
    match level {
        3 => vec![LEVEL_1_SCOPE, LEVEL_3_SCOPE],
        _ => vec![LEVEL_1_SCOPE],
    }
}

fn login_url(
    root_url: &str,
    level: u8,
    days: u32,
    client_name: &str,
    description: &str,
    callback_url: &str,
) -> Result<Url> {
    let expires = format!("{days}d");
    let mut params: Vec<(&str, &str)> = scopes_for_level(level)
        .into_iter()
        .map(|scope| ("scope", scope))
        .collect();
    params.extend([
        ("name", client_name),
        ("expires", &expires),
        ("callback_url", callback_url),
        ("description", description),
    ]);
    Ok(Url::parse_with_params(
        &format!("{root_url}/auth/clients/create"),
        params,
    )?)
}

fn client_ui_url(client_id: &str) -> Result<String> {
    let mut url = Url::parse(TC_ROOT_URL)?;
    url.path_segments_mut()
        .map_err(|()| anyhow!("{TC_ROOT_URL} cannot be a base URL"))?
        .extend(["auth", "clients", client_id]);
    // The url crate leaves '|' unescaped in path segments; the Taskcluster UI needs it escaped.
    Ok(url.as_str().replace('|', "%7C"))
}

/// Credentials issued by another Taskcluster deployment are not used.
fn env_credentials(lookup: impl Fn(&str) -> Option<String>) -> Option<taskcluster::Credentials> {
    if lookup("TASKCLUSTER_ROOT_URL").is_some_and(|root| root.trim_end_matches('/') != TC_ROOT_URL)
    {
        return None;
    }
    let client_id = lookup("TASKCLUSTER_CLIENT_ID")?;
    let access_token = lookup("TASKCLUSTER_ACCESS_TOKEN")?;
    Some(match lookup("TASKCLUSTER_CERTIFICATE") {
        Some(certificate) => {
            taskcluster::Credentials::new_with_certificate(client_id, access_token, certificate)
        }
        None => taskcluster::Credentials::new(client_id, access_token),
    })
}

fn credentials_path() -> Result<PathBuf> {
    credentials_path_from(
        std::env::var_os("XDG_CONFIG_HOME").as_deref(),
        if cfg!(windows) {
            dirs::config_dir()
        } else {
            dirs::home_dir().map(|home| home.join(".config"))
        },
    )
}

fn credentials_path_from(
    xdg_config_home: Option<&OsStr>,
    platform_dir: Option<PathBuf>,
) -> Result<PathBuf> {
    let config_dir = xdg_config_home
        .map(PathBuf::from)
        .filter(|dir| dir.is_absolute())
        .or(platform_dir)
        .context("Cannot locate a config directory. Set XDG_CONFIG_HOME to an absolute path.")?;
    Ok(config_dir.join("treeherder-cli").join("credentials.json"))
}

fn read_stored(path: &Path) -> Result<Option<StoredCredentials>> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(err) if err.kind() == ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(err).with_context(|| format!("reading {}", path.display())),
    };
    let stored =
        serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
    Ok(Some(stored))
}

fn write_stored(path: &Path, stored: &StoredCredentials) -> Result<()> {
    write_atomic(path, &serde_json::to_vec_pretty(stored)?, true)
}

async fn wait_for_callback(
    listener: &TcpListener,
    callback_path: &str,
) -> Result<(taskcluster::Credentials, TcpStream)> {
    loop {
        let (mut stream, _) = listener.accept().await?;
        // Browsers open speculative connections that may close without sending anything.
        if let Ok(Ok(head)) =
            tokio::time::timeout(CALLBACK_IO_TIMEOUT, read_request_head(&mut stream)).await
        {
            if let Some(credentials) = parse_callback(&head, callback_path) {
                return Ok((credentials, stream));
            }
        }
        respond(&mut stream, "404 Not Found", "").await;
    }
}

async fn read_request_head(stream: &mut TcpStream) -> Result<String> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 1024];
    while !buf.windows(4).any(|window| window == b"\r\n\r\n") && buf.len() < MAX_REQUEST_HEAD {
        let n = stream.read(&mut chunk).await?;
        if n == 0 {
            bail!("Incomplete callback request");
        }
        buf.extend_from_slice(&chunk[..n]);
    }
    anyhow::ensure!(
        buf.len() <= MAX_REQUEST_HEAD && buf.windows(4).any(|window| window == b"\r\n\r\n"),
        "Invalid callback request size"
    );
    Ok(String::from_utf8(buf)?)
}

/// Ignores write errors: the browser may have gone away, and the login result does not depend on it.
async fn respond(stream: &mut TcpStream, status: &str, body: &str) {
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nCache-Control: no-store\r\nReferrer-Policy: no-referrer\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = tokio::time::timeout(CALLBACK_IO_TIMEOUT, stream.write_all(response.as_bytes())).await;
}

fn parse_callback(request_head: &str, callback_path: &str) -> Option<taskcluster::Credentials> {
    let mut parts = request_head.lines().next()?.split_whitespace();
    if parts.next()? != "GET" {
        return None;
    }
    let target = parts.next()?;
    let (path, _) = target.split_once('?')?;
    if path != callback_path {
        return None;
    }
    let url = Url::parse(&format!("http://localhost{target}")).ok()?;
    let query: HashMap<String, String> = url.query_pairs().into_owned().collect();
    Some(taskcluster::Credentials::new(
        query
            .get("clientId")
            .filter(|value| !value.is_empty())?
            .clone(),
        query
            .get("accessToken")
            .filter(|value| !value.is_empty())?
            .clone(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn query_value(url: &Url, key: &str) -> Vec<String> {
        url.query_pairs()
            .filter(|(k, _)| k == key)
            .map(|(_, v)| v.into_owned())
            .collect()
    }

    #[test]
    fn login_url_carries_scopes_name_expiry_callback_and_description() {
        let url = login_url(
            "https://tc.example",
            3,
            90,
            "treeherder-cli-abc123",
            "treeherder-cli on my host",
            "http://localhost:4321/login-nonce",
        )
        .unwrap();
        assert_eq!(url.path(), "/auth/clients/create");
        assert_eq!(
            query_value(&url, "scope"),
            vec![LEVEL_1_SCOPE, LEVEL_3_SCOPE]
        );
        assert_eq!(query_value(&url, "name"), vec!["treeherder-cli-abc123"]);
        assert_eq!(query_value(&url, "expires"), vec!["90d"]);
        assert_eq!(
            query_value(&url, "callback_url"),
            vec!["http://localhost:4321/login-nonce"]
        );
        assert_eq!(
            query_value(&url, "description"),
            vec!["treeherder-cli on my host"]
        );
        assert!(url
            .as_str()
            .contains("callback_url=http%3A%2F%2Flocalhost%3A4321"));
        assert!(!url.as_str().contains(' '));
    }

    #[test]
    fn callback_accepts_redirect_and_rejects_other_requests() {
        let head = "GET /login-nonce?clientId=project%2Fabc%7Cx&accessToken=tok123 HTTP/1.1\r\nHost: localhost:4321\r\n\r\n";
        let credentials = parse_callback(head, "/login-nonce").unwrap();
        assert_eq!(credentials.client_id, "project/abc|x");
        assert_eq!(credentials.access_token, "tok123");

        assert!(parse_callback(head, "/different-nonce").is_none());
        for request in [
            "GET /favicon.ico HTTP/1.1\r\n\r\n",
            "GET /login-nonce?clientId=only HTTP/1.1\r\n\r\n",
            "GET /login-nonce?clientId=a&accessToken= HTTP/1.1\r\n\r\n",
            "POST /login-nonce?clientId=a&accessToken=b HTTP/1.1\r\n\r\n",
            "",
        ] {
            assert!(parse_callback(request, "/login-nonce").is_none());
        }
    }

    #[tokio::test]
    async fn listener_ignores_unrelated_requests_before_the_login_callback() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let browser = async {
            let mut wrong = TcpStream::connect(addr).await.unwrap();
            wrong
                .write_all(b"GET /wrong HTTP/1.1\r\nHost: localhost\r\n\r\n")
                .await
                .unwrap();
            let mut response = String::new();
            wrong.read_to_string(&mut response).await.unwrap();
            assert!(response.starts_with("HTTP/1.1 404"));

            let mut correct = TcpStream::connect(addr).await.unwrap();
            correct
                .write_all(b"GET /login-nonce?clientId=test&accessToken=dummy HTTP/1.1\r\nHost: localhost\r\n\r\n")
                .await.unwrap();
        };
        let (result, ()) = tokio::time::timeout(Duration::from_secs(10), async {
            tokio::join!(wait_for_callback(&listener, "/login-nonce"), browser)
        })
        .await
        .unwrap();
        let (credentials, _) = result.unwrap();
        assert_eq!(credentials.client_id, "test");
    }

    #[test]
    fn scopes_by_level() {
        assert_eq!(scopes_for_level(1), vec![LEVEL_1_SCOPE]);
        assert_eq!(scopes_for_level(3), vec![LEVEL_1_SCOPE, LEVEL_3_SCOPE]);
    }

    #[test]
    fn credentials_path_resolution() {
        let base = tempfile::tempdir().unwrap();
        let xdg = base.path().join("xdg");
        let platform = base.path().join("native");
        assert_eq!(
            credentials_path_from(Some(xdg.as_os_str()), Some(platform.clone())).unwrap(),
            xdg.join("treeherder-cli").join("credentials.json")
        );
        assert_eq!(
            credentials_path_from(Some(OsStr::new("relative")), Some(platform.clone())).unwrap(),
            platform.join("treeherder-cli").join("credentials.json")
        );
        assert_eq!(
            credentials_path_from(None, Some(platform.clone())).unwrap(),
            platform.join("treeherder-cli").join("credentials.json")
        );
        assert!(credentials_path_from(None, None).is_err());
    }

    #[test]
    fn env_credentials_need_both_and_take_optional_certificate() {
        let vars = |pairs: &[(&str, &str)]| {
            let map: HashMap<String, String> = pairs
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect();
            move |name: &str| map.get(name).cloned()
        };
        assert!(env_credentials(vars(&[("TASKCLUSTER_CLIENT_ID", "a")])).is_none());
        let creds = env_credentials(vars(&[
            ("TASKCLUSTER_CLIENT_ID", "a"),
            ("TASKCLUSTER_ACCESS_TOKEN", "t"),
        ]))
        .unwrap();
        assert_eq!(creds.client_id, "a");
        assert_eq!(creds.certificate, None);
        let creds = env_credentials(vars(&[
            ("TASKCLUSTER_CLIENT_ID", "a"),
            ("TASKCLUSTER_ACCESS_TOKEN", "t"),
            ("TASKCLUSTER_CERTIFICATE", "{}"),
        ]))
        .unwrap();
        assert_eq!(creds.certificate.as_deref(), Some("{}"));
        let with_root = |root: &str| {
            env_credentials(vars(&[
                ("TASKCLUSTER_CLIENT_ID", "a"),
                ("TASKCLUSTER_ACCESS_TOKEN", "t"),
                ("TASKCLUSTER_ROOT_URL", root),
            ]))
        };
        assert!(with_root(&format!("{TC_ROOT_URL}/")).is_some());
        assert!(with_root("https://community-tc.services.mozilla.com").is_none());
    }

    #[test]
    fn stored_credentials_round_trip() {
        let stored = StoredCredentials {
            client_id: "treeherder-cli-abc123".to_string(),
            access_token: "tok".to_string(),
            level: 3,
            expires_at: "2027-01-01T00:00:00.123Z".parse().unwrap(),
        };
        let json = serde_json::to_string(&stored).unwrap();
        assert!(json.contains("\"expires_at\":\"2027-01-01T00:00:00.123Z\""));
        let parsed: StoredCredentials = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.client_id, stored.client_id);
        assert_eq!(parsed.access_token, stored.access_token);
        assert_eq!(parsed.level, stored.level);
        assert_eq!(parsed.expires_at, stored.expires_at);
    }

    #[test]
    fn expiry_warning_threshold() {
        let now = "2027-01-01T00:00:00Z".parse().unwrap();
        assert!(expires_soon(
            now + SignedDuration::from_hours(6 * HOURS_PER_DAY),
            now
        ));
        assert!(!expires_soon(
            now + SignedDuration::from_hours(8 * HOURS_PER_DAY),
            now
        ));
    }

    #[test]
    fn client_ui_url_encodes_client_id() {
        let url = client_ui_url("project-gecko/a|b").unwrap();
        assert_eq!(
            url.as_str(),
            "https://firefox-ci-tc.services.mozilla.com/auth/clients/project-gecko%2Fa%7Cb"
        );
    }
}
