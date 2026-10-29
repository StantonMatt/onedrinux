#include "heldchanges.h"

#include "changeguardinterface.h"
#include "daemoncontroller.h"

#include <KLocalizedString>

#include <QCollator>
#include <QDBusError>
#include <QDBusPendingCallWatcher>
#include <QDBusPendingReply>
#include <QDBusServiceWatcher>
#include <QDateTime>
#include <QLocale>
#include <QMimeDatabase>
#include <QTimer>

#include <algorithm>
#include <limits>
#include <numeric>

const QStringList HeldChanges::KindOrder = {QStringLiteral("removal"), QStringLiteral("rewrite"), QStringLiteral("move"), QStringLiteral("upload")};

namespace
{
/// How long news waits before the next listing: a mass change sends many.
constexpr int ListDelayMs = 250;

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

/// What a single row counts: a removed or new folder its items too, as the daemon counts.
qulonglong changesIn(const KonedriveHeldChange &row)
{
    return 1 + (row.kind == QLatin1String("removal") || row.kind == QLatin1String("upload") ? row.descendants : 0);
}

/// "A", "A and B", "A, B and C".
QString joined(const QStringList &parts)
{
    if (parts.size() <= 1) {
        return parts.value(0);
    }
    return i18nc("@info the last two of a list: all before the last, the last", "%1 and %2", parts.mid(0, parts.size() - 1).join(i18nc("@info list separator", ", ")), parts.last());
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
    default:
        return {};
    }
}

void HeldRowsModel::setRows(KonedriveHeldChangeList rows, const QString &root)
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
    beginResetModel();
    m_rows = std::move(rows);
    m_root = root;
    endResetModel();
    if (countChanges) {
        Q_EMIT countChanged();
    }
}

HeldChanges::HeldChanges(const QDBusConnection &bus, const QString &accountPath, QObject *parent)
    : QObject(parent)
    , m_bus(bus)
    , m_id(accountPath.section(QLatin1Char('/'), -1))
    , m_interface(new OrgKonedriveChangeGuardInterface(DaemonController::ServiceName, DaemonController::ObjectPath, bus, this))
    , m_watcher(new QDBusServiceWatcher(DaemonController::ServiceName, bus, QDBusServiceWatcher::WatchForOwnerChange, this))
    , m_listTimer(new QTimer(this))
    , m_clock([] {
        return QDateTime::currentSecsSinceEpoch();
    })
    , m_rows(new HeldRowsModel(this))
{
    registerKonedriveChangeGuardTypes();
    // The signals first: the list read after them is never older than what they say.
    // HeldChanged names no account: every account lists again.
    connect(m_interface, &OrgKonedriveChangeGuardInterface::HeldChanged, this, &HeldChanges::requestList);
    connect(m_interface, &OrgKonedriveChangeGuardInterface::ChangesHeld, this, [this](const QString &account) {
        if (account == m_id) {
            requestList();
        }
    });
    connect(m_interface, &OrgKonedriveChangeGuardInterface::RestoreProgress, this, &HeldChanges::onRestoreProgress);
    m_listTimer->setSingleShot(true);
    m_listTimer->setInterval(ListDelayMs);
    connect(m_listTimer, &QTimer::timeout, this, &HeldChanges::list);
    connect(m_watcher, &QDBusServiceWatcher::serviceOwnerChanged, this, [this](const QString &, const QString &, const QString &newOwner) {
        // What was under way went with the daemon; its replies end in errors.
        if (newOwner.isEmpty()) {
            setServiceAvailable(false);
        } else {
            list();
        }
    });
    list();
}

HeldChanges::~HeldChanges() = default;

void HeldChanges::requestList()
{
    if (m_listing) {
        m_listAgain = true;
    } else if (!m_listTimer->isActive()) {
        m_listTimer->start();
    }
}

