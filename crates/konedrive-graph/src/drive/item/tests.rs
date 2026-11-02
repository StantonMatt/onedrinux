use super::*;

#[test]
fn graph_times_are_unix_seconds() {
    assert_eq!(parse_graph_time("1970-01-01T00:00:00Z"), Some(0));
    assert_eq!(parse_graph_time("2024-05-01T10:00:00Z"), Some(1_714_557_600));
    assert_eq!(parse_graph_time("2024-05-01T10:00:00.123Z"), Some(1_714_557_600), "fractions are dropped");
    assert_eq!(parse_graph_time("2000-02-29T23:59:59Z"), Some(951_868_799));
    assert_eq!(parse_graph_time("1969-12-31T23:59:59Z"), Some(-1));
}

#[test]
fn unix_seconds_format_back_to_graph_times() {
    for (seconds, text) in [
        (0, "1970-01-01T00:00:00Z"),
        (1_714_557_600, "2024-05-01T10:00:00Z"),
        (951_868_799, "2000-02-29T23:59:59Z"),
        (-1, "1969-12-31T23:59:59Z"),
    ] {
        assert_eq!(format_graph_time(seconds), text);
        assert_eq!(parse_graph_time(text), Some(seconds));
    }
}

#[test]
fn anything_else_is_not_a_graph_time() {
    for bad in ["", "2024-05-01", "2024-05-01T10:00:00", "2024-13-01T00:00:00Z", "2024-05-01T25:00:00Z", "x-05-01T10:00:00Z"] {
        assert_eq!(parse_graph_time(bad), None, "{bad:?}");
    }
}

#[test]
fn a_delta_item_deserializes_with_every_facet_it_may_carry() {
    let item: DriveItem = serde_json::from_value(serde_json::json!({
        "id": "I", "name": "n", "size": 3, "eTag": "e", "cTag": "c",
        "parentReference": {"id": "P", "driveId": "D"},
        "file": {"mimeType": "image/jpeg", "hashes": {"quickXorHash": "q"}},
        "fileSystemInfo": {"lastModifiedDateTime": "2024-05-01T10:00:00Z"},
        "specialFolder": {"name": "vault"}, "remoteItem": {}, "package": {"type": "oneNote"},
        "deleted": {"state": "deleted"}, "root": {}, "folder": {"childCount": 0}
    }))
    .unwrap();
    assert_eq!(item.parent_reference.as_ref().and_then(|p| p.id.as_deref()), Some("P"));
    assert_eq!(item.file.as_ref().and_then(|f| f.hashes.as_ref()).and_then(|h| h.quick_xor_hash.as_deref()), Some("q"));
    assert_eq!(item.special_folder.as_ref().and_then(|s| s.name.as_deref()), Some("vault"));
    assert!(item.remote_item.is_some() && item.package.is_some() && item.deleted.is_some() && item.root.is_some());
}

#[test]
fn media_facets_parse_allowed_fields_and_drop_odd_values() {
    let item: DriveItem = serde_json::from_value(serde_json::json!({
        "id":"F", "image":{"width":3200,"height":2400,"extra":true},
        "photo":{"cameraMake":"Example","cameraModel":"Example 2","takenDateTime":"2024-02-03T04:05:06.12Z",
            "fNumber":2.8,"exposureNumerator":1.0,"exposureDenominator":125.0,"focalLength":7.5,"iso":200,"orientation":6},
        "video":{"width":1080,"height":1920,"duration":12345,"frameRate":30.0,"fourCC":"TEST","bitrate":123},
        "location":{"latitude":1.0,"longitude":2.0}
    })).unwrap();
    assert_eq!(item.image.unwrap(), ImageFacet { width:Some(3200), height:Some(2400) });
    let photo = item.photo.unwrap();
    assert_eq!((photo.camera_make.as_deref(), photo.camera_model.as_deref(), photo.taken_date_time.as_deref()), (Some("Example"), Some("Example 2"), Some("2024-02-03T04:05:06.12Z")));
    assert_eq!((photo.f_number, photo.exposure_numerator, photo.exposure_denominator, photo.focal_length, photo.iso, photo.orientation), (Some(2.8), Some(1.0), Some(125.0), Some(7.5), Some(200), Some(6)));
    assert_eq!(item.video.unwrap(), VideoFacet { width:Some(1080), height:Some(1920), duration:Some(12345), frame_rate:Some(30.0) });
    let odd: DriveItem = serde_json::from_value(serde_json::json!({
        "id":"F", "image":{"width":"bad","height":25},
        "photo":{"cameraMake":[],"cameraModel":12,"fNumber":"2.8","exposureNumerator":{},"exposureDenominator":null,"focalLength":false,"iso":-1,"orientation":999,"takenDateTime":{}},
        "video":{"width":-2,"height":1.5,"duration":"soon","frameRate":[]}
    })).unwrap();
    assert_eq!(odd.image.unwrap(), ImageFacet { width:None, height:Some(25) });
    assert_eq!(odd.photo.unwrap(), PhotoFacet { camera_make:None, camera_model:None, taken_date_time:None, f_number:None, exposure_numerator:None, exposure_denominator:None, focal_length:None, iso:None, orientation:None });
    assert_eq!(odd.video.unwrap(), VideoFacet { width:None,height:None,duration:None,frame_rate:None });
    for facet in [serde_json::json!(null), serde_json::json!(false), serde_json::json!(8), serde_json::json!([]), serde_json::json!("bad")] {
        let odd: DriveItem = serde_json::from_value(serde_json::json!({"id":"F","image":facet,"photo":facet,"video":facet})).unwrap();
        assert!(odd.image.is_none() && odd.photo.is_none() && odd.video.is_none());
    }
}
