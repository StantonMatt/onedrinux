//! Only the photo/video fields used by desktop metadata. Unknown keys (including
//! location) are discarded; malformed facets and fields never reject a drive item.
use serde::{de::DeserializeOwned, Deserialize, Deserializer};

pub(super) fn lenient<'de, D: Deserializer<'de>, T: DeserializeOwned>(deserializer: D) -> Result<Option<T>, D::Error> {
    let value = serde_json::Value::deserialize(deserializer)?;
    Ok(serde_json::from_value(value).ok())
}

/// Serde structs also accept JSON sequences; Graph facets must be objects.
pub(super) fn facet<'de, D: Deserializer<'de>, T: DeserializeOwned>(deserializer: D) -> Result<Option<T>, D::Error> {
    let value = serde_json::Value::deserialize(deserializer)?;
    Ok(value.is_object().then(|| serde_json::from_value(value).ok()).flatten())
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImageFacet {
    #[serde(default, deserialize_with = "lenient")]
    pub width: Option<u64>,
    #[serde(default, deserialize_with = "lenient")]
    pub height: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PhotoFacet {
    #[serde(default, deserialize_with = "lenient")]
    pub camera_make: Option<String>,
    #[serde(default, deserialize_with = "lenient")]
    pub camera_model: Option<String>,
    #[serde(default, deserialize_with = "lenient")]
    pub taken_date_time: Option<String>,
    #[serde(default, deserialize_with = "lenient")]
    pub f_number: Option<f64>,
    #[serde(default, deserialize_with = "lenient")]
    pub exposure_numerator: Option<f64>,
    #[serde(default, deserialize_with = "lenient")]
    pub exposure_denominator: Option<f64>,
    #[serde(default, deserialize_with = "lenient")]
    pub focal_length: Option<f64>,
    #[serde(default, deserialize_with = "lenient")]
    pub iso: Option<u64>,
    #[serde(default, deserialize_with = "lenient")]
    pub orientation: Option<u8>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VideoFacet {
    #[serde(default, deserialize_with = "lenient")]
    pub width: Option<u64>,
    #[serde(default, deserialize_with = "lenient")]
    pub height: Option<u64>,
    #[serde(default, deserialize_with = "lenient")]
    pub duration: Option<u64>,
    #[serde(default, deserialize_with = "lenient")]
    pub frame_rate: Option<f64>,
}