void HeldChanges::list()
{
    if (m_listing) {
        m_listAgain = true;
        return;
    }
    m_listTimer->stop();
    m_listing = true;
    m_listAgain = false;
    auto *watcher = new QDBusPendingCallWatcher(m_interface->ListHeld(m_id), this);
    connect(watcher, &QDBusPendingCallWatcher::finished, this, [this](QDBusPendingCallWatcher *w) {
        w->deleteLater();
        m_listing = false;
        const QDBusPendingReply<QString, KonedriveHeldChangeList> reply = *w;
        if (reply.isError()) {
            // An operational failure is different from an absent service/interface. In
            // either case the cached holds are still unresolved, never an empty listing.
            m_listingError = reply.error().message();
            const auto error = reply.error().type();
            setServiceAvailable(error != QDBusError::ServiceUnknown && error != QDBusError::NoReply
                                && error != QDBusError::Disconnected && error != QDBusError::UnknownInterface
                                && error != QDBusError::UnknownMethod && error != QDBusError::UnknownObject);
            Q_EMIT changed();
        } else {
            take(reply.argumentAt<0>(), reply.argumentAt<1>());
        }
        if (m_listAgain) {
            requestList();
        }
    });
}

void HeldChanges::take(const QString &token, const KonedriveHeldChangeList &rows)
{
    KonedriveHeldChangeList held;
    KonedriveHeldChangeList sent;
    for (const KonedriveHeldChange &row : rows) {
        (row.sent ? sent : held) << row;
    }
    m_listingError.clear();
    m_token = token;
    m_held = held;
    // What was sent before a hold that is over stays while the page shows it.
    if (!sent.isEmpty() || !held.isEmpty() || !m_reviewing) {
        m_sent = sent;
    }
    m_listed = true;
    // Restores the window did not ask end with the listing that has none under way.
    if (std::none_of(held.cbegin(), held.cend(), [](const KonedriveHeldChange &row) {
            return row.state == QLatin1String("restoring");
        })) {
        m_otherRestores.clear();
    }
    if (!m_serviceAvailable) {
        m_serviceAvailable = true;
    }
    refreshRows();
    Q_EMIT changed();
}

void HeldChanges::setServiceAvailable(bool available)
{
    if (m_serviceAvailable == available) {
        return;
    }
    m_serviceAvailable = available;
    if (!available) {
        m_listTimer->stop();
        m_listAgain = false;
        m_token.clear();
        m_otherRestores.clear();
        refreshRows();
    }
    Q_EMIT changed();
}

void HeldChanges::setRootPath(const QString &root)
{
    if (m_root == root) {
        return;
    }
    m_root = root;
    refreshRows();
}

void HeldChanges::setReviewing(bool reviewing)
{
    if (m_reviewing == reviewing) {
        return;
    }
    m_reviewing = reviewing;
    // Left: what was sent goes with the hold.
    if (!reviewing && m_held.isEmpty() && !m_sent.isEmpty()) {
        m_sent.clear();
        refreshRows();
        Q_EMIT changed();
    }
}

void HeldChanges::showRows(const QString &kind, bool sent)
{
    m_rowsKind = kind;
    m_rowsSent = sent;
    refreshRows();
}

void HeldChanges::refreshRows()
{
    KonedriveHeldChangeList rows;
    if (!m_rowsKind.isEmpty()) {
        for (const KonedriveHeldChange &row : m_rowsSent ? m_sent : m_held) {
            if (row.kind == m_rowsKind) {
                rows << row;
            }
        }
    }
    m_rows->setRows(rows, m_root);
}

QList<HeldChanges::Kind> HeldChanges::summed(bool sent) const
{
    QList<Kind> kinds;
    for (const QString &name : KindOrder) {
        Kind kind{name, 0, 0, std::numeric_limits<qint64>::max()};
        for (const KonedriveHeldChange &row : sent ? m_sent : m_held) {
            if (row.kind == name) {
                kind.count += changesIn(row);
                kind.bytes += row.bytes;
                kind.since = std::min(kind.since, row.since);
            }
        }
        if (kind.count > 0) {
            kinds << kind;
        }
    }
    return kinds;
}

QList<HeldChanges::Kind> HeldChanges::held() const
{
    return summed(false);
}

int HeldChanges::count() const
{
    qulonglong count = 0;
    for (const KonedriveHeldChange &row : m_held) {
        count += changesIn(row);
    }
    return int(std::min<qulonglong>(count, std::numeric_limits<int>::max()));
}

