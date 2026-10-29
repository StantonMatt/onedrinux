#include "changeguardcontroller.h"

#include "changeguardinterface.h"
#include "daemoncontroller.h"

#include <KLocalizedString>

#include <QDBusError>
#include <QDBusPendingCallWatcher>
#include <QDBusPendingReply>
#include <QDBusServiceWatcher>

#include <algorithm>

namespace
{
constexpr qulonglong GiB = 1ULL << 30;

/// The daemon's defaults, until it says (dbus/org.konedrive.ChangeGuard.xml).
KonedriveThresholds defaults()
{
    return {
        {QStringLiteral("removals"), 20},
        {QStringLiteral("removal-bytes"), GiB},
        {QStringLiteral("daily-removals"), 100},
        {QStringLiteral("rewrites"), 50},
        {QStringLiteral("rewrite-bytes"), GiB},
        {QStringLiteral("moves"), 200},
        {QStringLiteral("uploads"), 500},
        {QStringLiteral("upload-bytes"), 5 * GiB},
        {QStringLiteral("window-seconds"), 600},
        {QStringLiteral("daily-window-seconds"), 86400},
    };
}

/// A dialog's key: the daemon's key, and how many of its units one of the dialog's is.
struct Limit {
    const char *key;
    const char *daemonKey;
    qulonglong unit;
};
constexpr Limit Limits[] = {
    {"removals", "removals", 1},
    {"removalGiB", "removal-bytes", GiB},
    {"rewrites", "rewrites", 1},
    {"rewriteGiB", "rewrite-bytes", GiB},
    {"moves", "moves", 1},
    {"uploads", "uploads", 1},
    {"uploadGiB", "upload-bytes", GiB},
    {"minutes", "window-seconds", 60},
    {"dailyRemovals", "daily-removals", 1},
};

QString periodText(qulonglong seconds)
{
    if (seconds == 86400) {
        return i18nc("@info a time window: within…", "a day");
    }
    if (seconds % 3600 == 0) {
        return i18ncp("@info a time window", "1 hour", "%1 hours", seconds / 3600);
    }
    if (seconds % 60 == 0) {
        return i18ncp("@info a time window", "1 minute", "%1 minutes", seconds / 60);
    }
    return i18ncp("@info a time window", "1 second", "%1 seconds", seconds);
}
}

ChangeGuardController::ChangeGuardController(QObject *parent)
    : ChangeGuardController(QDBusConnection::sessionBus(), parent)
{
}

ChangeGuardController::ChangeGuardController(const QDBusConnection &bus, QObject *parent)
    : QObject(parent)
    , m_bus(bus)
    , m_interface(new OrgKonedriveChangeGuardInterface(DaemonController::ServiceName, DaemonController::ObjectPath, bus, this))
    , m_watcher(new QDBusServiceWatcher(DaemonController::ServiceName, bus, QDBusServiceWatcher::WatchForOwnerChange, this))
    , m_thresholds(defaults())
{
    registerKonedriveChangeGuardTypes();
    // The signal first: the settings read after it are never older than what it says.
    m_bus.connect(DaemonController::ServiceName,
                  DaemonController::ObjectPath,
                  QStringLiteral("org.freedesktop.DBus.Properties"),
                  QStringLiteral("PropertiesChanged"),
                  this,
                  SLOT(onPropertiesChanged(QString, QVariantMap, QStringList)));
    connect(m_watcher, &QDBusServiceWatcher::serviceOwnerChanged, this, [this](const QString &, const QString &, const QString &newOwner) {
        if (newOwner.isEmpty()) {
            setServiceAvailable(false);
        } else {
            fetch();
        }
    });
    fetch();
}

ChangeGuardController::~ChangeGuardController() = default;

