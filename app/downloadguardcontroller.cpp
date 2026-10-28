#include "downloadguardcontroller.h"

#include "daemoncontroller.h"

#include <KLocalizedString>
#include <KService>

#include <QDBusArgument>
#include <QDBusError>
#include <QDBusMessage>
#include <QDBusPendingCallWatcher>
#include <QDBusReply>
#include <QDBusServiceWatcher>
#include <QFileInfo>
#include <QLocale>

#include <algorithm>
#include <limits>

const QString DownloadGuardController::InterfaceName = QStringLiteral("org.konedrive.DownloadGuard");
const QString DownloadGuardController::Unidentified = QStringLiteral("unidentified");

namespace
{
constexpr qulonglong GiB = 1ULL << 30;
constexpr int NameLength = 80;

QDBusMessage guardCall(const QString &method)
{
    return QDBusMessage::createMethodCall(DaemonController::ServiceName, DaemonController::ObjectPath, DownloadGuardController::InterfaceName, method);
}

/// Where a row's state puts it in Settings' list.
int rank(const QString &state)
{
    static const QStringList order = {QStringLiteral("blocked"),
                                      QStringLiteral("unidentified"),
                                      QStringLiteral("allowed-session"),
                                      QStringLiteral("allowed-always"),
                                      QStringLiteral("blocked-always")};
    return int(order.indexOf(state));
}
}

DownloadGuardController::DownloadGuardController(QObject *parent)
    : DownloadGuardController(QDBusConnection::sessionBus(), parent)
{
}

DownloadGuardController::DownloadGuardController(const QDBusConnection &bus, QObject *parent)
    : QObject(parent)
    , m_bus(bus)
    , m_watcher(new QDBusServiceWatcher(DaemonController::ServiceName, bus, QDBusServiceWatcher::WatchForOwnerChange, this))
{
    // The signals first: the list read after them is never older than what they say.
    m_bus.connect(DaemonController::ServiceName,
                  DaemonController::ObjectPath,
                  QStringLiteral("org.freedesktop.DBus.Properties"),
                  QStringLiteral("PropertiesChanged"),
                  this,
                  SLOT(onPropertiesChanged(QString, QVariantMap, QStringList)));
    m_bus.connect(DaemonController::ServiceName,
                  DaemonController::ObjectPath,
                  InterfaceName,
                  QStringLiteral("DownloadsBlocked"),
                  this,
                  SLOT(onDownloadsBlocked(QString, QString, qulonglong, qulonglong, QString)));
    m_bus.connect(DaemonController::ServiceName, DaemonController::ObjectPath, InterfaceName, QStringLiteral("AppsChanged"), this, SLOT(onAppsChanged()));
    connect(m_watcher, &QDBusServiceWatcher::serviceOwnerChanged, this, [this](const QString &, const QString &, const QString &newOwner) {
        if (newOwner.isEmpty()) {
            setServiceAvailable(false);
        } else {
            fetchAll();
        }
    });
    fetchAll();
}

QString DownloadGuardController::cleanName(const QString &name)
{
    QString clean;
    clean.reserve(name.size());
    for (const QChar c : name) {
        switch (c.category()) {
        case QChar::Other_Control:
        case QChar::Other_Format:
        case QChar::Separator_Line:
        case QChar::Separator_Paragraph:
            clean += QLatin1Char(' ');
            break;
        default:
            clean += c;
        }
    }
    clean = clean.simplified();
    if (clean.size() > NameLength) {
        int cut = NameLength - 1;
        if (clean.at(cut - 1).isHighSurrogate()) {
            --cut;
        }
        clean = clean.left(cut) + QChar(0x2026);
    }
    return clean;
}