QString HeldChanges::sizeText(qulonglong bytes)
{
    if (bytes == 0) {
        return i18nc("@info a size of nothing", "0 B");
    }
    const QLocale locale;
    const qint64 size = qint64(std::min<qulonglong>(bytes, std::numeric_limits<qint64>::max()));
    // One decimal, none where it is a 0: "2.4 MiB", "88 KiB".
    const QString text = locale.formattedDataSize(size, 1);
    return text.contains(locale.decimalPoint() + QLatin1Char('0') + QLatin1Char(' ')) ? locale.formattedDataSize(size, 0) : text;
}

QString HeldChanges::countText(const QString &kind, qulonglong count)
{
    if (kind == QLatin1String("removal")) {
        return i18ncp("@info held changes", "%1 deletion", "%1 deletions", count);
    }
    if (kind == QLatin1String("rewrite")) {
        return i18ncp("@info held changes", "%1 changed file", "%1 changed files", count);
    }
    if (kind == QLatin1String("move")) {
        return i18ncp("@info held changes", "%1 move", "%1 moves", count);
    }
    if (kind == QLatin1String("upload")) {
        return i18ncp("@info held changes", "%1 new file", "%1 new files", count);
    }
    return i18ncp("@info held changes", "%1 change", "%1 changes", count);
}

QString HeldChanges::kindsText(const QList<Kind> &kinds)
{
    QStringList parts;
    for (const Kind &kind : kinds) {
        parts << countText(kind.kind, kind.count);
    }
    return joined(parts);
}

QString HeldChanges::sinceText(qint64 unixSeconds) const
{
    const QDateTime when = QDateTime::fromSecsSinceEpoch(unixSeconds);
    const QDate today = QDateTime::fromSecsSinceEpoch(m_clock()).date();
    const QString time = QLocale().toString(when.time(), QLocale::ShortFormat);
    if (when.date() == today) {
        return time;
    }
    if (when.date() == today.addDays(-1)) {
        return i18nc("@info a time yesterday", "yesterday at %1", time);
    }
    return QLocale().toString(when.date(), QLocale::ShortFormat);
}

