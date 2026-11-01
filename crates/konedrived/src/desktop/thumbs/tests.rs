use std::os::unix::fs::PermissionsExt;
use std::sync::Arc;
use std::time::Duration;

use url::Url;
use wiremock::matchers::{method, path, path_regex};
use konedrive_graph::test_server::MockServer;
use wiremock::{Mock, ResponseTemplate};

use super::*;
use crate::mount::item_fixture::{onedrive_id_of, ItemFixture, ONEDRIVE_MTIME};
use konedrive_graph::token::StaticToken;

fn jpeg(width: u32, height: u32) -> Vec<u8> {
    let image = image::RgbImage::from_pixel(width, height, image::Rgb([200, 30, 30]));
    let mut out = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgb8(image).write_to(&mut out, image::ImageFormat::Jpeg).unwrap();
    out.into_inner()
}

/// A folder's rows with the files `names` shown, each a file OneDrive has with the MIME type
/// given, a fake Graph and an empty thumbnail cache.
struct World {
    server: MockServer,
    fixture: ItemFixture,
    cache: tempfile::TempDir,
    state: SyncStateHandle,
}

async fn world(files: &[(&str, &str)]) -> World {
    let fixture = ItemFixture::new();
    for (name, mime) in files {
        let shown = fixture.show_onedrive_file(name, name.as_bytes());
        let (item, mime) = (shown.local_item_id.as_i64(), (*mime).to_owned());
        // The listing's row carries no MIME type in the fixture: it is written as a listing would have.
        fixture.write_rows(move |store| store.bench_sql(&format!("UPDATE items SET onedrive_mime = '{mime}' WHERE local_item_id = {item}")));
    }
    World { server: MockServer::start().await, fixture, cache: tempfile::tempdir().unwrap(), state: SyncStateHandle::new(Default::default()) }
}

fn thumb_path(name: &str, size: &str) -> String {
    let mut url = Url::parse("http://fake/").unwrap();
    url.path_segments_mut().unwrap().extend(["me", "drive", "items", &onedrive_id_of(name), "thumbnails", "0", size, "content"]);
    url.path().to_owned()
}

impl World {
    fn root(&self) -> std::path::PathBuf {
        self.fixture.folder().canonicalize().unwrap()
    }

    fn filler(&self) -> ThumbnailFiller {
        // Short throttle waits, so a `503` is given up on in milliseconds.
        let retry = konedrive_graph::drive::RetryPolicy { attempts: 2, default_wait: Duration::from_millis(10), max_wait: Duration::from_millis(50) };
        let drive = konedrive_graph::drive::DriveClient::new(Url::parse(&format!("{}/", self.server.uri())).unwrap(), Arc::new(StaticToken::new("T"))).unwrap().with_retry(retry);
        let root = SyncRoot { path: self.root(), root_id: "r".into() };
        ThumbnailFiller::new(drive, self.fixture.store.clone(), root, self.cache.path().to_path_buf(), Arc::default(), self.state.clone())
    }

    fn cached(&self, dir: &str, name: &str) -> std::path::PathBuf {
        self.cache.path().join(dir).join(thumbnail_name(&self.root().join(name)))
    }

    fn item(&self, name: &str) -> LocalItemId {
        self.fixture.find_item_at(name).expect("the row").local_item_id
    }
}

#[test]
fn a_file_uri_escapes_what_a_path_segment_cannot_hold() {
    assert_eq!(file_uri(std::path::Path::new("/home/u/OneDrive/plain.jpg")), "file:///home/u/OneDrive/plain.jpg");
    assert_eq!(file_uri(std::path::Path::new("/h/with space.jpg")), "file:///h/with%20space.jpg");
    assert_eq!(file_uri(std::path::Path::new("/h/фото.jpg")), "file:///h/%D1%84%D0%BE%D1%82%D0%BE.jpg");
    assert_eq!(file_uri(std::path::Path::new("/h/sym (1)+&,;=@:!$'*.jpg")), "file:///h/sym%20(1)+&,;=@:!$'*.jpg");
    assert_eq!(file_uri(std::path::Path::new("/h/100%.jpg")), "file:///h/100%25.jpg");
}

