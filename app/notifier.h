#pragma once

#include "heldchanges.h"
#include "windowsection.h"

#include <QHash>
#include <QList>
#include <QObject>
#include <QPointer>
#include <QSet>
#include <QString>
#include <QUrl>

#include <functional>
#include <optional>

class AccountController;
class ChangeGuardController;
class DownloadGuardController;
class KNotification;
class SyncController;
class QTimer;
struct GuardApp;

/// A button of a notification, and what it does.
struct NoticeAction {
    QString text;
    std::function<void()> run;
};

/// One desktop notification, as the Notifier decides it.
struct Notice {
    /// The event in konedrive.notifyrc: signedOut, diskFull, downloadFailed,
    /// updateFailed, uploadFailed, conflict, notRemovedHere, nameTaken, throttled,
    /// newerRelease, downloadsBlocked, changesHeld, writeRefused.
    QString event;
    /// Where a click on the notification opens the window.
    WindowSection section = WindowSection::Status;
    QString title;
    QString text;
    /// When set, the notification offers "Show in Folder" for this path.
    QString showPath;
    /// 1 for a single event; for a summary, how many events it stands for. Posted with more
    /// than 1 (a report of several refused changes), it is shown as one, the rest counted.
    int count = 1;
    /// The account's object path: a click on the notification opens the window on it, at
    /// `section`.
    QString account = QString();
    /// When set, a click on the notification opens this page in the browser instead of the
    /// window, and the notification offers "Open Release Page" for it.
    QUrl link = QUrl();
    /// When set, the notice takes the place of the one sent with the same key, if that is
    /// still shown, and withdraw() takes it back.
    QString key = QString();
    /// The notification's buttons, in order.
    QList<NoticeAction> actions = {};
    /// The notification's icon, when it is not KOneDrive's.
    QString iconName = QString();
    /// When set, the first button, with these words, opens the window as a click on the
    /// notification does ("Review…").
    QString openText = QString();
};

/// Where notices go. The app sends them through KNotification
/// (KNotificationSink); tests record them.
class NotificationSink
{
public:
    virtual ~NotificationSink() = default;
    virtual void send(const Notice &notice) = 0;
    /// Takes back the notification sent with `key`, if it is still shown.
    virtual void withdraw(const QString &key) { Q_UNUSED(key) }
    /// Changes the words of the notification sent with the notice's key in place, without
    /// showing it again, if it is still shown; false when it is not.
    virtual bool update(const Notice &notice)
    {
        Q_UNUSED(notice)
        return false;
    }
};

/// Sends each notice as a KNotification of component "konedrive", so each
/// event can be configured in System Settings.
class KNotificationSink : public NotificationSink
{
public:
    using OpenWindow = std::function<void(const QString &account, WindowSection section)>;
    /// `openWindow` runs when the user clicks the notification itself, with
    /// the notice's account and section.
    explicit KNotificationSink(OpenWindow openWindow = {});
    /// The same, given once the window exists (the sink is made before the
    /// accounts, whose Notifiers point at it).
    void setOpenWindow(OpenWindow openWindow) { m_openWindow = std::move(openWindow); }
    void send(const Notice &notice) override;
    void withdraw(const QString &key) override;
    bool update(const Notice &notice) override;

private:
    OpenWindow m_openWindow;
    /// The notifications sent with a key; each goes once it is closed.
    QHash<QString, QPointer<KNotification>> m_keyed;
};