QVariantMap HeldChanges::kindMap(const Kind &kind) const
{
    const QString &k = kind.kind;
    const QString since = i18nc("@info the time of the first held change", "since %1", sinceText(kind.since));
    QString title;
    QString amount;
    QString iconName;
    QString note;
    QString allowText;
    QString allowIcon;
    QString restoreText;
    QString restoreIcon = QStringLiteral("edit-undo");
    if (k == QLatin1String("removal")) {
        title = i18nc("@title a kind of held change", "Deletions");
        amount = i18ncp("@info deleted items", "%1 item", "%1 items", kind.count);
        iconName = QStringLiteral("edit-delete");
        allowText = i18nc("@action:button", "Delete in OneDrive");
        allowIcon = QStringLiteral("edit-delete");
        restoreText = i18nc("@action:button bring deleted items back", "Restore");
    } else if (k == QLatin1String("rewrite")) {
        title = i18nc("@title a kind of held change", "Changed files");
        amount = i18ncp("@info changed files", "%1 file", "%1 files", kind.count);
        iconName = QStringLiteral("document-edit");
        note = i18n("Restore puts back OneDrive's version and keeps yours beside it as a copy.");
        allowText = i18nc("@action:button", "Upload Changes");
        allowIcon = QStringLiteral("cloud-upload");
        restoreText = i18nc("@action:button put back OneDrive's version", "Restore");
    } else if (k == QLatin1String("move")) {
        title = i18nc("@title a kind of held change", "Moves and renames");
        amount = i18ncp("@info moved items", "%1 item", "%1 items", kind.count);
        iconName = QStringLiteral("transform-move");
        allowText = i18nc("@action:button", "Move in OneDrive");
        allowIcon = QStringLiteral("transform-move");
        restoreText = i18nc("@action:button", "Move Back");
    } else {
        title = i18nc("@title a kind of held change", "New files");
        amount = i18ncp("@info new files", "%1 file", "%1 files", kind.count);
        iconName = QStringLiteral("document-new");
        note = i18n("Don't Upload keeps them on this computer only; Issues lists them.");
        allowText = i18nc("@action:button upload new files", "Upload");
        allowIcon = QStringLiteral("cloud-upload");
        restoreText = i18nc("@action:button", "Don't Upload");
        restoreIcon = QStringLiteral("dialog-cancel");
    }
    // Moves have no size.
    const bool sized = k != QLatin1String("move");
    const QString size = sizeText(kind.bytes);

    // A decision under way for the kind: the window's, or one asked elsewhere.
    bool working = false;
    bool restoring = false;
    qulonglong done = 0;
    qulonglong total = 0;
    const auto take = [&](const Call &call) {
        if (call.kinds.contains(k)) {
            working = true;
            restoring = call.restore;
            done = call.done;
            total = call.total;
        }
    };
    for (const Call &call : m_calls) {
        take(call);
    }
    for (const Call &call : m_otherRestores) {
        take(call);
    }
    QString workingText;
    if (working && restoring) {
        if (k == QLatin1String("move")) {
            workingText = total > 0 ? i18nc("@info progress", "Moving back… %1 of %2", done, total) : i18nc("@info progress", "Moving back…");
        } else if (k == QLatin1String("upload")) {
            workingText = total > 0 ? i18nc("@info progress", "Keeping them on this computer… %1 of %2", done, total)
                                    : i18nc("@info progress", "Keeping them on this computer…");
        } else {
            workingText = total > 0 ? i18nc("@info progress", "Restoring… %1 of %2", done, total) : i18nc("@info progress", "Restoring…");
        }
    } else if (working) {
        if (k == QLatin1String("removal")) {
            workingText = i18nc("@info progress", "Deleting in OneDrive…");
        } else if (k == QLatin1String("move")) {
            workingText = i18nc("@info progress", "Moving in OneDrive…");
        } else {
            workingText = i18nc("@info progress", "Uploading…");
        }
    }
    return {
        {QStringLiteral("kind"), k},
        {QStringLiteral("title"), title},
        {QStringLiteral("detail"),
         sized ? i18nc("@info a held kind: how many, how much, since when", "%1 · %2 · %3", amount, size, since)
               : i18nc("@info a held kind: how many, since when", "%1 · %2", amount, since)},
        {QStringLiteral("countText"), countText(k, kind.count)},
        {QStringLiteral("statusDetail"), sized ? i18nc("@info a held kind: how much, since when", "%1 · %2", size, since) : since},
        {QStringLiteral("iconName"), iconName},
        {QStringLiteral("note"), note},
        {QStringLiteral("allowText"), allowText},
        {QStringLiteral("allowIcon"), allowIcon},
        {QStringLiteral("restoreText"), restoreText},
        {QStringLiteral("restoreIcon"), restoreIcon},
        {QStringLiteral("working"), working},
        {QStringLiteral("workingText"), workingText},
        {QStringLiteral("progress"), restoring && total > 0 ? double(done) / double(total) : -1.0},
    };
}

QVariantList HeldChanges::kinds() const
{
    QVariantList rows;
    for (const Kind &kind : held()) {
        rows << kindMap(kind);
    }
    return rows;
}