/// Expected spellings captured by the standalone Qt 6.10.2 probe of Gwenview 25.12's
/// exact expression, `url.adjusted(QUrl::RemovePassword).url()` (docs/kio-behavior.md §A).
#[test]
fn a_gwenview_uri_matches_qt_pretty_decoded() {
    let cases = [
        ("/h/plain.jpg", "file:///h/plain.jpg"),
        ("/h/with space.jpg", "file:///h/with space.jpg"),
        ("/h/фото.jpg", "file:///h/фото.jpg"),
        ("/h/😀.jpg", "file:///h/😀.jpg"),
        ("/h/100%.jpg", "file:///h/100%25.jpg"),
        ("/h/#.jpg", "file:///h/%23.jpg"),
        ("/h/?.jpg", "file:///h/%3F.jpg"),
        ("/h/[ ].jpg", "file:///h/%5B %5D.jpg"),
        ("/h/{ }.jpg", "file:///h/%7B %7D.jpg"),
        ("/h/^.jpg", "file:///h/%5E.jpg"),
        ("/h/`.jpg", "file:///h/%60.jpg"),
        ("/h/|.jpg", "file:///h/%7C.jpg"),
        ("/h/\".jpg", "file:///h/%22.jpg"),
        ("/h/<>.jpg", "file:///h/%3C%3E.jpg"),
        ("/h/\\.jpg", "file:///h/%5C.jpg"),
        ("/h/+&,;=@:!$'*().jpg", "file:///h/+&,;=@:!$'*().jpg"),
        ("/h/tab\t.jpg", "file:///h/tab%09.jpg"),
    ];
    for (path, expected) in cases {
        assert_eq!(gwenview_uri(Path::new(path)).as_deref(), Some(expected), "{path:?}");
    }
    // Qt emits file:///h/bad�.jpg for these bytes, losing their identity. We skip it.
    let invalid = Path::new(std::ffi::OsStr::from_bytes(b"/h/bad\xff.jpg"));
    assert_eq!(gwenview_uri(invalid), None);
    assert_eq!(thumbnail_uris(invalid), ["file:///h/bad%FF.jpg"]);
}

#[test]
fn spaces_and_unicode_get_both_cache_names_and_plain_names_get_one() {
    let cache = tempfile::tempdir().unwrap();
    for file in ["/folder/with space.jpg", "/folder/фото😀.jpg", "/folder/plain.jpg"] {
        let file = Path::new(file);
        write_thumbnail(cache.path(), file, 123, 456, &jpeg(600, 400)).unwrap();
        let uris = thumbnail_uris(file);
        assert_eq!(uris.len(), if file.ends_with("plain.jpg") { 1 } else { 2 });
        for uri in uris {
            for (dir, edge) in SIZES {
                let path = cache.path().join(dir).join(name_for_uri(&uri));
                assert!(cached_png(&path, &uri, 123, 456).unwrap().is_some(), "own URI and mtime: {uri}");
                assert!(cached_png(&path, &uri, 124, 456).unwrap().is_none());
                let image = image::open(path).unwrap();
                assert!(image.width().max(image.height()) <= *edge);
            }
        }
    }
    for (dir, _) in SIZES {
        assert_eq!(std::fs::read_dir(cache.path().join(dir)).unwrap().count(), 5, "two names + two names + one; no temporary files");
    }
    // A new cloud version can retain mtime and size: refresh both of our names anyway.
    let file = Path::new("/folder/with space.jpg");
    write_thumbnail(cache.path(), file, 123, 456, &jpeg(32, 32)).unwrap();
    for uri in thumbnail_uris(file) {
        for (dir, _) in SIZES {
            assert_eq!(image::open(cache.path().join(dir).join(name_for_uri(&uri))).unwrap().width(), 32);
        }
    }
}

/// Seed the old cache and store without ever asking the fake Graph for an image.
fn seed_thumbnail(w: &World, name: &str) {
    let row = w.fixture.find_item_at(name).unwrap();
    let content = row.onedrive.unwrap().content.unwrap();
    write_thumbnail(w.cache.path(), &w.root().join(name), content.mtime().0, content.size(), &jpeg(64, 64)).unwrap();
    let path = PathBuf::from(name);
    w.fixture.write_rows(move |store| store.item_store().set_thumbnail_made(row.local_item_id, &content, &path, true));
}

#[tokio::test]
async fn the_first_drain_upgrades_old_names_in_batches_without_graph_or_forgetting_marks() {
    let names = ["a space.jpg", "b space.jpg", "фото😀.jpg"];
    let w = world(&names.map(|name| (name, "image/jpeg"))).await;
    for name in names {
        seed_thumbnail(&w, name);
        let uri = gwenview_uri(&w.root().join(name)).unwrap();
        for (dir, _) in SIZES {
            std::fs::remove_file(w.cache.path().join(dir).join(name_for_uri(&uri))).unwrap();
        }
    }
    let stop = CancellationToken::new();
    // The first wake is a nudge; spawn must still select SyncStarted and upgrade.
    let kick = Arc::new(Notify::new());
    let task = w.filler().spawn(kick.clone(), stop.clone());
    kick.notify_one();
    let last = name_for_uri(&gwenview_uri(&w.root().join(names[2])).unwrap());
    let upgraded = tokio::time::timeout(Duration::from_secs(10), async {
        while !w.cache.path().join("x-large").join(last.clone()).is_file() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }).await;
    stop.cancel();
    task.await.unwrap();
    upgraded.expect("first drain repairs all aliases");
    // Repeat with batches of one, to cover pagination independently of spawn's 200.
    for name in names {
        let uri = gwenview_uri(&w.root().join(name)).unwrap();
        std::fs::remove_file(w.cache.path().join("large").join(name_for_uri(&uri))).unwrap();
    }
    assert_eq!(w.filler().drain(&CancellationToken::new(), 1, DrainCause::SyncStarted).await, RunOutcome::default());
    for name in names {
        assert_eq!(find_cached_sizes(w.cache.path(), &w.root().join(name), ONEDRIVE_MTIME, name.len() as u64).unwrap().len(), 3);
    }
    assert_eq!(w.fixture.read_rows(|reader| reader.list_thumbnail_marks(None, 10)).len(), 3, "all marks kept");
    assert!(w.server.received_requests().await.unwrap().is_empty(), "no Graph requests, including failed attempts");
}