GuardApp DownloadGuardController::resolve(const QString &identity, const QString &displayName)
{
    GuardApp app;
    app.identity = identity;
    app.iconName = QStringLiteral("application-x-executable");
    if (identity == Unidentified) {
        app.name = i18nc("@info a program the download guard could not identify", "Unknown program");
        return app;
    }
    if (identity.startsWith(QLatin1String("app:"))) {
        if (const KService::Ptr service = KService::serviceByDesktopName(identity.mid(4))) {
            app.name = cleanName(service->name());
            if (!service->icon().isEmpty()) {
                app.iconName = service->icon();
            }
        }
    }
    if (app.name.isEmpty()) {
        app.name = cleanName(displayName);
    }
    if (app.name.isEmpty()) {
        const QString rest = identity.section(QLatin1Char(':'), 1);
        app.name = cleanName(identity.startsWith(QLatin1String("exe:")) ? QFileInfo(rest).fileName() : rest);
    }
    return app;
}

GuardApp DownloadGuardController::named(const QString &identity, const QString &displayName) const
{
    const QString key = identity + QChar(0) + displayName;
    if (const auto it = m_named.constFind(key); it != m_named.constEnd()) {
        return *it;
    }
    GuardApp app = m_resolver ? m_resolver(identity, displayName) : resolve(identity, displayName);
    app.identity = identity;
    m_named.insert(key, app);
    return app;
}

void DownloadGuardController::fetchAll()
{
    auto message = QDBusMessage::createMethodCall(DaemonController::ServiceName,
                                                  DaemonController::ObjectPath,
                                                  QStringLiteral("org.freedesktop.DBus.Properties"),
                                                  QStringLiteral("GetAll"));
    message << InterfaceName;
    watch(
        m_bus.asyncCall(message),
        [this](const QDBusMessage &reply) {
            applyProperties(QDBusReply<QVariantMap>(reply).value());
            setServiceAvailable(true);
            listApps();
        },
        // Away, or a daemon from before the guard.
        [this](const QString &) {
            setServiceAvailable(false);
        });
}

void DownloadGuardController::listApps()
{
    const quint64 asked = ++m_listAsked;
    watch(
        m_bus.asyncCall(guardCall(QStringLiteral("ListApps"))),
        [this, asked](const QDBusMessage &reply) {
            if (asked == m_listAsked) {
                takeApps(reply);
            }
        },
        {});
}

void DownloadGuardController::takeApps(const QDBusMessage &reply)
{
    if (reply.signature() != QLatin1String("a(sssbstt)")) {
        return;
    }
    QList<GuardApp> apps;
    const QDBusArgument argument = reply.arguments().constFirst().value<QDBusArgument>();
    argument.beginArray();
    while (!argument.atEnd()) {
        QString identity;
        QString displayName;
        QString policy;
        bool blocked = false;
        QString reason;
        qulonglong files = 0;
        qulonglong bytes = 0;
        argument.beginStructure();
        argument >> identity >> displayName >> policy >> blocked >> reason >> files >> bytes;
        argument.endStructure();
        GuardApp app = named(identity, displayName);
        app.policy = policy;
        app.blocked = blocked;
        app.reason = reason;
        app.files = files;
        app.bytes = bytes;
        apps << app;
    }
    argument.endArray();
    m_apps = apps;
    // A block that is over takes its place in the order with it.
    for (auto it = m_blockedOrder.begin(); it != m_blockedOrder.end();) {
        const GuardApp *known = app(it.key());
        it = known && stateOf(*known) == QLatin1String("blocked") ? std::next(it) : m_blockedOrder.erase(it);
    }
    Q_EMIT appsChanged();
}

void DownloadGuardController::onPropertiesChanged(const QString &interfaceName, const QVariantMap &changed, const QStringList &invalidated)
{
    if (interfaceName != InterfaceName) {
        return;
    }
    applyProperties(changed);
    if (!invalidated.isEmpty()) {
        fetchAll();
    }
}

