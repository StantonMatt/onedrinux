#include "heldchanges.h"

#include <KLocalizedString>

#include <QCollator>
#include <QMimeDatabase>

#include <algorithm>

namespace
{
QString sizeText(qulonglong bytes)
{
    return HeldChanges::sizeText(bytes);
}

/// The folder of a relative path; the drive's top is "OneDrive".
QString folderOf(const QString &path)
{
    const int slash = path.lastIndexOf(QLatin1Char('/'));
    return slash < 0 ? i18nc("@info the folder of an item at the top of the drive", "OneDrive") : path.left(slash);
}

QString nameOf(const QString &path)
{
    return path.section(QLatin1Char('/'), -1);
}

}

HeldRowsModel::HeldRowsModel(QObject *parent)
    : QAbstractListModel(parent)
{
}

int HeldRowsModel::rowCount(const QModelIndex &parent) const
{
    return parent.isValid() ? 0 : int(m_rows.size());
}

QHash<int, QByteArray> HeldRowsModel::roleNames() const
{
    return {
        {NameRole, "name"},
        {PathRole, "path"},
        {DetailRole, "detail"},
        {DetailToolTipRole, "detailToolTip"},
        {IconNameRole, "iconName"},
        {SizeRole, "sizeText"},
        {AfterRole, "afterText"},
        {EmptiedRole, "emptied"},
        {CountRole, "countText"},
        {LocalPathRole, "localPath"},
        {FailureRole, "failure"},
    };
}

QVariant HeldRowsModel::data(const QModelIndex &index, int role) const
{
    if (!index.isValid() || index.row() >= m_rows.size()) {
        return {};
    }
    const KonedriveHeldChange &row = m_rows.at(index.row());
    const bool removal = row.kind == QLatin1String("removal");
    const bool rewrite = row.kind == QLatin1String("rewrite");
    const bool move = row.kind == QLatin1String("move");
    switch (role) {
    case Qt::DisplayRole:
    case NameRole:
        return nameOf(row.path);
    case PathRole:
        return row.path;
    case DetailRole:
        if (move && !row.oldPath.isEmpty()) {
            if (folderOf(row.oldPath) == folderOf(row.path)) {
                return i18nc("@info a renamed item: its old name, its folder", "Renamed from %1 · %2", nameOf(row.oldPath), folderOf(row.path));
            }
            // Moved, and maybe renamed on the way: where from, then the folder it is in now.
            const QString from = nameOf(row.oldPath) == nameOf(row.path) ? folderOf(row.oldPath) : row.oldPath;
            return i18nc("@info a moved item: where it was, where it is now", "%1 → %2", from, folderOf(row.path));
        }
        return folderOf(row.path);
    case DetailToolTipRole:
        if (move && !row.oldPath.isEmpty()) {
            return i18nc("@info:tooltip a moved item: its old path, its new path", "%1 → %2", row.oldPath, row.path);
        }
        return row.path;
    case IconNameRole:
        if (row.directory) {
            return QStringLiteral("folder");
        }
        return QMimeDatabase().mimeTypeForFile(nameOf(row.path), QMimeDatabase::MatchExtension).iconName();
    case SizeRole:
        // A move changes no size; a folder that moved shows what is in it.
        if (move) {
            return QString();
        }
        return sizeText(row.bytes);
    case AfterRole:
        return rewrite ? sizeText(row.afterBytes) : QString();
    case EmptiedRole:
        return rewrite && row.afterBytes == 0 && row.bytes > 0;
    case CountRole:
        if (!row.directory) {
            return QString();
        }
        if (row.kind == QLatin1String("upload")) {
            return i18ncp("@info what a new folder holds", "%1 file", "%1 files", row.descendants);
        }
        return i18ncp("@info what a folder holds", "%1 item", "%1 items", row.descendants);
    case LocalPathRole:
        // A deleted item isn't in the folder to show.
        if (removal || row.sent || m_root.isEmpty()) {
            return QString();
        }
        return QString(m_root + QLatin1Char('/') + row.path);
    case FailureRole:
        return failure(row);
    default:
        return {};
    }
}

QString HeldRowsModel::failure(const KonedriveHeldChange &row) const
{
    if (m_failures.isEmpty()) {
        return QString();
    }
    QString reason = m_failures.value(row.path);
    // A folder that stands for what was in it: the reason of what failed nearest its top, the
    // shortest path first and, between paths as long, the first by path.
    if (reason.isEmpty() && row.directory) {
        const QString inside = row.path + QLatin1Char('/');
        QString nearest;
        for (auto it = m_failures.cbegin(); it != m_failures.cend(); ++it) {
            const QString &path = it.key();
            if (path.startsWith(inside) && (nearest.isEmpty() || path.size() < nearest.size() || (path.size() == nearest.size() && path < nearest))) {
                nearest = path;
                reason = it.value();
            }
        }
    }
    // The daemon's words go on a line of their own: "it is open in another program" begins it.
    if (!reason.isEmpty()) {
        reason[0] = reason.at(0).toUpper();
    }
    return reason;
}

void HeldRowsModel::setRows(KonedriveHeldChangeList rows, const QString &root, const QHash<QString, QString> &failures)
{
    // By path, a part at a time, as Dolphin sorts names: numbers by value, case aside.
    QCollator collator;
    collator.setNumericMode(true);
    collator.setCaseSensitivity(Qt::CaseInsensitive);
    std::stable_sort(rows.begin(), rows.end(), [&collator](const KonedriveHeldChange &a, const KonedriveHeldChange &b) {
        const QStringList pa = a.path.split(QLatin1Char('/'));
        const QStringList pb = b.path.split(QLatin1Char('/'));
        for (int i = 0; i < std::min(pa.size(), pb.size()); ++i) {
            if (const int c = collator.compare(pa.at(i), pb.at(i)); c != 0) {
                return c < 0;
            }
        }
        return pa.size() < pb.size();
    });
    const bool countChanges = rows.size() != m_rows.size();
    const bool folders = std::any_of(rows.cbegin(), rows.cend(), [](const KonedriveHeldChange &row) {
        return row.directory;
    });
    beginResetModel();
    m_rows = std::move(rows);
    m_root = root;
    m_failures = failures;
    endResetModel();
    if (countChanges) {
        Q_EMIT countChanged();
    }
    if (folders != m_folders) {
        m_folders = folders;
        Q_EMIT foldersChanged();
    }
}