QVariantList HeldChanges::sentKinds() const
{
    QVariantList rows;
    for (const Kind &kind : summed(true)) {
        qint64 last = 0;
        for (const KonedriveHeldChange &row : m_sent) {
            if (row.kind == kind.kind) {
                last = std::max(last, row.since);
            }
        }
        const QString from = QLocale().toString(QDateTime::fromSecsSinceEpoch(kind.since).time(), QLocale::ShortFormat);
        const QString to = QLocale().toString(QDateTime::fromSecsSinceEpoch(last).time(), QLocale::ShortFormat);
        const QString when = from == to ? from : i18nc("@info a span of time of day", "%1–%2", from, to);
        QString where;
        QString iconName;
        if (kind.kind == QLatin1String("removal")) {
            where = i18n("in OneDrive's recycle bin");
            iconName = QStringLiteral("edit-delete");
        } else if (kind.kind == QLatin1String("rewrite")) {
            where = i18n("earlier versions are in each file's version history");
            iconName = QStringLiteral("document-edit");
        } else if (kind.kind == QLatin1String("move")) {
            where = i18n("already moved in OneDrive");
            iconName = QStringLiteral("transform-move");
        } else {
            where = i18n("already in OneDrive");
            iconName = QStringLiteral("document-new");
        }
        rows << QVariantMap{
            {QStringLiteral("kind"), kind.kind},
            {QStringLiteral("title"), countText(kind.kind, kind.count)},
            {QStringLiteral("detail"), i18nc("@info sent before the hold: when, where it is now", "%1 · %2", when, where)},
            {QStringLiteral("iconName"), iconName},
        };
    }
    return rows;
}

bool HeldChanges::removalsSent() const
{
    return std::any_of(m_sent.cbegin(), m_sent.cend(), [](const KonedriveHeldChange &row) {
        return row.kind == QLatin1String("removal");
    });
}

QString HeldChanges::headline() const
{
    const QList<Kind> kinds = held();
    if (kinds.isEmpty()) {
        return QString();
    }
    const qulonglong total = std::accumulate(kinds.cbegin(), kinds.cend(), qulonglong(0), [](qulonglong sum, const Kind &kind) {
        return sum + kind.count;
    });
    // The counts are in the kinds' words.
    return total == 1 ? i18nc("@info the held kinds, joined", "%1 is on hold", kindsText(kinds)) : i18nc("@info the held kinds, joined", "%1 are on hold", kindsText(kinds));
}

QString HeldChanges::headDetail() const
{
    const QList<Kind> kinds = held();
    if (kinds.isEmpty()) {
        return QString();
    }
    qint64 since = std::numeric_limits<qint64>::max();
    qulonglong total = 0;
    for (const Kind &kind : kinds) {
        since = std::min(since, kind.since);
        total += kind.count;
    }
    if (total == 1) {
        return i18nc("@info the time of the held change",
                     "It was made in the folder at %1 and is not sent to OneDrive until you allow it. Everything else keeps syncing.",
                     sinceText(since));
    }
    return i18nc("@info the time of the first held change",
                 "They were made in the folder since %1 and are not sent to OneDrive until you allow them. Everything else keeps syncing.",
                 sinceText(since));
}

QString HeldChanges::summary() const
{
    const QList<Kind> kinds = held();
    if (kinds.isEmpty()) {
        return QString();
    }
    if (kinds.size() == 1) {
        return i18nc("@info status: how many of a kind are on hold", "%1 on hold", countText(kinds.constFirst().kind, kinds.constFirst().count));
    }
    return i18ncp("@info status", "%1 change on hold", "%1 changes on hold", count());
}

void HeldChanges::onRestoreProgress(const QString &account, const QString &token, const QStringList &kinds, qulonglong done, qulonglong total)
{
    if (account != m_id) {
        return;
    }
    // An empty list is every kind.
    const QStringList named = kinds.isEmpty() ? KindOrder : kinds;
    bool ours = false;
    for (Call &call : m_calls) {
        if (call.restore && call.token == token && call.kinds == named) {
            call.done = done;
            call.total = total;
            ours = true;
        }
    }
    if (!ours) {
        const QString key = named.join(QLatin1Char(','));
        if (done >= total) {
            m_otherRestores.remove(key);
        } else {
            m_otherRestores.insert(key, Call{true, token, named, done, total});
        }
    }
    Q_EMIT changed();
}