void DownloadGuardController::applyProperties(const QVariantMap &p)
{
    if (const auto it = p.constFind(QStringLiteral("Enabled")); it != p.constEnd()) {
        m_enabled = it->toBool();
    }
    const uint files = m_maxFiles;
    const qulonglong bytes = m_maxBytes;
    if (const auto it = p.constFind(QStringLiteral("MaxFiles")); it != p.constEnd()) {
        m_maxFiles = it->toUInt();
    }
    if (const auto it = p.constFind(QStringLiteral("MaxBytes")); it != p.constEnd()) {
        m_maxBytes = it->toULongLong();
    }
    if (const auto it = p.constFind(QStringLiteral("WindowSeconds")); it != p.constEnd()) {
        m_windowSeconds = it->toUInt();
    }
    Q_EMIT changed();
    // "Blocked after 30 files" or "after 2 GiB" is chosen by the limits.
    if ((files != m_maxFiles || bytes != m_maxBytes) && !m_apps.isEmpty()) {
        Q_EMIT appsChanged();
    }
}

void DownloadGuardController::onDownloadsBlocked(const QString &identity, const QString &displayName, qulonglong files, qulonglong bytes, const QString &reason)
{
    GuardApp blocked = named(identity, displayName);
    const GuardApp *known = app(identity);
    blocked.policy = known ? known->policy : QStringLiteral("automatic");
    blocked.blocked = true;
    blocked.reason = reason;
    blocked.files = files;
    blocked.bytes = bytes;
    m_blockedOrder.insert(identity, ++m_lastBlock);
    // In the list at once, until it is read again: what reads it meanwhile sees the block.
    const auto it = std::find_if(m_apps.begin(), m_apps.end(), [&identity](const GuardApp &app) {
        return app.identity == identity;
    });
    if (it != m_apps.end()) {
        *it = blocked;
    } else {
        m_apps << blocked;
    }
    Q_EMIT downloadsBlocked(blocked);
    Q_EMIT appsChanged();
    listApps();
}

void DownloadGuardController::onAppsChanged()
{
    listApps();
}

void DownloadGuardController::setServiceAvailable(bool available)
{
    if (m_serviceAvailable == available) {
        return;
    }
    m_serviceAvailable = available;
    if (!available) {
        // Blocks, allowances for this session and what was counted go with the daemon.
        ++m_listAsked;
        m_apps.clear();
        m_blockedOrder.clear();
        m_freeingUp.clear();
        Q_EMIT freeUpChanged();
        Q_EMIT appsChanged();
    }
    Q_EMIT changed();
}

void DownloadGuardController::setActionError(const QString &message)
{
    if (m_actionError == message) {
        return;
    }
    m_actionError = message;
    Q_EMIT actionErrorChanged();
}

void DownloadGuardController::watch(const QDBusPendingCall &pending, std::function<void(const QDBusMessage &)> done, std::function<void(const QString &)> failed)
{
    auto *watcher = new QDBusPendingCallWatcher(pending, this);
    connect(watcher, &QDBusPendingCallWatcher::finished, this, [done, failed](QDBusPendingCallWatcher *w) {
        w->deleteLater();
        if (w->isError()) {
            if (failed) {
                failed(w->error().message());
            }
        } else if (done) {
            done(w->reply());
        }
    });
}

void DownloadGuardController::call(const QString &method, const QVariantList &arguments, std::function<void(const QString &)> failed)
{
    QDBusMessage message = guardCall(method);
    message.setArguments(arguments);
    watch(m_bus.asyncCall(message), {}, [this, failed](const QString &error) {
        // Nothing changed: the rows show what the daemon has.
        listApps();
        if (failed) {
            failed(error);
        }
    });
}

void DownloadGuardController::setEnabled(bool on)
{
    setActionError(QString());
    call(QStringLiteral("SetEnabled"), {on}, [this](const QString &error) {
        setActionError(error);
    });
}

void DownloadGuardController::setLimits(uint files, uint gib, uint minutes)
{
    const qulonglong bytes = gib == maxGiB() ? m_maxBytes : qulonglong(gib) * GiB;
    const uint seconds = minutes == windowMinutes() ? m_windowSeconds : minutes * 60;
    setActionError(QString());
    call(QStringLiteral("SetThresholds"), {QVariant::fromValue(files), QVariant::fromValue(bytes), QVariant::fromValue(seconds)}, [this](const QString &error) {
        setActionError(error);
    });
}

