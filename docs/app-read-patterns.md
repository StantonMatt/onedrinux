# Desktop application reads

Measured on Kubuntu 26.04, 2026-11-01, from the `app-reads` worktree based on
`63fbb34`. Applications read generated local samples in private HOME/XDG trees
and private D-Bus sessions; Qt offscreen or an isolated Xvfb display was used.
No cloud mount, installed daemon, real bus, wallet or account was used.

Installed versions: VLC 3.0.23, Haruna 1.7.1, Okular 25.12.3, LibreOffice
26.2.6.3, KIO 6.24.0, ffmpegthumbs and kdegraphics-thumbnailers 25.12.3.

## Capture and replay

The corpus has identical 60-second H.264 videos as MP4 with `moov` at the end,
MP4 with `+faststart`, and MKV (about 42.3 MB each); a 150-page text PDF
(134,265 bytes); a 24-page image PDF (46,221,022 bytes); an 81-page DOCX
(8,571 bytes), ODT (45,740 bytes) and an 8,000-row XLSX (678,351 bytes).
These compressible office documents exercise ZIP seeks but do not represent
large image-heavy office archives.

`strace -f -ttt -yy -s 0` recorded `openat`, `close`, `read`, `readv`, `pread64`,
`preadv`, `preadv2`, `lseek`, `mmap` and `munmap`, filtered to the samples. The
normalizer keeps successful reads and handle lifetimes; seeks determine later
read offsets. Close-only fork/exec children discard inherited descriptor copies;
those closes are omitted from the effective reader handle, since the parent
continues reading that descriptor without reopening. No source reads are
omitted. The export validates every handle and range against the sample sizes. No sampled app
mapped the source file. Repeated reads are retained, not deduplicated. Generated
media and raw traces stay outside the checkout; normalized data has no content
or private paths.

`mount/item_kernel_requests/tests/app_reads/traces.json` records 22 captures:
15 app/open/navigation/browse cases and seven KIO previews. Each event is
`[operation, handle, file index or offset, read length]`: operations 0/1/2 are
open/read/close. `startup` is the event boundary at the first local screenshot,
before navigation (headless VLC runs use their whole capture window). The checked-in replay reconstructs deterministic bytes at
the same sizes and offsets, validating every returned range against those bytes.

The replays use the real Graph content source over fake Graph, with 300 ms delay
on each content response. Two modes cover a new argv launch with the default
1 MiB threshold, and navigation/streaming without argv intent with the threshold
raised to isolate range-cache costs. Even that raised threshold still hydrates
a file read completely, as `min(download_after, size)` requires. The real desktop
IDs are simulated with `ProcessReader`; the local probes themselves inherit the
T3 agent's executable scope, not a desktop launcher scope.

Replay timings measure **first returned bytes** and the captured startup read
prefix, not a measured first rendered frame/page over FUSE. Recorded delays for
playback, rendering and UI interaction are compressed out. Kernel page-cache,
read coalescing and readahead are not reproduced by this unit harness. The VM
suite separately checks real FUSE admission and parent-process classification.

## Observations

- VLC uses `readv` for demux/playback, reads the MP4 tail for `moov`/metadata and
  returns to media near the head. Its Qt seek capture jumps from the opening
  seconds to about 45 seconds using the normal keyboard shortcut. The isolated
  Qt/Xvfb video surface was green in this VLC build, so rendered VLC frames are
  unverified; demux/read and seek-position evidence is retained separately. A
  separate four-second local H.264 control exported correct decoded PNGs at
  zero and two seconds through VLC's scene filter, despite converter warnings. MKV also seeks to its tail. A complete 60-second
  playback at 8× reads all 42,271,635 MP4 bytes. Ordinary playback and a far seek
  require ranges at those offsets, never a prefix download up to the seek.
- Haruna needs Xvfb/software OpenGL here: Qt offscreen alone remained alive but
  never opened the video. Under Xvfb it rendered frames and accepted seeking.
  It read 15.1 MB of the selected MP4 and also inspected neighbouring MKV/MP4
  files for its playlist: 229,376 and 46,754 distinct bytes. Those neighbours
  remain cloud-only in the replay; they still cost metadata-read bandwidth.