void HeldChanges::decide(bool restore,
                         const QStringList &kinds,
                         const QString &token,
                         std::function<void(const KonedriveGuardResult &)> done,
                         std::function<void(const QString &)> failed)
{
    const quint64 number = ++m_lastCall;
    m_calls.insert(number, Call{restore, token, kinds, 0, 0});
    Q_EMIT changed();
    // Restoring thousands of items takes minutes, as Free Up Space does: no D-Bus timeout.
    auto message = QDBusMessage::createMethodCall(DaemonController::ServiceName,
                                                  DaemonController::ObjectPath,
                                                  QLatin1String(OrgKonedriveChangeGuardInterface::staticInterfaceName()),
                                                  restore ? QStringLiteral("RestoreHeld") : QStringLiteral("AllowHeld"));
    message << m_id << token << kinds;
    auto *watcher = new QDBusPendingCallWatcher(m_bus.asyncCall(message, std::numeric_limits<int>::max()), this);
    connect(watcher, &QDBusPendingCallWatcher::finished, this, [this, number, done, failed](QDBusPendingCallWatcher *w) {
        w->deleteLater();
        m_calls.remove(number);
        const QDBusPendingReply<KonedriveGuardResult> reply = *w;
        if (reply.isError()) {
            if (failed) {
                failed(reply.error().message());
            }
        } else if (done) {
            done(reply.value());
        }
        Q_EMIT changed();
        requestList();
    });
}

void HeldChanges::allow(const QString &kind)
{
    decideHere(false, kind);
}

void HeldChanges::restore(const QString &kind)
{
    decideHere(true, kind);
}

void HeldChanges::decideHere(bool restore, const QString &kind)
{
    QList<Kind> selected;
    QStringList kinds;
    qulonglong shown = 0;
    for (const Kind &held : held()) {
        if (kind.isEmpty() || held.kind == kind) {
            selected << held;
            kinds << held.kind;
            shown += held.count;
        }
    }
    if (kinds.isEmpty() || m_token.isEmpty()) {
        return;
    }
    decide(
        restore,
        kinds,
        m_token,
        [this, restore, selected, kinds, shown](const KonedriveGuardResult &result) {
            // Every kind done as shown: what each became, in the page's order.
            if (selected.size() > 1 && result.failed == 0 && result.pendingOffline == 0 && result.done == shown) {
                for (auto it = selected.crbegin(); it != selected.crend(); ++it) {
                    KonedriveGuardResult one;
                    one.done = it->count;
                    QString type;
                    const QString text = resultText(restore, {it->kind}, it->count, one, &type);
                    addResult(type, text);
                }
                return;
            }
            QString type;
            const QString text = resultText(restore, kinds, shown, result, &type);
            addResult(type, text);
        },
        [this, restore](const QString &error) {
            addResult(QStringLiteral("error"), restore ? i18n("Nothing was restored: %1", error) : i18n("Nothing was allowed: %1", error));
        });
}

QString HeldChanges::failureText(const KonedriveGuardResult &result)
{
    QStringList reasons;
    for (const KonedriveGuardFailure &failure : result.failures) {
        if (!failure.reason.isEmpty() && !reasons.contains(failure.reason)) {
            reasons << failure.reason;
        }
    }
    return reasons.join(QLatin1Char(' '));
}