#[tokio::test]
async fn either_names_missing_size_is_repaired_but_a_size_missing_from_both_is_fetched() {
    let name = "a space.jpg";
    let w = world(&[(name, "image/jpeg")]).await;
    seed_thumbnail(&w, name);
    let uris = thumbnail_uris(&w.root().join(name));
    for uri in &uris {
        for (dir, _) in SIZES {
            let path = w.cache.path().join(dir).join(name_for_uri(uri));
            std::fs::remove_file(&path).unwrap();
            assert_eq!(w.filler().drain(&CancellationToken::new(), 1, DrainCause::TimerFired).await, RunOutcome::default());
            assert!(cached_png(&path, uri, ONEDRIVE_MTIME, name.len() as u64).unwrap().is_some(), "{uri} {dir}");
        }
    }
    assert!(w.server.received_requests().await.unwrap().is_empty());
    for uri in uris {
        std::fs::remove_file(w.cache.path().join("large").join(name_for_uri(&uri))).unwrap();
    }
    assert_eq!(decide_cached_thumbnail(&find_cached_sizes(w.cache.path(), &w.root().join(name), ONEDRIVE_MTIME, name.len() as u64).unwrap()), CachedThumbnail::Missing);
    Mock::given(method("GET")).and(path(thumb_path(name, "c512x512")))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(jpeg(64, 64))).expect(1).mount(&w.server).await;
    assert_eq!(w.filler().drain(&CancellationToken::new(), 1, DrainCause::TimerFired).await.written, 1);
    assert_eq!(w.server.received_requests().await.unwrap().len(), 1);
}

fn other_viewers_png(path: &Path, uri: &str, mtime: i64, size: u64) {
    let mut encoder = png::Encoder::new(std::fs::File::create(path).unwrap(), 1, 1);
    encoder.set_color(png::ColorType::Rgb);
    encoder.set_depth(png::BitDepth::Eight);
    for (key, value) in [("Thumb::URI", uri.to_owned()), ("Thumb::MTime", mtime.to_string()), ("Thumb::Size", size.to_string()), ("Software", "Gwenview".into())] {
        encoder.add_text_chunk(key.into(), value).unwrap();
    }
    let mut writer = encoder.write_header().unwrap();
    writer.write_image_data(&[0, 255, 0]).unwrap();
    writer.finish().unwrap();
}

#[tokio::test]
async fn current_gwenview_pngs_are_left_alone_and_stale_ones_are_replaced_locally() {
    let name = "a space.jpg";
    let w = world(&[(name, "image/jpeg")]).await;
    seed_thumbnail(&w, name);
    let uri = gwenview_uri(&w.root().join(name)).unwrap();
    let path = w.cache.path().join("large").join(name_for_uri(&uri));
    other_viewers_png(&path, &uri, ONEDRIVE_MTIME, name.len() as u64);
    let original = std::fs::read(&path).unwrap();
    assert_eq!(w.filler().drain(&CancellationToken::new(), 1, DrainCause::SyncStarted).await, RunOutcome::default());
    assert_eq!(std::fs::read(&path).unwrap(), original);
    // A later Graph write must preserve another viewer's current PNG too.
    write_thumbnail(w.cache.path(), &w.root().join(name), ONEDRIVE_MTIME, name.len() as u64, &jpeg(64, 64)).unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), original);
    for (tag_uri, mtime, size) in [(uri.as_str(), ONEDRIVE_MTIME - 1, 0), ("wrong-uri", ONEDRIVE_MTIME, 0), (uri.as_str(), ONEDRIVE_MTIME, 999)] {
        other_viewers_png(&path, tag_uri, mtime, size);
        assert_eq!(w.filler().drain(&CancellationToken::new(), 1, DrainCause::TimerFired).await, RunOutcome::default());
        assert_ne!(std::fs::read(&path).unwrap(), original);
        assert!(cached_png(&path, &uri, ONEDRIVE_MTIME, name.len() as u64).unwrap().is_some());
    }
    assert!(w.server.received_requests().await.unwrap().is_empty());
}