- Okular/Poppler uses many overlapping 16 KiB `pread64` calls. Opening and jumping
  to the last of 150 text pages returned 8.51 MB across 587 reads, covering only
  131,632 distinct bytes. One aligned small-file fetch answers those repeats.
  The image PDF read first-page/nearby-page images and tail/xref data; jumping
  to page 24 made sparse later reads. Local screenshots confirm both pages.
- Writer and Calc use ZIP head/tail/directory probes and `pread64`; repeated
  probes greatly exceed distinct data on the small DOCX. Screenshots confirm
  Writer's last of 81 pages and Calc at O8000. `soffice.bin` is launched through
  `oosplash` and keeps the file argument. LibreOffice desktop IDs are not program
  hosts; neither executable needs a whitelist or a new host-list entry.
- Fresh file-open dialogs in all four apps browsed the sample directory without
  reading any original video/PDF/office sample. Okular did read generated PNG
  preview artifacts present in the local directory. The browse replay exercises
  directory listing/attributes and checks that those need no Graph request.
- KIO `PreviewJob`, using the installed plugins Dolphin uses, produced PDF,
  video and ODT previews. The generated DOCX/XLSX lacked embedded preview images;
  `opendocumentthumbnail` read their ZIP data but returned no preview. This is
  not an installed-daemon thumbnail-cache test. With guard enforcement, local
  thumbnail workers cannot read cloud-only sources; the daemon's independent
  Graph-thumbnail cache remains the normal preview source.

## Defects fixed

**Ghostscript lost the background role.** `gsthumbnail` launches
`gs -sDEVICE=png16m -sOutputFile=- -dFirstPage=1 -dLastPage=1 ... <file.pdf>`.
Ghostscript opens the PDF itself, under the inherited app unit. Before the fix,
`gs` in Dolphin's app unit was an ordinary reader; its filename argument could
also start whole-file launch-intent hydration. The image-PDF preview read about
1.94 MB of distinct source data, exceeding the ordinary coverage threshold too.

The direct KIO thumbnail-parent relationship now preserves `kio-thumbnail` for
`gs`; the parent and child facts are checked through procfs. No executable-wide
Ghostscript block was added. KIO `file.so` parents and independent Ghostscript
continue to read normally. A regression failed before the change (`gs` versus
`kio-thumbnail`); replayed PDF/video/office thumbnail opens now refuse before
any Graph request. A VM regression uses real parent/child processes in a
Dolphin app cgroup, with a resident-file and deliberate-file-worker control.

**Crossing packets refetched cached blocks.** A read straddling a 1 MiB boundary
fetched both aligned blocks even when one edge was already cached. The focused
regression showed `bytes=0-2097151` after `bytes=0-1048575`, wasting 1 MiB. Listed
reads now trim fully cached blocks from either edge and assemble the answer from
version-checked bytes. Missing middle blocks still use one request; expiry,
version bindings, authentication and the cache bound stay in force. Concurrent
cache eviction can conservatively fall back to the existing metadata path.

## Landing and remaining limits

Under [design/mount.md](design/mount.md) §5.2–5.3, a fresh
argv launch requests a verified whole copy at its first read; other readers
trigger hydration after 1 MiB of distinct coverage (or the whole smaller file).
A fully watched video therefore lands on disk, usually well before its end.
Streaming-only playback would be a product-policy change and is not introduced.
Seeking continues as checked ranges while a whole copy downloads; unverified
staging is never a source for playback.

Haruna's playlist probes cannot be distinguished from deliberate reads of those
neighbours by file offset alone. Launch intent now supplies the distinction for
thirty seconds: the same desktop identity's reads of other cloud-only audio/video
files in the launched media file's folder are background peeks, even after the
target's first read consumed its intent. Stored OneDrive MIME types identify media;
unknown types follow normal rules. Peeks return ordinary checked ranges without
counting item or handle landing coverage or triggering a whole download through
launch intent or coverage, so even a small neighbour read completely stays
cloud-only under those rules. The mount.md §5.5 version-revalidation exception still
starts or joins a whole download and lands the asked content before returning its
range when that content was previously refused. Files named in argv are exempt.
After the window, playing a neighbour counts fresh coverage normally. The captured Haruna neighbours
still stay cloud-only; their metadata bandwidth remains application behavior.

The cached-edge optimization applies to listed/version-proven requests. The
conservative metadata fallback still fetches the full aligned interval. Real
network throughput, application rendering under a mounted slow connection,
single-instance D-Bus activation and large office archives remain unmeasured.