QString HeldChanges::resultText(bool restore, const QStringList &kinds, qulonglong shown, const KonedriveGuardResult &result, QString *type)
{
    const qulonglong done = result.done + result.pendingOffline;
    if (result.failed > 0) {
        *type = QStringLiteral("warning");
        const qulonglong asked = done + result.failed;
        const QString stays = i18ncp("@info changes that stay on hold", "1 is still on hold.", "%1 are still on hold.", result.failed);
        const QString what = restore ? i18nc("@info some failed", "Restored %1 of %2 items.", done, asked) : i18nc("@info some failed", "Allowed %1 of %2 changes.", done, asked);
        const QString why = failureText(result);
        return why.isEmpty() ? i18nc("@info what was done, what stays", "%1 %2", what, stays) : i18nc("@info what was done, what stays, why", "%1 %2 %3", what, stays, why);
    }
    const QString kind = kinds.size() == 1 ? kinds.constFirst() : QString();
    if (result.pendingOffline > 0) {
        *type = QStringLiteral("information");
        const qulonglong n = result.pendingOffline;
        if (kind == QLatin1String("removal")) {
            return i18np("1 item waits to be deleted in OneDrive: it is deleted once OneDrive can be reached.",
                         "%1 items wait to be deleted in OneDrive: they are deleted once OneDrive can be reached.",
                         n);
        }
        if (kind == QLatin1String("move")) {
            return i18np("1 item waits to be moved in OneDrive: it is moved once OneDrive can be reached.",
                         "%1 items wait to be moved in OneDrive: they are moved once OneDrive can be reached.",
                         n);
        }
        if (kind == QLatin1String("rewrite") || kind == QLatin1String("upload")) {
            return i18np("1 file waits to upload: it is uploaded once OneDrive can be reached.", "%1 files wait to upload: they are uploaded once OneDrive can be reached.", n);
        }
        return i18np("1 change waits to be sent to OneDrive: it is sent once OneDrive can be reached.",
                     "%1 changes wait to be sent to OneDrive: they are sent once OneDrive can be reached.",
                     n);
    }
    *type = QStringLiteral("positive");
    const qulonglong n = result.done;
    if (kind.isEmpty() || n != shown) {
        // Several kinds at once: what each became is on the page.
        return restore ? i18np("Restored 1 change.", "Restored %1 changes.", n) : i18np("Sending 1 change to OneDrive.", "Sending %1 changes to OneDrive.", n);
    }
    if (!restore) {
        if (kind == QLatin1String("removal")) {
            return i18np("Deleting 1 item in OneDrive: it goes to its recycle bin.", "Deleting %1 items in OneDrive: they go to its recycle bin.", n);
        }
        if (kind == QLatin1String("rewrite")) {
            return i18np("Uploading 1 changed file.", "Uploading %1 changed files.", n);
        }
        if (kind == QLatin1String("move")) {
            return i18np("Moving 1 item in OneDrive.", "Moving %1 items in OneDrive.", n);
        }
        return i18np("Uploading 1 new file.", "Uploading %1 new files.", n);
    }
    if (kind == QLatin1String("removal")) {
        return i18np("Restored 1 item: it is back in the folder.", "Restored %1 items: they are back in the folder.", n);
    }
    if (kind == QLatin1String("rewrite")) {
        return i18np("Restored 1 file to OneDrive's version. Yours is kept beside it, with this computer's name added.",
                     "Restored %1 files to OneDrive's version. Yours are kept beside them, with this computer's name added.",
                     n);
    }
    if (kind == QLatin1String("move")) {
        return i18np("Moved back 1 item.", "Moved back %1 items.", n);
    }
    return i18np("Kept 1 new file on this computer: it is not uploaded, and Issues lists it.",
                 "Kept %1 new files on this computer: they are not uploaded, and Issues lists them.",
                 n);
}

void HeldChanges::addResult(const QString &type, const QString &text)
{
    m_results.prepend(QVariantMap{{QStringLiteral("id"), ++m_lastResult}, {QStringLiteral("type"), type}, {QStringLiteral("text"), text}});
    Q_EMIT resultsChanged();
}

void HeldChanges::closeResult(int id)
{
    for (int i = 0; i < m_results.size(); ++i) {
        if (m_results.at(i).toMap().value(QStringLiteral("id")).toInt() == id) {
            m_results.removeAt(i);
            Q_EMIT resultsChanged();
            return;
        }
    }
}

void HeldChanges::uploadLocal(const QString &path)
{
    QString relative = path;
    if (!m_root.isEmpty() && path.startsWith(m_root + QLatin1Char('/'))) {
        relative = path.mid(m_root.size() + 1);
    }
    if (!m_actionError.isEmpty()) {
        m_actionError.clear();
        Q_EMIT actionErrorChanged();
    }
    auto *watcher = new QDBusPendingCallWatcher(m_interface->UploadLocal(m_id, relative), this);
    connect(watcher, &QDBusPendingCallWatcher::finished, this, [this](QDBusPendingCallWatcher *w) {
        w->deleteLater();
        const QDBusPendingReply<KonedriveGuardResult> reply = *w;
        QString error;
        if (reply.isError()) {
            error = reply.error().message();
        } else if (reply.value().failed > 0) {
            error = failureText(reply.value());
        }
        if (error != m_actionError) {
            m_actionError = error;
            Q_EMIT actionErrorChanged();
        }
    });
}
