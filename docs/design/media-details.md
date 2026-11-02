# Media details attribute contract

The daemon and the KFileMetaData plugin share this contract. For the desktop behavior, see
[desktop.md §11.3](desktop.md#113-media-details). Only stored photo/video details are used;
GPS is neither stored nor served.

## Source (Graph delta, no $select)

- `image`: `width`, `height` (present on every image, including PNG screenshots).
- `photo`: `cameraMake`, `cameraModel`, `takenDateTime` (string like `2022-01-08T17:04:08Z` or `...08.61Z`), `fNumber`, `exposureNumerator`, `exposureDenominator`, `focalLength`, `iso`, `orientation` (1–8). Any of them may be missing.
- `video`: `width`, `height`, `duration` (ms), `frameRate` (often missing). Videos also carry a `photo` facet: IGNORE it for videos.
- Ignore `location`, `bitrate`, `fourCC`, audio fields: never store them.

## Stored and served

The daemon stores only the fields above (per item, bound to the content version/eTag that delivered them; removed when a later delta drops all three facets or the item is deleted/replaced) and serves them on the FUSE mount as the xattr `user.konedrive.media`, value = compact UTF-8 JSON object, keys omitted when unknown:

```json
{"v":1,"kind":"photo","width":4608,"height":2112,"orientation":6,"cameraMake":"OnePlus","cameraModel":"ONEPLUS A6013","takenDateTime":"2020-01-29T18:05:59.61Z","fNumber":1.7,"exposureNumerator":1.0,"exposureDenominator":25.0,"focalLength":4.25,"iso":500}
{"v":1,"kind":"video","width":1080,"height":1920,"durationMs":2323,"frameRate":30.0}
```

- `kind`: `"video"` if the item has a `video` facet; otherwise `"photo"` if it has an `image` or `photo` facet. Values are OneDrive's, unconverted (the daemon does NOT swap or reformat; the plugin applies Dolphin's semantics).
- Served ONLY while the file's content is NOT on disk (the mount's own content-presence decision, `ContentPresence::NotOnDisk` in crates/konedrived/src/mount/item_read_origin.rs / item_kernel_requests.rs — the same decision that sends a background reader's open to `check_cloud_open`). Whenever the content is on disk (synced, modified here, kept, landed — even if `user.konedrive.state` shows `hydrating` because of range fetches or progress linger), return ENODATA, because Dolphin's built-in extractors can read the file then and two answers would merge. The `user.konedrive.state` attribute is NOT a content-presence bit and must not be used for this.
- Shown and listed content may differ only in modification time; a time-only change leaves the shown version unchanged. The stored facts still require an exact match to the listing's binding, eTag and content. Facts follow that unchanged remote binding transactionally when it moves to another local row.
- ENODATA when nothing is stored. Only for files (never folders). Not listed by `listxattr` (like the other `user.konedrive.*` names today), so `cp -a`/`rsync -X` never copy it. Answered from the store only: no network, no content read, no guard check.

## Plugin mapping (KFileMetaData properties)

Answer whenever `user.konedrive.media` is present and valid (the daemon serves it only while the content is not on disk, so the plugin does NOT read or check `user.konedrive.state`). If the attribute is missing/unsupported (any non-OneDrive file, or a file on disk), return immediately and add nothing. A hydration finishing between the plugin's getxattr and a built-in extractor's open can merge values once in that render; accepted.

- photo: `Width`/`Height` = stored size: swap OneDrive's width/height when orientation is 5, 6, 7 or 8 (OneDrive reports them already turned; Dolphin shows the stored size plus the orientation row). `ImageOrientation` (int) if present. `Manufacturer`, `Model` (strings). `PhotoFNumber`, `PhotoFocalLength` (double). `PhotoExposureTime` = numerator/denominator (double, only when both present, denominator > 0 and the quotient is finite). `PhotoISOSpeedRatings` (int). `PhotoDateTimeOriginal` = takenDateTime read as the camera's clock: drop the trailing `Z` and any fraction, parse as LOCAL time (Qt::LocalTime), do not convert from UTC. Do NOT add `ImageDateTime`. `addType(Type::Image)`.
- video: `Width`/`Height` as given (display size; accepted that sideways-recorded phone videos show the two numbers swapped once downloaded, since OneDrive has no rotation field). `Duration` = durationMs / 1000 (integer seconds, truncated and range-checked). `FrameRate` (double) if present. `addType(Type::Video)`. Nothing else.
- Unknown `v` or unparsable JSON: add nothing.