/// Compressed tags are read, but one that inflates past the bound is no tag: the PNG is
/// not current, and reading it does not inflate it in full.
#[test]
fn a_compressed_tag_is_read_only_up_to_a_bound() {
    let dir = tempfile::tempdir().unwrap();
    let png_with = |path: &Path, uri: String, compressed_itxt: bool| {
        let mut encoder = png::Encoder::new(std::fs::File::create(path).unwrap(), 1, 1);
        encoder.set_color(png::ColorType::Rgb);
        encoder.set_depth(png::BitDepth::Eight);
        if !compressed_itxt {
            encoder.add_ztxt_chunk("Thumb::URI".into(), uri.clone()).unwrap();
        }
        encoder.add_text_chunk("Thumb::MTime".into(), "123".into()).unwrap();
        let mut writer = encoder.write_header().unwrap();
        if compressed_itxt {
            let mut chunk = png::text_metadata::ITXtChunk::new("Thumb::URI", uri);
            chunk.compressed = true;
            writer.write_text_chunk(&chunk).unwrap();
        }
        writer.write_image_data(&[0, 255, 0]).unwrap();
        writer.finish().unwrap();
    };
    for compressed_itxt in [false, true] {
        let path = dir.path().join("t.png");
        png_with(&path, "file:///h/a b.jpg".into(), compressed_itxt);
        assert!(cached_png(&path, "file:///h/a b.jpg", 123, 0).unwrap().is_some(), "itxt {compressed_itxt}");
        let huge = format!("file:///h/{}", "a".repeat(4 * 1024 * 1024));
        png_with(&path, huge.clone(), compressed_itxt);
        assert!(std::fs::metadata(&path).unwrap().len() < 64 * 1024, "the tag compresses well");
        assert!(cached_png(&path, &huge, 123, 0).unwrap().is_none(), "itxt {compressed_itxt}");
    }
}

#[test]
fn a_current_gwenview_png_is_kept_when_both_viewers_share_the_name() {
    let cache = tempfile::tempdir().unwrap();
    let file = Path::new("/folder/plain.jpg");
    write_thumbnail(cache.path(), file, 123, 456, &jpeg(64, 64)).unwrap();
    let path = cache.path().join("large").join(thumbnail_name(file));
    other_viewers_png(&path, &file_uri(file), 123, 456);
    let original = std::fs::read(&path).unwrap();
    write_thumbnail(cache.path(), file, 123, 456, &jpeg(64, 64)).unwrap();
    assert_eq!(std::fs::read(path).unwrap(), original);
}

