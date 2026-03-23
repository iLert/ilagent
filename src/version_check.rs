use log::warn;

const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");
const LATEST_RELEASE_URL: &str = "https://github.com/iLert/ilagent/releases/latest";

pub fn spawn_version_check() {
    tokio::spawn(async {
        if let Err(e) = check_latest_version().await {
            log::debug!("Version check failed: {}", e);
        }
    });
}

async fn check_latest_version() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(std::time::Duration::from_secs(5))
        .build()?;

    let resp = client.head(LATEST_RELEASE_URL).send().await?;

    let location = resp
        .headers()
        .get("location")
        .and_then(|v| v.to_str().ok())
        .ok_or("no location header")?;

    let latest = location
        .rsplit('/')
        .next()
        .ok_or("could not parse version from location")?;

    if latest != CURRENT_VERSION {
        warn!(
            "A new version of ilagent is available: {} (current: {})",
            latest, CURRENT_VERSION
        );
    }

    Ok(())
}
