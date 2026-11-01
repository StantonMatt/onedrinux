#pragma once

#include <QDBusConnection>
#include <QDBusPendingCall>
#include <QObject>
#include <QString>
#include <QStringList>
#include <QVariantMap>

#include <functional>
#include <optional>

class OrgKonedriveAccountInterface;
class QDBusServiceWatcher;

/// Presents one account's org.konedrive.Account (at /org/konedrive/Accounts/<id>)
/// to QML. Never blocks the GUI thread.
class AccountController : public QObject
{
    Q_OBJECT
    Q_PROPERTY(bool serviceAvailable READ serviceAvailable NOTIFY serviceAvailableChanged)
    Q_PROPERTY(QString path READ path CONSTANT)
    /// The last element of the path: 12 lowercase hex characters.
    Q_PROPERTY(QString id READ id CONSTANT)
    /// What people see and type; the id until the daemon has said.
    Q_PROPERTY(QString label READ label NOTIFY accountChanged)
    /// The mode the account runs in (Account.Mode): "read-only", or "read-write" while
    /// changes made on this computer are uploaded.
    Q_PROPERTY(QString mode READ mode NOTIFY accountChanged)
    /// Microsoft still grants write permission while signed in and running read-only.
    /// False when the grant is unknown; presentation also checks serviceAvailable.
    Q_PROPERTY(bool writeStillGranted READ writeStillGranted NOTIFY accountChanged)
    /// The mode a switch under way goes to, empty when none is: from setMode() until the
    /// daemon has answered and its properties have been read again, and for "read-write"
    /// on while its sign-in waits in the browser (modeSignInPending).
    Q_PROPERTY(QString switchingTo READ switchingTo NOTIFY modeSwitchChanged)
    /// A switch to read-write waits for its sign-in, whose link is signInUrl: until Mode
    /// turns "read-write", LastError says why not, the account leaves "signed-in", or
    /// cancelModeSwitch(). Account says nothing while it waits.
    Q_PROPERTY(bool modeSignInPending READ modeSignInPending NOTIFY modeSwitchChanged)
    Q_PROPERTY(QString state READ state NOTIFY accountChanged)
    Q_PROPERTY(QString lastError READ lastError NOTIFY accountChanged)
    /// The account keeps a wish for read-only (Account.ReadOnlyWanted): it is read-write
    /// still, sends what is unsent, and switches by itself when nothing is.
    /// setMode("read-only", true) switches at once; setMode("read-write") takes the wish back.
    Q_PROPERTY(bool switchingToReadOnly READ switchingToReadOnly NOTIFY accountChanged)
    /// While switchingToReadOnly: how many changes are still to send (Account.UnsentBeforeReadOnly).
    Q_PROPERTY(qulonglong unsentCount READ unsentCount NOTIFY accountChanged)
    Q_PROPERTY(QString displayName READ displayName NOTIFY accountChanged)
    Q_PROPERTY(QString email READ email NOTIFY accountChanged)
    Q_PROPERTY(qulonglong quotaUsed READ quotaUsed NOTIFY accountChanged)
    Q_PROPERTY(qulonglong quotaTotal READ quotaTotal NOTIFY accountChanged)
    Q_PROPERTY(QString signInUrl READ signInUrl NOTIFY signInUrlChanged)
    Q_PROPERTY(QString actionError READ actionError NOTIFY actionErrorChanged)
    /// A trial (Account.WriteScopeEnabled): only writePaths can be changed on this computer.
    Q_PROPERTY(bool writeScopeEnabled READ writeScopeEnabled NOTIFY accountChanged)
    /// The trial's folders (Account.WritePaths), relative to the drive root; empty without one.
    Q_PROPERTY(QStringList writePaths READ writePaths NOTIFY accountChanged)
    /// A SetWriteScope or ClearWriteScope asked from this window has not answered yet.
    Q_PROPERTY(bool writeScopeChanging READ writeScopeChanging NOTIFY writeScopeChangingChanged)

public:
    static const QString ServiceName;
    static const QString InterfaceName;

    explicit AccountController(const QString &path, QObject *parent = nullptr);
    AccountController(const QDBusConnection &bus, const QString &path, QObject *parent = nullptr);

    bool serviceAvailable() const { return m_serviceAvailable; }
    QString path() const { return m_path; }
    QString id() const { return m_id; }
    QString label() const { return m_label.isEmpty() ? m_id : m_label; }
    QString mode() const { return m_mode; }
    bool writeStillGranted() const { return m_writeStillGranted; }
    QString switchingTo() const { return m_switchingTo; }
    bool modeSignInPending() const { return m_switchingTo == QLatin1String("read-write") && !m_signInUrl.isEmpty(); }
    QString state() const { return m_state; }
    QString lastError() const { return m_lastError; }
    bool switchingToReadOnly() const { return m_switchingToReadOnly; }
    qulonglong unsentCount() const { return m_unsentCount; }
    QString displayName() const { return m_displayName; }
    QString email() const { return m_email; }
    qulonglong quotaUsed() const { return m_quotaUsed; }
    qulonglong quotaTotal() const { return m_quotaTotal; }
    QString signInUrl() const { return m_signInUrl; }
    QString actionError() const { return m_actionError; }
    bool writeScopeEnabled() const { return m_writeScopeEnabled; }
    QStringList writePaths() const { return m_writePaths; }
    bool writeScopeChanging() const { return m_scopeCall.has_value(); }