#[tokio::test]
async fn a_failed_or_cancelled_local_repair_keeps_its_mark_without_graph() {
    let name = "a space.jpg";
    let w = world(&[(name, "image/jpeg")]).await;
    seed_thumbnail(&w, name);
    let uri = gwenview_uri(&w.root().join(name)).unwrap();
    let path = w.cache.path().join("normal").join(name_for_uri(&uri));
    std::fs::remove_file(&path).unwrap();
    let cancelled = CancellationToken::new();
    cancelled.cancel();
    assert_eq!(w.filler().drain(&cancelled, 1, DrainCause::SyncStarted).await, RunOutcome::default());
    assert!(!path.exists(), "cancelled scan writes nothing");

    // A directory at the destination makes the atomic rename fail, even as root.
    std::fs::create_dir(&path).unwrap();
    assert_eq!(w.filler().drain(&CancellationToken::new(), 1, DrainCause::SyncStarted).await, RunOutcome::default());
    assert_eq!(w.fixture.read_rows(|reader| reader.list_thumbnail_marks(None, 10)).len(), 1);
    assert!(w.server.received_requests().await.unwrap().is_empty());
    std::fs::remove_dir(&path).unwrap();
    assert_eq!(w.filler().drain(&CancellationToken::new(), 1, DrainCause::TimerFired).await, RunOutcome::default());
    assert!(path.is_file(), "next scan retries the local repair");
    assert!(w.server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn renaming_a_file_writes_both_new_names_and_removal_does_not_fetch_it() {
    let (old, new) = ("old space.jpg", "new space.jpg");
    let w = world(&[(old, "image/jpeg")]).await;
    seed_thumbnail(&w, old);
    w.fixture.move_here(w.item(old), new);
    // Its OneDrive binding remains the old id until the move is sent.
    Mock::given(method("GET")).and(path(thumb_path(old, "c512x512")))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(jpeg(64, 64))).expect(1).mount(&w.server).await;
    assert_eq!(w.filler().drain(&CancellationToken::new(), 1, DrainCause::TimerFired).await.written, 1);
    for name in [old, new] {
        for uri in thumbnail_uris(&w.root().join(name)) {
            for (dir, _) in SIZES {
                assert!(w.cache.path().join(dir).join(name_for_uri(&uri)).is_file());
            }
        }
    }
    w.fixture.remove_here(w.item(new));
    assert_eq!(w.filler().drain(&CancellationToken::new(), 1, DrainCause::TimerFired).await, RunOutcome::default());
    assert_eq!(w.server.received_requests().await.unwrap().len(), 1);
}

/// An image the folder shows gets the thumbnails KIO looks for, named by the path the folder
/// shows it at and stamped with the time of the content OneDrive has; made once, it is not
/// asked for again.
#[tokio::test]
async fn an_image_gets_the_thumbnails_kio_looks_for_and_is_asked_for_once() {
    let w = world(&[("p.jpg", "image/jpeg")]).await;
    Mock::given(method("GET")).and(path(thumb_path("p.jpg", "c512x512")))
        .respond_with(ResponseTemplate::new(200).insert_header("content-type", "image/jpeg").set_body_bytes(jpeg(600, 400)))
        .expect(1)
        .mount(&w.server).await;
    let filler = w.filler();
    let made = filler.run_once(&CancellationToken::new(), 100).await;
    assert_eq!((made.taken, made.written), (1, 1));
    for (dir, edge) in SIZES {
        let decoder = png::Decoder::new(std::io::BufReader::new(std::fs::File::open(w.cached(dir, "p.jpg")).unwrap()));
        let reader = decoder.read_info().unwrap();
        let info = reader.info();
        assert!(info.width.max(info.height) <= *edge, "{dir}: {}x{}", info.width, info.height);
        let text = |key: &str| info.uncompressed_latin1_text.iter().find(|t| t.keyword == key).map(|t| t.text.clone());
        assert_eq!(text("Thumb::URI"), Some(file_uri(&w.root().join("p.jpg"))));
        assert_eq!(text("Thumb::MTime"), Some(ONEDRIVE_MTIME.to_string()));
    }
    let again = filler.run_once(&CancellationToken::new(), 100).await;
    assert_eq!((again.taken, again.written), (0, 0), "no candidate the second time");
}

/// What settles the question is recorded for the content, and not asked again: Graph has
/// no thumbnail (404), refuses (403), or answers bytes that are no image.
#[tokio::test]
async fn an_item_with_no_usable_thumbnail_is_not_asked_again() {
    let w = world(&[("none.jpg", "image/jpeg"), ("refused.png", "image/png"), ("broken.mp4", "video/mp4")]).await;
    for (name, answer) in [("none.jpg", ResponseTemplate::new(404)), ("refused.png", ResponseTemplate::new(403)), ("broken.mp4", ResponseTemplate::new(200).set_body_bytes(b"no image at all".to_vec()))] {
        Mock::given(method("GET")).and(path(thumb_path(name, "c512x512"))).respond_with(answer).expect(1).mount(&w.server).await;
    }
    let filler = w.filler();
    let first = filler.run_once(&CancellationToken::new(), 100).await;
    assert_eq!((first.taken, first.written), (3, 0));
    assert_eq!(filler.run_once(&CancellationToken::new(), 100).await.taken, 0);
}

/// The thumbnails being downloaded are counted as their requests start and end, and none is
/// once the run has ended.
#[tokio::test]
async fn the_thumbnails_being_downloaded_are_counted_and_none_after_the_run() {
    let w = world(&[("a.jpg", "image/jpeg"), ("b.jpg", "image/jpeg")]).await;
    Mock::given(method("GET"))
        .and(path_regex(r"^/me/drive/items/[ab]\.jpg/thumbnails/0/c512x512/content$"))
        .respond_with(ResponseTemplate::new(200).insert_header("content-type", "image/jpeg").set_body_bytes(jpeg(64, 64)).set_delay(Duration::from_millis(300)))
        .mount(&w.server)
        .await;
    let mut counts = w.state.subscribe();
    let seen = tokio::spawn(async move {
        let mut seen = Vec::new();
        while counts.changed().await.is_ok() {
            let count = counts.borrow_and_update().transfers.thumbnail_downloads;
            seen.push(count);
            if count == 0 {
                break;
            }
        }
        seen
    });
    let made = w.filler().run_once(&CancellationToken::new(), 100).await;
    assert_eq!(made.written, 2);
    assert_eq!(w.state.get().transfers.thumbnail_downloads, 0, "none after the run");
    let seen = seen.await.unwrap();
    assert!(seen.contains(&2), "both requests under way at once: {seen:?}");
}

/// Documents are asked for as images are; plain text is not.
#[tokio::test]
async fn a_document_is_asked_for_and_plain_text_is_not() {
    let w = world(&[("notes.txt", "text/plain"), ("report.pdf", "application/pdf")]).await;
    Mock::given(method("GET")).and(path(thumb_path("report.pdf", "c512x512")))
        .respond_with(ResponseTemplate::new(200).insert_header("content-type", "image/jpeg").set_body_bytes(jpeg(400, 512)))
        .expect(1)
        .mount(&w.server).await;
    let made = w.filler().run_once(&CancellationToken::new(), 100).await;
    assert_eq!((made.taken, made.written), (1, 1));
    assert!(w.cached("x-large", "report.pdf").is_file());
}

/// Batches go on where the last one stopped, by local item id — each candidate is looked at
/// once per drain, the ones Graph fails for this time included — and the next drain starts
/// from the beginning again.
#[tokio::test]
async fn batches_go_on_where_the_last_stopped() {
    let names: Vec<String> = (0..5).map(|i| format!("p{i}.jpg")).collect();
    let files: Vec<(&str, &str)> = names.iter().map(|name| (name.as_str(), "image/jpeg")).collect();
    let w = world(&files).await;
    // Every request fails in a way worth trying again (a server error): nothing is recorded.
    Mock::given(method("GET")).and(path_regex(r"^/me/drive/items/p\d\.jpg/thumbnails/0/c512x512/content$")).respond_with(ResponseTemplate::new(500)).mount(&w.server).await;
    let filler = w.filler();
    let (first, next) = filler.run_from(&CancellationToken::new(), 2, None).await;
    assert_eq!((first.taken, next), (2, Some(w.item("p1.jpg"))));
    let (second, next) = filler.run_from(&CancellationToken::new(), 2, next).await;
    assert_eq!((second.taken, next), (2, Some(w.item("p3.jpg"))), "not the first two again");
    let (third, next) = filler.run_from(&CancellationToken::new(), 2, next).await;
    assert_eq!((third.taken, next), (1, None), "a batch that is not full was the last");
    let total = filler.drain(&CancellationToken::new(), 2, DrainCause::Nudged).await;
    assert_eq!(total.taken, 5, "a drain looks at each once, and ends");
    // A passing trouble settled nothing: the next drain asks for all of them again.
    assert_eq!(filler.drain(&CancellationToken::new(), 2, DrainCause::Nudged).await.taken, 5);
}

/// A full batch whose every candidate was settled leaves nothing for the next one, and the
/// drain ends there: it does not go round.
#[tokio::test]
async fn a_drain_ends_when_the_batch_after_a_full_one_is_empty() {
    let w = world(&[("a.jpg", "image/jpeg"), ("b.jpg", "image/jpeg")]).await;
    Mock::given(method("GET")).and(path_regex(r"^/me/drive/items/[ab]\.jpg/thumbnails/0/c512x512/content$")).respond_with(ResponseTemplate::new(404)).expect(2).mount(&w.server).await;
    let total = w.filler().drain(&CancellationToken::new(), 2, DrainCause::Nudged).await;
    assert_eq!(total.taken, 2);
}

/// Graph refuses `c512x512` for some items with `406`; its named size `large` is asked once
/// instead.
#[tokio::test]
async fn a_406_is_asked_again_at_graphs_named_size() {
    let w = world(&[("p.jpg", "image/jpeg")]).await;
    Mock::given(method("GET")).and(path(thumb_path("p.jpg", "c512x512"))).respond_with(ResponseTemplate::new(406)).expect(1).mount(&w.server).await;
    Mock::given(method("GET")).and(path(thumb_path("p.jpg", "large"))).respond_with(ResponseTemplate::new(200).set_body_bytes(jpeg(64, 64))).expect(1).mount(&w.server).await;
    assert_eq!(w.filler().run_once(&CancellationToken::new(), 100).await.written, 1);
    assert!(w.cached("normal", "p.jpg").is_file());
}

/// A stop does not wait for candidates that were not asked yet: with the stop given before
/// the batch, nothing is asked of Graph.
#[tokio::test]
async fn a_filler_that_is_told_to_stop_asks_for_nothing_more() {
    let w = world(&[("p.jpg", "image/jpeg")]).await;
    let stop = CancellationToken::new();
    stop.cancel();
    let made = w.filler().run_once(&stop, 100).await;
    assert_eq!(made.written, 0);
    assert!(w.server.received_requests().await.unwrap().is_empty());
}

/// The requests Graph was asked for thumbnails.
async fn asked_count(w: &World) -> usize {
    w.server.received_requests().await.unwrap().len()
}

/// A run that would wait for its turn is given a quarter of a second, then stopped.
async fn run_briefly(filler: &ThumbnailFiller) -> RunOutcome {
    let stop = CancellationToken::new();
    let stopper = {
        let stop = stop.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(250)).await;
            stop.cancel();
        })
    };
    let made = filler.run_once(&stop, 100).await;
    stopper.abort();
    made
}