/// Turns what one account's daemon objects report into notifications:
/// signing out after being signed in, a full disk, a failed download, a
/// failed update, a change that cannot be uploaded, a conflict (a copy kept
/// beside the file), a directory that cannot be removed here, a name OneDrive holds with an
/// item that cannot be shown here, a file that could not be
/// opened because OneDrive asked the account to wait — once for each wait —, and a change
/// refused outside a trial's folders, with Allow This Folder. Nothing else
/// an account reports notifies (a newer release is ReleaseCheck's). Each event kind sends at
/// most one notification per 10 s window (10 minutes for refused changes); what arrives inside
/// a window is counted and sent as one summary when the window ends. Each account has its
/// own Notifier, so windows and summaries are per account and kind.
class Notifier : public QObject
{
    Q_OBJECT

public:
    /// Milliseconds, monotonic.
    using Clock = std::function<qint64()>;
    static constexpr qint64 WindowMs = 10000;
    /// Refused changes: the first says where, the rest of the next 10 minutes are counted.
    static constexpr qint64 RefusedWindowMs = 10 * 60 * 1000;
    /// How long a sign-out waits before it is announced: an account being
    /// removed is signed out first and gone within it, taking this Notifier
    /// (a child of its AccountItem) and the notice with it.
    static constexpr int SignOutDelayMs = 2000;

    Notifier(AccountController *account, SyncController *sync, NotificationSink *sink, Clock clock = {}, QObject *parent = nullptr);

    /// The timer that ends the windows (tests check that it is armed).
    QTimer *windowTimer() const { return m_timer; }

    /// The account's name for the titles ("Download failed — Family"),
    /// asked each time; empty (the default) leaves titles as they are —
    /// with one account there is nothing to tell apart.
    void setAccountName(std::function<QString()> name);

    /// Tests shorten SignOutDelayMs.
    void setSignOutDelay(int ms);

public Q_SLOTS:
    /// Ends every window whose time is up: sends its summary, if it counted
    /// anything, and opens a new window for it. The timer calls this.
    void flushDue();

private:
    struct Window {
        qint64 endsAt = 0;
        int pending = 0;
        Notice last;
    };

    void onActivity(qint64 time, const QString &kind, const QString &path, const QString &detail);
    void onAccountChanged();
    /// Account.WriteRefused: writeRefused, with Allow This Folder unless at the top.
    void onWriteRefused(const QString &path, const QString &folder, const QString &app, qulonglong count);
    static qint64 windowMs(const QString &event);
    /// OneDrive turning full (QuotaFull from false): uploadFailed, once each time. A file
    /// refused during the wait OneDrive asked for (RetryAfterRefusal from empty):
    /// throttled, once for the wait.
    void onSyncChanged();
    /// The sign-out, once SignOutDelayMs has passed with the account still signed out.
    void announceSignOut();
    void post(const Notice &notice);
    Notice summary(const Window &window) const;
    void arm();

    AccountController *m_account;
    SyncController *m_sync;
    NotificationSink *m_sink;
    Clock m_clock;
    std::function<QString()> m_accountName;
    QTimer *m_timer;
    QTimer *m_signOutTimer;
    int m_signOutDelayMs = SignOutDelayMs;
    QHash<QString, Window> m_windows;
    QString m_accountState;
    /// The user signed out from this window: the sign-out that follows is theirs, not news.
    bool m_signOutAsked = false;
    /// QuotaFull as last seen; -1 while the daemon is away. The daemon's first
    /// answer only sets it (a restart replays nothing): one notice when OneDrive
    /// turns full.
    int m_full = -1;
    /// RetryAfterRefusal as last seen: the daemon names one file for a wait, and takes
    /// the name off when the wait is over. The daemon's first answer only sets it.
    QString m_waitRefusal;
};

