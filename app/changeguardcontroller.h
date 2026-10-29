#pragma once

#include "changeguardtypes.h"

#include <QDBusConnection>
#include <QObject>
#include <QString>
#include <QVariantMap>

#include <functional>

class OrgKonedriveChangeGuardInterface;
class QDBusServiceWatcher;

/// konedrived's change guard settings (dbus/org.konedrive.ChangeGuard.xml), on the manager
/// object: whether large changes are held, and the limits. The daemon's, for every account and
/// every one added later: the controls show what it says, and a change goes through it. What
/// each account holds is its HeldChanges. Never blocks the GUI thread.
class ChangeGuardController : public QObject
{
    Q_OBJECT
    /// The daemon answers for the change guard; false while it is away, or too old to have one.
    Q_PROPERTY(bool serviceAvailable READ serviceAvailable NOTIFY changed)
    Q_PROPERTY(bool enabled READ enabled NOTIFY changed)
    /// The limits as the limits dialog takes them, at least 1 each: removals, removalGiB,
    /// rewrites, rewriteGiB, moves, uploads, uploadGiB, minutes, dailyRemovals.
    Q_PROPERTY(QVariantMap limits READ limits NOTIFY changed)
    /// The switch's description, with the limits in it.
    Q_PROPERTY(QString description READ description NOTIFY changed)
    /// Why the last call asked from the window was refused.
    Q_PROPERTY(QString actionError READ actionError NOTIFY actionErrorChanged)

public:
    explicit ChangeGuardController(QObject *parent = nullptr);
    explicit ChangeGuardController(const QDBusConnection &bus, QObject *parent = nullptr);
    ~ChangeGuardController() override;

    bool serviceAvailable() const { return m_serviceAvailable; }
    bool enabled() const { return m_enabled; }
    QVariantMap limits() const;
    QString description() const;
    QString actionError() const { return m_actionError; }

    /// The daemon's limit by its key (removals, removal-bytes, window-seconds…), defaults resolved.
    qulonglong threshold(const QString &key) const;
    /// "10 minutes": the window the limits count in.
    QString windowText() const;
    /// "a day": the window of the daily deletion limit.
    QString dailyWindowText() const;

    /// SetEnabled; a refusal lands in actionError. Turning it off never releases what is held.
    Q_INVOKABLE void setEnabled(bool on);
    /// SetThresholds from the limits dialog, with `limits`' keys. A number the same as shown
    /// in `limits` is not sent, so that the daemon keeps its exact value (bytes and seconds
    /// need not be whole GiB or minutes).
    Q_INVOKABLE void setLimits(const QVariantMap &limits);

Q_SIGNALS:
    void changed();
    void actionErrorChanged();

private Q_SLOTS:
    void onPropertiesChanged(const QString &interfaceName, const QVariantMap &changed, const QStringList &invalidated);

private:
    void fetch();
    void setServiceAvailable(bool available);
    void setActionError(const QString &message);
    void call(const QDBusPendingCall &pending);

    QDBusConnection m_bus;
    OrgKonedriveChangeGuardInterface *m_interface;
    QDBusServiceWatcher *m_watcher;
    bool m_serviceAvailable = false;
    bool m_enabled = true;
    KonedriveThresholds m_thresholds;
    QString m_actionError;
};