/// No more than `REQUESTS_AT_ONCE` requests are under way together, however many
/// candidates there are and however many slots the pool has free; none takes a slot.
#[tokio::test]
async fn a_few_thumbnails_are_asked_for_at_once_and_none_takes_a_slot_of_the_pool() {
    let names: Vec<String> = (0..5).map(|i| format!("p{i}.jpg")).collect();
    let files: Vec<(&str, &str)> = names.iter().map(|name| (name.as_str(), "image/jpeg")).collect();
    let w = world(&files).await;
    // Each answer takes long enough for the first two to be under way together, and for
    // the filler to ask once more whether the third may start.
    Mock::given(method("GET")).and(path_regex(r"^/me/drive/items/p\d\.jpg/thumbnails/0/c512x512/content$")).respond_with(ResponseTemplate::new(404).set_delay(Duration::from_millis(600))).mount(&w.server).await;
    let filler = Arc::new(w.filler());
    let pool = Arc::clone(filler.drive.pool());
    let run = {
        let filler = Arc::clone(&filler);
        tokio::spawn(async move { filler.run_once(&CancellationToken::new(), 100).await })
    };
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(asked_count(&w).await, request_start::REQUESTS_AT_ONCE, "the rest wait for these");
    assert_eq!(pool.held(Class::Download), 0, "a thumbnail takes no slot");
    assert_eq!(run.await.unwrap().taken, 5);
    assert_eq!(asked_count(&w).await, 5, "each was asked in its turn");
    assert_eq!(pool.size(), konedrive_graph::pool::START, "and none made the pool grow");
}

