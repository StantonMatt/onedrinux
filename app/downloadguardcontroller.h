#pragma once

#include <QDBusConnection>
#include <QHash>
#include <QList>
#include <QMetaType>
#include <QObject>
#include <QString>
#include <QVariantList>

#include <functional>

class QDBusMessage;
class QDBusPendingCall;
class QDBusServiceWatcher;

/// One app the download guard knows (a row of ListApps), as the window shows it.
struct GuardApp {
    /// `app:<desktop-id>`, `exe:<absolute path>`, or `unidentified`.
    QString identity;
    /// What the window calls it: the desktop entry's name, else the daemon's display name,
    /// cleaned of control characters. Plain text, never markup: the daemon's name is the
    /// program's own say.
    QString name;
    QString iconName;
    /// automatic, allowed-session, allowed-always or blocked-always.
    QString policy;
    bool blocked = false;
    /// Why it is blocked: bulk, background, user, unidentified, or empty.
    QString reason;
    /// What it was let download in the window (at its refusal, once blocked).
    qulonglong files = 0;
    qulonglong bytes = 0;
};
Q_DECLARE_METATYPE(GuardApp)

/// konedrived's download guard (dbus/org.konedrive.DownloadGuard.xml), on the manager object:
/// whether it is on, its limits, and the apps it blocked or has a rule for. One for the whole
/// app, since the guard counts each app across every account. Its signals are taken before
/// the list is read, and the list is read again after each of them and when the daemon comes
/// back: the list is what the daemon says, whatever a signal missed. Never blocks the GUI
/// thread.
class DownloadGuardController : public QObject
{
    Q_OBJECT
    /// The daemon answers for the guard; false while it is away, or too old to have one.
    Q_PROPERTY(bool serviceAvailable READ serviceAvailable NOTIFY changed)
    Q_PROPERTY(bool enabled READ enabled NOTIFY changed)
    Q_PROPERTY(uint maxFiles READ maxFiles NOTIFY changed)
    Q_PROPERTY(qulonglong maxBytes READ maxBytes NOTIFY changed)
    Q_PROPERTY(uint windowSeconds READ windowSeconds NOTIFY changed)
    /// The limits as the limits dialog takes them: whole GiB and whole minutes, at least 1.
    Q_PROPERTY(uint maxGiB READ maxGiB NOTIFY changed)
    Q_PROPERTY(uint windowMinutes READ windowMinutes NOTIFY changed)
    /// The switch's description, with the limits in it.
    Q_PROPERTY(QString description READ description NOTIFY changed)
    /// Settings' rows, in order: blocked (the newest first), allowed for this session, always
    /// allowed, always blocked, by name within each. Each a map: identity, name, iconName,
    /// state (blocked, unidentified, allowed-session, allowed-always, blocked-always) and
    /// stateText. An app counted as usual, and a reader the guard always refuses, have none.
    Q_PROPERTY(QVariantList apps READ apps NOTIFY appsChanged)
    /// Status' rows: those of `apps` that are blocked and wait for the user (blocked,
    /// unidentified).
    Q_PROPERTY(QVariantList blockedApps READ blockedApps NOTIFY appsChanged)
    /// Why the last call asked from the window was refused.
    Q_PROPERTY(QString actionError READ actionError NOTIFY actionErrorChanged)
    /// What the last Free Up What It Downloaded asked from the window did, until it is cleared.
    Q_PROPERTY(QString freeUpResult READ freeUpResult NOTIFY freeUpChanged)
    /// The app a Free Up is under way for; empty when none is.
    Q_PROPERTY(QString freeingUp READ freeingUp NOTIFY freeUpChanged)

public:
    static const QString InterfaceName;
    static const QString Unidentified;

    /// What the window calls an app and its icon, from its identity and the daemon's name.
    using Resolver = std::function<GuardApp(const QString &identity, const QString &displayName)>;

    explicit DownloadGuardController(QObject *parent = nullptr);
    explicit DownloadGuardController(const QDBusConnection &bus, QObject *parent = nullptr);

    /// `app:` by its desktop entry (KService), `exe:` by its file's name, and the program that
    /// could not be identified as "Unknown program"; only `name` and `iconName` are set.
    static GuardApp resolve(const QString &identity, const QString &displayName);
    /// A name the daemon passed on, fit to show: no control or direction characters, one line,
    /// at most 80 characters.
    static QString cleanName(const QString &name);