    Q_INVOKABLE void retry();
    /// SetLabel; the daemon checks it (the rules AccountsModel::labelProblem states).
    Q_INVOKABLE void setLabel(const QString &label);
    Q_INVOKABLE void signIn();
    Q_INVOKABLE void cancelSignIn();
    Q_INVOKABLE void signOut();
    Q_INVOKABLE void refreshAccountInfo();
    Q_INVOKABLE void copySignInUrl();
    /// SetMode(mode, force). To read-write, the sign-in URL it answers is opened like
    /// signIn()'s, and the switch waits for it (modeSignInPending).
    Q_INVOKABLE void setMode(const QString &mode, bool force = false);
    /// Gives up a switch to read-write waiting for its sign-in (CancelSignIn).
    Q_INVOKABLE void cancelModeSwitch();
    /// `folder` (relative to the drive root) can be changed: SetWriteScope with it added to
    /// the trial's folders (writeScopeWith), or with it alone, which starts a trial. Nothing is
    /// asked when a folder listed covers it already.
    Q_INVOKABLE void allowFolder(const QString &folder);
    /// SetWriteScope without `folder`. The last folder stays: an empty list would refuse every
    /// change; clearWriteScope() ends the trial instead.
    Q_INVOKABLE void removeFolder(const QString &folder);
    /// ClearWriteScope: every folder can be changed, and the list is forgotten.
    Q_INVOKABLE void clearWriteScope();

    /// What the window says when SetMode(`mode`) was refused `errorName`, the daemon's
    /// `message` kept only where no plain words fit.
    static QString modeRefusalText(const QString &mode, const QString &errorName, const QString &message);

Q_SIGNALS:
    void serviceAvailableChanged();
    void accountChanged();
    void signInUrlChanged();
    void actionErrorChanged();
    void modeSwitchChanged();
    /// BeginSignIn returned the authorization URL; the UI opens it in the browser.
    void openUrlRequested(const QString &url);
    /// The user asked to sign out from this window (the Notifier stays quiet about it).
    void signOutRequested();
    void writeScopeChangingChanged();
    /// Account.WriteRefused: `count` changes outside the trial's folders were refused since the
    /// last report, one of them `path`, made in `folder` (empty: the top of the drive) by `app`.
    void writeRefused(const QString &path, const QString &folder, const QString &app, qulonglong count);

private Q_SLOTS:
    void onPropertiesChanged(const QString &interfaceName, const QVariantMap &changed, const QStringList &invalidated);
    void onWriteRefused(const QString &path, const QString &folder, const QString &app, qulonglong count);

private:
    /// GetAll; `then` runs once its answer is applied.
    void fetchAll(std::function<void()> then = {});
    void applyProperties(const QVariantMap &properties);
    void setServiceAvailable(bool available);
    void endModeSwitch();
    void setActionError(const QString &message);
    void call(const QDBusPendingCall &pending, std::function<void(const QDBusPendingCall &)> onSuccess = {});
    /// The trial as asked from this window: on with `paths`, or off.
    struct WriteScope {
        bool enabled = false;
        QStringList paths;
    };
    /// The trial as it will be once the call under way has answered, else as it is.
    WriteScope writeScope() const;
    /// SetWriteScope(`scope`'s paths), or ClearWriteScope for a scope that is off; a refusal
    /// lands in actionError.
    void changeWriteScope(const WriteScope &scope);

    QDBusConnection m_bus;
    QString m_path;
    QString m_id;
    OrgKonedriveAccountInterface *m_iface;
    QDBusServiceWatcher *m_watcher;
    bool m_serviceAvailable = false;
    QString m_label;
    QString m_mode = QStringLiteral("read-only");
    bool m_writeStillGranted = false;
    QString m_state = QStringLiteral("signed-out");
    QString m_lastError;
    bool m_switchingToReadOnly = false;
    qulonglong m_unsentCount = 0;
    QString m_displayName;
    QString m_email;
    qulonglong m_quotaUsed = 0;
    qulonglong m_quotaTotal = 0;
    QString m_signInUrl;
    QString m_actionError;
    QString m_switchingTo;
    /// Counts setMode() and cancelModeSwitch() calls: an answer to an older one is ignored.
    quint64 m_modeCall = 0;
    bool m_writeScopeEnabled = false;
    QStringList m_writePaths;
    /// What the scope call under way asks for: what the next change starts from, so that two
    /// quick changes do not undo each other.
    std::optional<WriteScope> m_scopeCall;
    /// Counts the scope calls: only the last one's answer ends writeScopeChanging.
    quint64 m_scopeCalls = 0;
};