/// While a listing is under way nothing is asked, and the run ends at once: the
/// listing's end nudges the filler.
#[tokio::test]
async fn nothing_is_asked_while_a_listing_is_under_way() {
    let w = world(&[("p.jpg", "image/jpeg")]).await;
    w.fixture.write_rows(|store| store.item_store().begin_delta_listing().map(|_| ()));
    let filler = w.filler();
    let (made, next) = filler.run_from(&CancellationToken::new(), 1, None).await;
    assert_eq!((made.written, next), (0, None), "the drain ends here");
    assert_eq!(asked_count(&w).await, 0);
}

/// Before a full listing ended (the store has no delta link) nothing is asked.
#[tokio::test]
async fn nothing_is_asked_before_the_first_listing_ended() {
    let w = world(&[("p.jpg", "image/jpeg")]).await;
    w.fixture.write_rows(|store| store.item_store().begin_full_listing().map(|_| ()));
    assert_eq!(w.filler().run_once(&CancellationToken::new(), 100).await.written, 0);
    assert_eq!(asked_count(&w).await, 0);
}

/// Thumbnails give way: none is asked while a file is being opened, while a transfer
/// waits for a slot of the pool, or during a throttle's wait.
#[tokio::test]
async fn nothing_is_asked_while_a_file_is_opened_a_transfer_waits_or_onedrive_throttles() {
    let w = world(&[("p.jpg", "image/jpeg")]).await;
    let filler = w.filler();
    let pool = Arc::clone(filler.drive.pool());

    let opened = pool.acquire(Class::Open).await;
    run_briefly(&filler).await;
    assert_eq!(asked_count(&w).await, 0, "a file is being opened");
    drop(opened);

    let mut downloads = Vec::new();
    for _ in 0..pool.size() {
        downloads.push(pool.acquire(Class::Download).await);
    }
    let waiting = {
        let pool = Arc::clone(&pool);
        tokio::spawn(async move { pool.acquire(Class::Upload).await })
    };
    while !pool.waiting() {
        tokio::task::yield_now().await;
    }
    run_briefly(&filler).await;
    assert_eq!(asked_count(&w).await, 0, "an upload waits for a slot");
    waiting.abort();
    let _ = waiting.await;
    run_briefly(&filler).await;
    assert_eq!(asked_count(&w).await, 1, "every slot busy and nothing waiting: a thumbnail needs no slot");
    drop(downloads);

    let w = world(&[("p.jpg", "image/jpeg")]).await;
    let filler = w.filler();
    filler.drive.pool().throttled(Some(Duration::from_secs(300)));
    run_briefly(&filler).await;
    assert_eq!(asked_count(&w).await, 0, "OneDrive asked to wait");
}

