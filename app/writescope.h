#pragma once

#include <QDir>
#include <QString>
#include <QStringList>

/// A trial's folders that can be changed (Account.WritePaths): paths relative to the drive
/// root, compared by their components, a folder covering everything inside it.

/// `path` relative to the OneDrive folder `root`, spelled as picked; empty for the folder
/// itself and for anything outside it.
inline QString folderInside(const QString &root, const QString &path)
{
    if (root.isEmpty() || path.isEmpty()) {
        return QString();
    }
    const QString base = QDir::cleanPath(root);
    const QString folder = QDir::cleanPath(path);
    const QString prefix = base == QLatin1String("/") ? base : base + QLatin1Char('/');
    return folder.startsWith(prefix) && folder.size() > prefix.size() ? folder.mid(prefix.size()) : QString();
}

/// `folder` is `other`, or inside it.
inline bool folderWithin(const QString &folder, const QString &other)
{
    return folder == other || folder.startsWith(other + QLatin1Char('/'));
}

/// The folders that can be changed once `folder` is added to `paths`: as they are when a
/// folder listed already covers it; otherwise with it last, and without the folders inside it.
inline QStringList writeScopeWith(const QStringList &paths, const QString &folder)
{
    for (const QString &path : paths) {
        if (folderWithin(folder, path)) {
            return paths;
        }
    }
    QStringList with;
    for (const QString &path : paths) {
        if (!folderWithin(path, folder)) {
            with << path;
        }
    }
    with << folder;
    return with;
}
