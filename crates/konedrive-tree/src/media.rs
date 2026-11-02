//! OneDrive's allowed media facts, unconverted, for the version-1 desktop xattr.
//! No location, codec, bitrate or audio values enter this representation.
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MediaKind { Photo, Video }

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaFacts {
    pub v: u8,
    pub kind: MediaKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub height: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub orientation: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub camera_make: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub camera_model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub taken_date_time: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub f_number: Option<serde_json::Number>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exposure_numerator: Option<serde_json::Number>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exposure_denominator: Option<serde_json::Number>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub focal_length: Option<serde_json::Number>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub iso: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frame_rate: Option<serde_json::Number>,
}

impl MediaFacts {
    /// An existing facet with no known fields still establishes its kind.
    pub fn empty(kind: MediaKind) -> Self {
        Self { v: 1, kind, width: None, height: None, orientation: None, camera_make: None, camera_model: None, taken_date_time: None, f_number: None, exposure_numerator: None, exposure_denominator: None, focal_length: None, iso: None, duration_ms: None, frame_rate: None }
    }
}