void DownloadGuardController::allowApp(const QString &identity, bool always)
{
    setActionError(QString());
    allowApp(identity, always, [this](const QString &error) {
        setActionError(error);
    });
}

void DownloadGuardController::allowApp(const QString &identity, bool always, std::function<void(const QString &)> failed)
{
    call(QStringLiteral("AllowApp"), {identity, always}, std::move(failed));
}

void DownloadGuardController::blockApp(const QString &identity)
{
    setActionError(QString());
    blockApp(identity, [this](const QString &error) {
        setActionError(error);
    });
}

void DownloadGuardController::blockApp(const QString &identity, std::function<void(const QString &)> failed)
{
    call(QStringLiteral("BlockApp"), {identity}, std::move(failed));
}

void DownloadGuardController::resetApp(const QString &identity)
{
    setActionError(QString());
    call(QStringLiteral("ResetApp"), {identity}, [this](const QString &error) {
        setActionError(error);
    });
}

void DownloadGuardController::freeUpAppDownloads(const QString &identity)
{
    if (!m_freeingUp.isEmpty()) {
        return;
    }
    m_freeingUp = identity;
    m_freeUpResult.clear();
    Q_EMIT freeUpChanged();
    freeUpAppDownloads(
        identity,
        [this](qulonglong files, qulonglong bytes) {
            m_freeingUp.clear();
            m_freeUpResult = freedText(files, bytes);
            Q_EMIT freeUpChanged();
        },
        [this](const QString &error) {
            m_freeingUp.clear();
            m_freeUpResult = i18n("Could not free up space: %1", error);
            Q_EMIT freeUpChanged();
        });
}

void DownloadGuardController::freeUpAppDownloads(const QString &identity,
                                                 std::function<void(qulonglong, qulonglong)> done,
                                                 std::function<void(const QString &)> failed)
{
    QDBusMessage message = guardCall(QStringLiteral("FreeUpAppDownloads"));
    message << identity;
    // Freeing many copies can take minutes, as Free Up Space does: no D-Bus timeout.
    watch(
        m_bus.asyncCall(message, std::numeric_limits<int>::max()),
        [done](const QDBusMessage &reply) {
            const QList<QVariant> values = reply.arguments();
            if (done) {
                done(values.value(0).toULongLong(), values.value(1).toULongLong());
            }
        },
        std::move(failed));
}

void DownloadGuardController::clearFreeUpResult()
{
    if (m_freeUpResult.isEmpty()) {
        return;
    }
    m_freeUpResult.clear();
    Q_EMIT freeUpChanged();
}

QString DownloadGuardController::freedText(qulonglong files, qulonglong bytes)
{
    if (files == 0) {
        return i18n("Nothing was freed up. Files that are open, changed or kept on this device stay downloaded.");
    }
    return i18np("Freed 1 file (%2).", "Freed %1 files (%2).", files, QLocale().formattedDataSize(qint64(bytes)));
}

uint DownloadGuardController::maxGiB() const
{
    return uint(std::max<qulonglong>(1, (m_maxBytes + GiB / 2) / GiB));
}

uint DownloadGuardController::windowMinutes() const
{
    return std::max<uint>(1, (m_windowSeconds + 30) / 60);
}

QString DownloadGuardController::filesText(qulonglong files) const
{
    return i18np("1 file", "%1 files", files);
}

QString DownloadGuardController::bytesText(qulonglong bytes)
{
    // "2 GiB" rather than "2.00 GiB" for a limit in whole GiB.
    return bytes % GiB == 0 ? QLocale().formattedDataSize(qint64(bytes), 0) : QLocale().formattedDataSize(qint64(bytes));
}

QString DownloadGuardController::windowText() const
{
    if (m_windowSeconds % 60 == 0) {
        return i18np("1 minute", "%1 minutes", m_windowSeconds / 60);
    }
    return i18np("1 second", "%1 seconds", m_windowSeconds);
}

