#include "filestate.h"

#include <QFile>
#include <QHash>

#include <cerrno>
#include <climits>
#include <string_view>

#include <sys/stat.h>
#include <sys/xattr.h>
#include <unistd.h>

namespace konedrive
{

namespace
{
FileState stateAttribute(const QByteArray &native)
{
    // The longest value is "online-only"; anything that does not fit is not
    // a state this plugin knows.
    char value[32];
    const ssize_t size = ::lgetxattr(native.constData(), StateAttribute, value, sizeof value);
    if (size < 0) {
        // ENODATA: no attribute. ENOTSUP: a filesystem without extended
        // attributes, where nothing can be ours either.
        return errno == ERANGE ? FileState::Unrecognised : FileState::Unmanaged;
    }

    const std::string_view state(value, static_cast<size_t>(size));
    if (state == "online-only") {
        return FileState::OnlineOnly;
    }
    if (state == "hydrating") {
        return FileState::Hydrating;
    }
    if (state == "hydrated") {
        return FileState::Hydrated;
    }
    if (state == "failed") {
        return FileState::Failed;
    }
    return FileState::Unrecognised;
}
} // namespace

FileState readFileState(const QString &path)
{
    const QByteArray native = QFile::encodeName(path);
    struct stat info {
    };
    if (::lstat(native.constData(), &info) != 0 || !S_ISREG(info.st_mode)) {
        return FileState::NotAFile;
    }
    return stateAttribute(native);
}

FileState readFolderState(const QString &dir)
{
    return stateAttribute(QFile::encodeName(dir));
}

UploadState readUploadState(const QString &path)
{
    const QByteArray native = QFile::encodeName(path);
    // The longest value is "uploading"; anything that does not fit is not
    // one this plugin knows.
    char value[16];
    const ssize_t size = ::lgetxattr(native.constData(), SyncAttribute, value, sizeof value);
    if (size < 0) {
        return UploadState::None;
    }
    const std::string_view state(value, static_cast<size_t>(size));
    if (state == "pending") {
        return UploadState::Pending;
    }
    if (state == "uploading") {
        return UploadState::Uploading;
    }
    if (state == "blocked") {
        return UploadState::Blocked;
    }
    return UploadState::None;
}

bool hasRootMark(const QString &dir)
{
    const QByteArray native = QFile::encodeName(dir);
    return ::lgetxattr(native.constData(), RootAttribute, nullptr, 0) >= 0;
}

bool hasPinMark(const QString &path)
{
    const QByteArray native = QFile::encodeName(path);
    return ::lgetxattr(native.constData(), PinAttribute, nullptr, 0) >= 0;
}

std::optional<QString> findRoot(const QString &dir, const RootMarkReader &hasMark)
{
    for (QString current = dir; !current.isEmpty(); current = parentDirectory(current)) {
        if (hasMark(current)) {
            return current;
        }
    }
    return std::nullopt;
}

QString physicalDirectory(const QString &dir)
{
    constexpr int MaxLinks = 40; // Linux's own limit when it resolves a path
    if (!dir.startsWith(QLatin1Char('/'))) {
        return {};
    }
    QStringList resolved; // physical components, none of them a link
    QStringList pending = dir.split(QLatin1Char('/'), Qt::SkipEmptyParts);
    int links = 0;
    while (!pending.isEmpty()) {
        const QString part = pending.takeFirst();
        if (part == QLatin1String(".")) {
            continue;
        }
        if (part == QLatin1String("..")) {
            if (!resolved.isEmpty()) {
                resolved.removeLast();
            }
            continue;
        }
        const QByteArray candidate = QFile::encodeName(QLatin1Char('/') + (resolved + QStringList{part}).join(QLatin1Char('/')));
        struct stat info {
        };
        if (::lstat(candidate.constData(), &info) != 0) {
            return {};
        }
        if (!S_ISLNK(info.st_mode)) {
            resolved.append(part);
            continue;
        }
        if (++links > MaxLinks) {
            return {};
        }
        char target[PATH_MAX];
        const ssize_t length = ::readlink(candidate.constData(), target, sizeof target);
        if (length <= 0 || length == static_cast<ssize_t>(sizeof target)) {
            return {};
        }
        const QString link = QFile::decodeName(QByteArray(target, length));
        if (link.startsWith(QLatin1Char('/'))) {
            resolved.clear();
        }
        pending = link.split(QLatin1Char('/'), Qt::SkipEmptyParts) + pending;
    }
    return QLatin1Char('/') + resolved.join(QLatin1Char('/'));
}

std::optional<QString> rootOf(const QString &dir, const RootMarkReader &hasMark)
{
    const QString physical = physicalDirectory(dir);
    if (physical.isEmpty()) {
        return std::nullopt;
    }
    return findRoot(physical, hasMark);
}

Emblem emblemFor(FileState state, bool pinned)
{
    switch (state) {
    case FileState::OnlineOnly:
        // The sweep queues a pinned online-only file for hydration; until
        // that finishes it looks exactly like a file already downloading.
        return pinned ? Emblem::Syncing : Emblem::Cloud;
    case FileState::Hydrating:
        return Emblem::Syncing;
    case FileState::Hydrated:
        return pinned ? Emblem::CheckFilled : Emblem::CheckOutline;
    case FileState::Failed:
        return Emblem::Error;
    case FileState::Unmanaged:
    case FileState::Unrecognised:
    case FileState::NotAFile:
        break;
    }
    return Emblem::None;
}

Emblem folderEmblemFor(FileState state, bool pinned)
{
    switch (state) {
    case FileState::Failed:
        return Emblem::Error;
    case FileState::Hydrating:
        // Something below is downloading, or kept and not downloaded yet.
        return Emblem::Syncing;
    case FileState::OnlineOnly:
        return pinned ? Emblem::CheckFilled : Emblem::Cloud;
    case FileState::Hydrated:
        return pinned ? Emblem::CheckFilled : Emblem::CheckOutline;
    case FileState::Unmanaged:
    case FileState::Unrecognised:
    case FileState::NotAFile:
        break;
    }
    return pinned ? Emblem::CheckFilled : Emblem::None;
}

QStringList overlayNames(Emblem emblem)
{
    // KOneDrive's own emblems (dolphin/emblems), installed in hicolor so that
    // every icon theme finds them. Each sits on a white plate in fixed
    // colours, which no theme recolours: Breeze's monochrome clouds and checks
    // vanished on white pages in Breeze Dark, and its syncing and error
    // states were the online-only cloud again.
    //   - konedrive-online-only: a blue outline cloud;
    //   - konedrive-syncing: a solid blue disc with circular arrows
    //     (downloading, pinned and still online-only -- the sweep is filling
    //     it -- or waiting to be uploaded);
    //   - konedrive-downloaded: a green ring and check, for a hydrated file
    //     nobody asked to keep (the OUTLINE case);
    //   - konedrive-always-keep: a solid green disc and white check, for one
    //     that a pin holds for (the FILLED case);
    //   - konedrive-error: a red rounded square and white X -- square, so that
    //     it is not told from the green discs by colour alone.
    switch (emblem) {
    case Emblem::Cloud:
        return {QStringLiteral("konedrive-online-only")};
    case Emblem::Syncing:
        return {QStringLiteral("konedrive-syncing")};
    case Emblem::CheckOutline:
        return {QStringLiteral("konedrive-downloaded")};
    case Emblem::CheckFilled:
        return {QStringLiteral("konedrive-always-keep")};
    case Emblem::Error:
        return {QStringLiteral("konedrive-error")};
    case Emblem::None:
        break;
    }
    return {};
}

namespace
{
QString withoutTrailingSlashes(const QString &path)
{
    QString result = path;
    while (result.size() > 1 && result.endsWith(QLatin1Char('/'))) {
        result.chop(1);
    }
    return result;
}
} // namespace

QString parentDirectory(const QString &path)
{
    const QString trimmed = withoutTrailingSlashes(path);
    const qsizetype slash = trimmed.lastIndexOf(QLatin1Char('/'));
    if (slash < 0 || trimmed == QLatin1String("/")) {
        return {};
    }
    if (slash == 0) {
        return QStringLiteral("/");
    }
    return trimmed.left(slash);
}

QString fileName(const QString &path)
{
    const QString trimmed = withoutTrailingSlashes(path);
    if (trimmed == QLatin1String("/")) {
        return {};
    }
    return trimmed.mid(trimmed.lastIndexOf(QLatin1Char('/')) + 1);
}

QString joinPath(const QString &dir, const QString &name)
{
    return dir.endsWith(QLatin1Char('/')) ? QString(dir + name) : QString(dir + QLatin1Char('/') + name);
}

bool isDirectory(const QString &path)
{
    const QByteArray native = QFile::encodeName(path);
    struct stat info {
    };
    return ::lstat(native.constData(), &info) == 0 && S_ISDIR(info.st_mode);
}

Emblem emblemForItem(FileState state, bool isDir, bool pinned, UploadState upload)
{
    if (upload == UploadState::Blocked || state == FileState::Failed) {
        return Emblem::Error;
    }
    if (upload == UploadState::Pending || upload == UploadState::Uploading) {
        return Emblem::Syncing;
    }
    return isDir ? folderEmblemFor(state, pinned) : emblemFor(state, pinned);
}

Emblem itemEmblem(const QString &path, const PinMarkReader &hasPin)
{
    // An item whose upload is blocked needs nothing else read.
    const UploadState upload = readUploadState(path);
    if (upload == UploadState::Blocked) {
        return Emblem::Error;
    }
    const bool isDir = isDirectory(path);
    const FileState state = isDir ? readFolderState(path) : readFileState(path);
    return emblemForItem(state, isDir, hasPin(path), upload);
}

bool anyInSyncFolder(const QStringList &paths, const RootMarkReader &hasRoot)
{
    // A root is looked up once per directory, for the duration of this call
    // only: a selection is usually many items of one folder.
    QHash<QString, bool> known;
    for (const QString &path : paths) {
        // An account's folder itself counts: it is asked about too.
        const QString dir = isDirectory(path) ? path : parentDirectory(path);
        if (dir.isEmpty()) {
            continue;
        }
        auto found = known.find(dir);
        if (found == known.end()) {
            found = known.insert(dir, rootOf(dir, hasRoot).has_value());
        }
        if (found.value()) {
            return true;
        }
    }
    return false;
}

} // namespace konedrive