/// The download guard's notifications, for the whole app: the guard counts each app across
/// every account, so they name none. An app blocked for downloading too much (DownloadsBlocked,
/// reason bulk) gets one, with Allow for This Session and Always Allow, the two buttons a
/// Plasma popup has room for (Always Block and Free Up What It Downloaded are in the window's
/// row of the app); a terminal or an IDE, which downloads for whatever runs in it, gets "A
/// program in Konsole downloaded …" and Allow for This Session only, Always Allow staying in
/// the window; a program that could not be identified (unidentified) gets one with
/// nothing to press, since it can't be allowed. A reader the guard always refuses (background)
/// and an app the user always blocks (user) notify nothing. One notification for each block,
/// never one per refused read, and never summed up with others: each has its own buttons. An
/// app has one at a time: a new one takes the place of the one shown, and one whose app is no
/// longer blocked (allowed in the window, removed, the daemon gone) is withdrawn. A click opens
/// the window on the apps it blocked (WindowSection::GuardApps).
class GuardNotifier : public QObject
{
    Q_OBJECT

public:
    GuardNotifier(DownloadGuardController *guard, NotificationSink *sink, QObject *parent = nullptr);

private:
    void onBlocked(const GuardApp &app);
    /// Withdraws what is shown for an app the list no longer has blocked.
    void reconcile();
    void post(const QString &identity, Notice notice);
    /// The daemon refused what a button asked.
    void refused(const QString &identity, const QString &name, const QString &error);

    QPointer<DownloadGuardController> m_guard;
    NotificationSink *m_sink;
    /// The apps that have a notification of theirs out.
    QSet<QString> m_shown;
};

/// The change guard's notification for one account (event changesHeld): one at a time, keyed
/// `changeGuard:<account id>`, never summed up with others, since it has buttons of its own.
/// It says what is held, kind by kind (for deletions, the folder deleted with what was in it,
/// or the folder they were in), and what of those kinds went out just before the hold, with
/// Review… (the window's On Hold) and Restore (Move Back for moves, Don't Upload for new files,
/// Restore All for several kinds): allowing is on the On Hold page only. A kind that comes on hold shows it (again, if it was closed); more
/// of a kind it shows, or fewer, change its words in place; nothing held any more takes it
/// back. Its buttons act on what it says (the token of that listing): what was held after it
/// stays on hold. What a button did takes its place, with Open Recycle Bin when deletions went
/// out before the hold; a decision asked in the window notifies
/// nothing. The holds the daemon has when the app starts are its baseline, not news.
class ChangeNotifier : public QObject
{
    Q_OBJECT

public:
    ChangeNotifier(AccountController *account, HeldChanges *held, NotificationSink *sink, QObject *parent = nullptr);

    /// The account's name for the titles, asked each time; empty leaves titles as they are.
    void setAccountName(std::function<QString()> name);
    /// The window's limits, for "within 10 minutes"; none: the words without them.
    void setGuard(ChangeGuardController *guard);
    /// Opens OneDrive's recycle bin in the browser: a restore's result offers it when deletions
    /// went out before the hold. None: no button.
    void setOpenRecycleBin(std::function<void()> open);

private:
    void onChanged();
    /// The notice for the kinds held now, with the token that lists them.
    Notice heldNotice() const;
    void restore(quint64 generation, const QString &token, const QList<HeldChanges::Kind> &counts, const QList<HeldChanges::Kind> &sent);
    /// What the Restore of `kinds` did, in place of the notification, and where what was `sent`
    /// of them before the hold is.
    void result(const QList<HeldChanges::Kind> &kinds, const QList<HeldChanges::Kind> &sent, const KonedriveGuardResult &result);
    void refused(const QString &error);
    /// Sends `notice` as this account's, or with `update` changes the shown one in place:
    /// false when it was closed.
    bool post(Notice notice, bool update);
    QString key() const;

    AccountController *m_account;
    QPointer<HeldChanges> m_held;
    NotificationSink *m_sink;
    QPointer<ChangeGuardController> m_guard;
    std::function<QString()> m_accountName;
    std::function<void()> m_openRecycleBin;
    /// The kinds held at the last listing; null until the daemon's first answer.
    std::optional<QStringList> m_kinds;
    /// What is shown: nothing, the held changes, or what a button did.
    enum class Shown { Nothing, Held, Result } m_shown = Shown::Nothing;
    /// What the notification says (the title without the account's name). Its button
    /// captures its own token and counts.
    QString m_shownTitle;
    QString m_shownText;
    /// Actions and completions belong only to this version of the notice.
    quint64 m_generation = 0;
};