void ChangeGuardController::fetch()
{
    auto *watcher = new QDBusPendingCallWatcher(m_interface->GetSettings(), this);
    connect(watcher, &QDBusPendingCallWatcher::finished, this, [this](QDBusPendingCallWatcher *w) {
        w->deleteLater();
        const QDBusPendingReply<bool, KonedriveThresholds> reply = *w;
        if (reply.isError()) {
            // Away, or a daemon from before the change guard.
            setServiceAvailable(false);
            return;
        }
        m_enabled = reply.argumentAt<0>();
        KonedriveThresholds thresholds = defaults();
        const KonedriveThresholds said = reply.argumentAt<1>();
        for (auto it = said.cbegin(); it != said.cend(); ++it) {
            thresholds.insert(it.key(), it.value());
        }
        m_thresholds = thresholds;
        m_serviceAvailable = true;
        Q_EMIT changed();
    });
}

void ChangeGuardController::onPropertiesChanged(const QString &interfaceName, const QVariantMap &, const QStringList &)
{
    // The limits come as a{st}: read again, all together.
    if (interfaceName == QLatin1String(OrgKonedriveChangeGuardInterface::staticInterfaceName())) {
        fetch();
    }
}

void ChangeGuardController::setServiceAvailable(bool available)
{
    if (m_serviceAvailable == available) {
        return;
    }
    m_serviceAvailable = available;
    Q_EMIT changed();
}

void ChangeGuardController::setActionError(const QString &message)
{
    if (m_actionError == message) {
        return;
    }
    m_actionError = message;
    Q_EMIT actionErrorChanged();
}

qulonglong ChangeGuardController::threshold(const QString &key) const
{
    return m_thresholds.value(key, defaults().value(key));
}

QVariantMap ChangeGuardController::limits() const
{
    QVariantMap limits;
    for (const Limit &limit : Limits) {
        const qulonglong value = threshold(QLatin1String(limit.daemonKey));
        // Rounded to the dialog's unit, and at least 1, as its boxes take them.
        limits.insert(QLatin1String(limit.key), std::max<qulonglong>(1, (value + limit.unit / 2) / limit.unit));
    }
    return limits;
}

QString ChangeGuardController::windowText() const
{
    return periodText(threshold(QStringLiteral("window-seconds")));
}

QString ChangeGuardController::dailyWindowText() const
{
    return periodText(threshold(QStringLiteral("daily-window-seconds")));
}

QString ChangeGuardController::description() const
{
    return i18nc("@info the limits: deletions, changed files, moves, new files, the window",
                 "More than %1, %2, %3 or %4 within %5 wait for your review before they're sent to OneDrive.",
                 i18ncp("@info a limit", "%1 deletion", "%1 deletions", threshold(QStringLiteral("removals"))),
                 i18ncp("@info a limit", "%1 changed file", "%1 changed files", threshold(QStringLiteral("rewrites"))),
                 i18ncp("@info a limit", "%1 move", "%1 moves", threshold(QStringLiteral("moves"))),
                 i18ncp("@info a limit", "%1 new file", "%1 new files", threshold(QStringLiteral("uploads"))),
                 windowText());
}

void ChangeGuardController::call(const QDBusPendingCall &pending)
{
    setActionError(QString());
    auto *watcher = new QDBusPendingCallWatcher(pending, this);
    connect(watcher, &QDBusPendingCallWatcher::finished, this, [this](QDBusPendingCallWatcher *w) {
        w->deleteLater();
        if (w->isError()) {
            setActionError(w->error().message());
            // Nothing changed: the controls show what the daemon has.
            fetch();
        }
    });
}

void ChangeGuardController::setEnabled(bool on)
{
    call(m_interface->SetEnabled(on));
}

void ChangeGuardController::setLimits(const QVariantMap &wanted)
{
    const QVariantMap shown = limits();
    KonedriveThresholds thresholds;
    for (const Limit &limit : Limits) {
        const QString key = QLatin1String(limit.key);
        if (!wanted.contains(key)) {
            continue;
        }
        const qulonglong value = wanted.value(key).toULongLong();
        if (value == shown.value(key).toULongLong()) {
            continue;
        }
        thresholds.insert(QLatin1String(limit.daemonKey), value * limit.unit);
    }
    if (thresholds.isEmpty()) {
        return;
    }
    call(m_interface->SetThresholds(thresholds));
}