    bool serviceAvailable() const { return m_serviceAvailable; }
    bool enabled() const { return m_enabled; }
    uint maxFiles() const { return m_maxFiles; }
    qulonglong maxBytes() const { return m_maxBytes; }
    uint windowSeconds() const { return m_windowSeconds; }
    uint maxGiB() const;
    uint windowMinutes() const;
    QString description() const;
    QVariantList apps() const;
    QVariantList blockedApps() const;
    QString actionError() const { return m_actionError; }
    QString freeUpResult() const { return m_freeUpResult; }
    QString freeingUp() const { return m_freeingUp; }

    /// The app as last listed, or as its last DownloadsBlocked said; nullptr when unknown.
    const GuardApp *app(const QString &identity) const;
    /// The apps as last listed.
    QList<GuardApp> listed() const { return m_apps; }
    /// What a row says of `app`: blocked, unidentified, allowed-session, allowed-always,
    /// blocked-always; empty for an app counted as usual or a reader always refused.
    QString stateOf(const GuardApp &app) const;

    /// "30 files", "2 GiB", "10 minutes": the limits in words.
    QString filesText(qulonglong files) const;
    static QString bytesText(qulonglong bytes);
    QString windowText() const;

    /// Tests name apps without the desktop's entries.
    void setResolver(Resolver resolver) { m_resolver = std::move(resolver); }

    /// SetEnabled; a refusal lands in actionError.
    Q_INVOKABLE void setEnabled(bool on);
    /// SetThresholds from the limits dialog. A number left as maxGiB or windowMinutes shows it
    /// keeps the daemon's exact bytes or seconds, which need not be whole.
    Q_INVOKABLE void setLimits(uint files, uint gib, uint minutes);
    /// AllowApp, BlockApp, ResetApp from the window; a refusal lands in actionError.
    Q_INVOKABLE void allowApp(const QString &identity, bool always);
    Q_INVOKABLE void blockApp(const QString &identity);
    Q_INVOKABLE void resetApp(const QString &identity);
    /// FreeUpAppDownloads from the window: what it did lands in freeUpResult. One at a time.
    Q_INVOKABLE void freeUpAppDownloads(const QString &identity);
    Q_INVOKABLE void clearFreeUpResult();

    /// The same for the notifications: `failed` gets the daemon's reason; actionError stays.
    void allowApp(const QString &identity, bool always, std::function<void(const QString &)> failed);
    void blockApp(const QString &identity, std::function<void(const QString &)> failed);
    void freeUpAppDownloads(const QString &identity, std::function<void(qulonglong, qulonglong)> done, std::function<void(const QString &)> failed);

    /// The words for what FreeUpAppDownloads freed.
    static QString freedText(qulonglong files, qulonglong bytes);

Q_SIGNALS:
    void changed();
    /// The list was read anew.
    void appsChanged();
    void actionErrorChanged();
    void freeUpChanged();
    /// DownloadsBlocked: `app` with the name and icon the window gives it.
    void downloadsBlocked(const GuardApp &app);

private Q_SLOTS:
    void onPropertiesChanged(const QString &interfaceName, const QVariantMap &changed, const QStringList &invalidated);
    void onDownloadsBlocked(const QString &identity, const QString &displayName, qulonglong files, qulonglong bytes, const QString &reason);
    void onAppsChanged();

private:
    void fetchAll();
    void listApps();
    void takeApps(const QDBusMessage &reply);
    void applyProperties(const QVariantMap &properties);
    void setServiceAvailable(bool available);
    void setActionError(const QString &message);
    void call(const QString &method, const QVariantList &arguments, std::function<void(const QString &)> failed);
    void watch(const QDBusPendingCall &pending, std::function<void(const QDBusMessage &)> done, std::function<void(const QString &)> failed);
    GuardApp named(const QString &identity, const QString &displayName) const;
    /// Where a row goes, as a map for QML; empty for an app with none.
    QVariantMap row(const GuardApp &app) const;
    QString stateText(const GuardApp &app, const QString &state) const;
    QList<GuardApp> ordered() const;

    QDBusConnection m_bus;
    QDBusServiceWatcher *m_watcher;
    Resolver m_resolver;
    /// What each identity and display name was resolved to: KService is asked once.
    mutable QHash<QString, GuardApp> m_named;
    bool m_serviceAvailable = false;
    bool m_enabled = true;
    uint m_maxFiles = 30;
    qulonglong m_maxBytes = 2ULL << 30;
    uint m_windowSeconds = 600;
    QList<GuardApp> m_apps;
    /// When each app's last DownloadsBlocked came, in order: the newest block first.
    QHash<QString, quint64> m_blockedOrder;
    quint64 m_lastBlock = 0;
    /// The last ListApps asked: an answer to an older one is dropped.
    quint64 m_listAsked = 0;
    QString m_actionError;
    QString m_freeUpResult;
    QString m_freeingUp;
};