QString DownloadGuardController::description() const
{
    return i18n("Indexing and previews never download files. An app that downloads more than %1 or %2 within %3 is blocked until you allow it.",
                filesText(m_maxFiles),
                bytesText(m_maxBytes),
                windowText());
}

const GuardApp *DownloadGuardController::app(const QString &identity) const
{
    for (const GuardApp &app : m_apps) {
        if (app.identity == identity) {
            return &app;
        }
    }
    return nullptr;
}

QString DownloadGuardController::stateOf(const GuardApp &app) const
{
    if (app.identity == Unidentified || app.reason == QLatin1String("unidentified")) {
        return app.blocked ? QStringLiteral("unidentified") : QString();
    }
    // Always Block keeps the block the limits set, under the rule that now holds.
    if (app.policy == QLatin1String("blocked-always")) {
        return app.policy;
    }
    if (app.blocked && app.reason == QLatin1String("bulk")) {
        return QStringLiteral("blocked");
    }
    // A rule shows, also for a reader that is refused whatever it says: it can be removed.
    if (app.policy == QLatin1String("allowed-session") || app.policy == QLatin1String("allowed-always")) {
        return app.policy;
    }
    return QString();
}

QString DownloadGuardController::stateText(const GuardApp &app, const QString &state) const
{
    if (state == QLatin1String("blocked")) {
        if (app.files >= m_maxFiles) {
            return i18np("Blocked after 1 file", "Blocked after %1 files", app.files);
        }
        if (app.bytes > 0) {
            return i18nc("@info the amount it downloaded", "Blocked after %1", QLocale().formattedDataSize(qint64(app.bytes)));
        }
        // One file larger than the limit on its own: refused before anything was counted.
        return i18nc("@info the byte limit", "Blocked at a file larger than %1", bytesText(m_maxBytes));
    }
    if (state == QLatin1String("unidentified")) {
        return i18n("Blocked: it couldn't be identified");
    }
    if (state == QLatin1String("allowed-session")) {
        return i18n("Allowed for this session");
    }
    if (state == QLatin1String("allowed-always")) {
        return i18n("Always allowed");
    }
    if (state == QLatin1String("blocked-always")) {
        return i18n("Always blocked");
    }
    return QString();
}

QVariantMap DownloadGuardController::row(const GuardApp &app) const
{
    const QString state = stateOf(app);
    if (state.isEmpty()) {
        return {};
    }
    return {
        {QStringLiteral("identity"), app.identity},
        {QStringLiteral("name"), app.name},
        {QStringLiteral("iconName"), app.iconName},
        {QStringLiteral("state"), state},
        {QStringLiteral("stateText"), stateText(app, state)},
    };
}

QList<GuardApp> DownloadGuardController::ordered() const
{
    QList<GuardApp> shown;
    for (const GuardApp &app : m_apps) {
        if (!stateOf(app).isEmpty()) {
            shown << app;
        }
    }
    std::stable_sort(shown.begin(), shown.end(), [this](const GuardApp &a, const GuardApp &b) {
        const int ra = rank(stateOf(a));
        const int rb = rank(stateOf(b));
        if (ra != rb) {
            return ra < rb;
        }
        const quint64 na = m_blockedOrder.value(a.identity);
        const quint64 nb = m_blockedOrder.value(b.identity);
        if (na != nb) {
            return na > nb;
        }
        return QString::localeAwareCompare(a.name, b.name) < 0;
    });
    return shown;
}

QVariantList DownloadGuardController::apps() const
{
    QVariantList rows;
    for (const GuardApp &app : ordered()) {
        rows << row(app);
    }
    return rows;
}

QVariantList DownloadGuardController::blockedApps() const
{
    QVariantList rows;
    for (const GuardApp &app : ordered()) {
        const QString state = stateOf(app);
        if (state == QLatin1String("blocked") || state == QLatin1String("unidentified")) {
            rows << row(app);
        }
    }
    return rows;
}
