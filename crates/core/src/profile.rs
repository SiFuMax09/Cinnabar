use crate::utils::HyphenatedUUID;
use anyhow::Result;
use cinnabar_network::packets::PlayerProperty;
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};
use std::time::Duration;

#[derive(Debug, Deserialize)]
pub struct PlayerProfile {
    #[serde(rename = "id")]
    pub uuid: HyphenatedUUID,
    #[serde(rename = "name")]
    pub username: String,
}

impl PlayerProfile {
    pub async fn lookup_by_username(username: &str) -> Result<PlayerProfile> {
        let url = format!(
            "https://api.mojang.com/users/profiles/minecraft/{}",
            username
        );
        let client = reqwest::Client::new();
        let res = client
            .get(url)
            .send()
            .await?
            .json::<PlayerProfile>()
            .await?;
        Ok(res)
    }
}

#[derive(Deserialize)]
struct SessionProperty {
    name: String,
    value: String,
    signature: Option<String>,
}

#[derive(Deserialize)]
struct SessionProfile {
    properties: Vec<SessionProperty>,
}

/// Skin properties by lowercase username. A failed lookup is cached as an empty list, so a player
/// without a Mojang account doesn't delay every login.
static SKIN_CACHE: LazyLock<Mutex<HashMap<String, Vec<PlayerProperty>>>> =
    LazyLock::new(Default::default);

/// How long a login may wait for Mojang's servers before the default skin is used
const SKIN_LOOKUP_TIMEOUT: Duration = Duration::from_secs(2);

async fn fetch_skin_properties(username: &str) -> Result<Vec<PlayerProperty>> {
    let profile = PlayerProfile::lookup_by_username(username).await?;
    let url = format!(
        "https://sessionserver.mojang.com/session/minecraft/profile/{:032x}?unsigned=false",
        profile.uuid.0
    );
    let session = reqwest::Client::new()
        .get(url)
        .send()
        .await?
        .error_for_status()?
        .json::<SessionProfile>()
        .await?;
    Ok(session
        .properties
        .into_iter()
        .map(|prop| PlayerProperty {
            name: prop.name,
            value: prop.value,
            signature: prop.signature,
        })
        .collect())
}

/// Looks up the signed skin textures of a Mojang account. This blocks for at most
/// `SKIN_LOOKUP_TIMEOUT`, and returns no properties (the default skin) if the lookup fails.
pub fn skin_properties(username: &str) -> Vec<PlayerProperty> {
    let key = username.to_lowercase();
    if let Some(cached) = SKIN_CACHE.lock().unwrap().get(&key) {
        return cached.clone();
    }

    let result = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(anyhow::Error::from)
        .and_then(|rt| {
            rt.block_on(async {
                tokio::time::timeout(SKIN_LOOKUP_TIMEOUT, fetch_skin_properties(username))
                    .await
                    .map_err(anyhow::Error::from)?
            })
        });
    let properties = match result {
        Ok(properties) => properties,
        Err(err) => {
            tracing::debug!("Could not look up skin of {username}: {err}");
            Vec::new()
        }
    };
    SKIN_CACHE.lock().unwrap().insert(key, properties.clone());
    properties
}