/// The cache cannot be written: nothing is recorded, the requests under way when the first
/// failure came end and no other starts; with the cache writable again, the next drain
/// asks for each and makes it.
#[tokio::test]
async fn a_cache_that_cannot_be_written_records_nothing_and_stops_the_drain() {
    let names: Vec<String> = (0..5).map(|i| format!("p{i}.jpg")).collect();
    let files: Vec<(&str, &str)> = names.iter().map(|name| (name.as_str(), "image/jpeg")).collect();
    let w = world(&files).await;
    Mock::given(method("GET"))
        .and(path_regex(r"^/me/drive/items/p\d\.jpg/thumbnails/0/c512x512/content$"))
        .respond_with(ResponseTemplate::new(200).insert_header("content-type", "image/jpeg").set_body_bytes(jpeg(64, 64)))
        .mount(&w.server)
        .await;
    let filler = w.filler();
    // No directory of a size can be made in it.
    std::fs::set_permissions(w.cache.path(), std::fs::Permissions::from_mode(0o500)).unwrap();
    let failed = filler.drain(&CancellationToken::new(), 200, DrainCause::Nudged).await;
    std::fs::set_permissions(w.cache.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(failed.written, 0);
    let asked = asked_count(&w).await;
    assert!((1..=request_start::REQUESTS_AT_ONCE).contains(&asked), "{asked} asked: none after the first failure");

    let made = filler.drain(&CancellationToken::new(), 200, DrainCause::Nudged).await;
    assert_eq!((made.taken, made.written), (5, 5), "nothing was recorded");
    assert!(w.cached("normal", "p0.jpg").is_file());
}

/// A thumbnail the cache lacks one size of is made again at the drain that looks into the
/// cache — the timer's, and the first after a start — and not at a cycle's. A file OneDrive
/// has no thumbnail of has nothing in the cache to look for, and is asked once.
#[tokio::test]
async fn a_thumbnail_missing_from_the_cache_is_made_again_at_the_timer_and_after_a_start_and_not_after_a_cycle() {
    let w = world(&[("p.jpg", "image/jpeg"), ("none.pdf", "application/pdf")]).await;
    Mock::given(method("GET")).and(path(thumb_path("p.jpg", "c512x512")))
        .respond_with(ResponseTemplate::new(200).insert_header("content-type", "image/jpeg").set_body_bytes(jpeg(600, 400)))
        .expect(3)
        .mount(&w.server).await;
    Mock::given(method("GET")).and(path(thumb_path("none.pdf", "c512x512"))).respond_with(ResponseTemplate::new(404)).expect(1).mount(&w.server).await;
    let filler = w.filler();
    let first = filler.drain(&CancellationToken::new(), 200, DrainCause::Nudged).await;
    assert_eq!((first.taken, first.written), (2, 1));

    std::fs::remove_file(w.cached("large", "p.jpg")).unwrap();
    assert_eq!(filler.drain(&CancellationToken::new(), 200, DrainCause::Nudged).await.taken, 0, "a cycle's drain goes by the marks alone");
    assert!(!w.cached("large", "p.jpg").exists());
    let timer = filler.drain(&CancellationToken::new(), 200, DrainCause::TimerFired).await;
    assert_eq!((timer.taken, timer.written), (1, 1), "the timer's drain looks into the cache");
    assert!(w.cached("large", "p.jpg").is_file());
    assert_eq!(filler.drain(&CancellationToken::new(), 200, DrainCause::TimerFired).await.taken, 0, "a thumbnail the cache holds whole is not made again");

    std::fs::remove_file(w.cached("x-large", "p.jpg")).unwrap();
    let started = filler.drain(&CancellationToken::new(), 200, DrainCause::SyncStarted).await;
    assert_eq!((started.taken, started.written), (1, 1), "and so does the first drain after a start");
    assert!(w.cached("x-large", "p.jpg").is_file());
}

/// A filler's first drain is the one after the folder's sync started, whatever woke it:
/// it looks into the cache.
#[tokio::test]
async fn the_first_drain_of_a_filler_looks_into_the_cache_whatever_woke_it() {
    let w = world(&[("p.jpg", "image/jpeg")]).await;
    Mock::given(method("GET")).and(path(thumb_path("p.jpg", "c512x512")))
        .respond_with(ResponseTemplate::new(200).insert_header("content-type", "image/jpeg").set_body_bytes(jpeg(64, 64)))
        .expect(2)
        .mount(&w.server).await;
    assert_eq!(w.filler().run_once(&CancellationToken::new(), 100).await.written, 1);
    std::fs::remove_file(w.cached("normal", "p.jpg")).unwrap();

    let (kick, stop) = (Arc::new(Notify::new()), CancellationToken::new());
    let task = w.filler().spawn(Arc::clone(&kick), stop.clone());
    kick.notify_one();
    let made_again = tokio::time::timeout(Duration::from_secs(10), async {
        while !w.cached("normal", "p.jpg").is_file() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
    stop.cancel();
    task.await.unwrap();
    assert!(made_again.is_ok(), "the nudge started the first drain, and it made the thumbnail again");
}

#[test]
fn a_thumbnail_every_size_of_which_is_found_is_whole() {
    assert_eq!(decide_cached_thumbnail(&["normal", "large", "x-large"]), CachedThumbnail::Whole);
    assert_eq!(decide_cached_thumbnail(&["x-large", "normal", "large"]), CachedThumbnail::Whole, "in any order");
}

#[test]
fn a_thumbnail_one_size_of_which_is_not_found_is_missing() {
    for lacked in ["normal", "large", "x-large"] {
        let found: Vec<&str> = SIZES.iter().map(|(size, _)| *size).filter(|size| *size != lacked).collect();
        assert_eq!(decide_cached_thumbnail(&found), CachedThumbnail::Missing, "without {lacked}");
    }
    assert_eq!(decide_cached_thumbnail(&["normal", "large", "xx-large"]), CachedThumbnail::Missing, "a size the filler does not write stands for none");
}

#[test]
fn a_thumbnail_no_size_of_which_is_found_is_missing() {
    assert_eq!(decide_cached_thumbnail(&[]), CachedThumbnail::Missing);
}

/// The look into the cache finds the sizes whose PNG is a file there, and fails where the
/// cache cannot be looked into.
#[test]
fn the_sizes_found_are_those_whose_png_is_in_the_cache() {
    let cache = tempfile::tempdir().unwrap();
    let file = std::path::Path::new("/folder/p.jpg");
    let name = thumbnail_name(file);
    for size in ["normal", "large", "x-large"] {
        std::fs::create_dir_all(cache.path().join(size)).unwrap();
    }
    for size in ["normal", "x-large"] {
        other_viewers_png(&cache.path().join(size).join(&name), &file_uri(file), 123, 0);
    }
    std::fs::create_dir(cache.path().join("large").join(&name)).unwrap();
    assert_eq!(find_cached_sizes(cache.path(), file, 123, 456).unwrap(), ["normal", "x-large"], "a directory of that name is no thumbnail");
    assert_eq!(find_cached_sizes(&cache.path().join("none"), file, 123, 456).unwrap(), Vec::<&str>::new(), "no cache at all");

    let not_a_directory = cache.path().join("a-file");
    std::fs::write(&not_a_directory, b"").unwrap();
    assert!(find_cached_sizes(&not_a_directory, file, 123, 456).is_err());
}
