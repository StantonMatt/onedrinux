#include "notifier.h"

#include "accountcontroller.h"
#include "activitymodel.h"
#include "downloadguardcontroller.h"
#include "skipreasons.h"
#include "uploadreasons.h"
#include "synccontroller.h"

#include <KIO/OpenFileManagerWindowJob>
#include <KIO/OpenUrlJob>
#include <KLocalizedString>
#include <KNotification>
#include <KWindowSystem>

#include <QElapsedTimer>
#include <QFileInfo>
#include <QLocale>
#include <QTimer>
#include <QUrl>

#include <limits>
#include <memory>

namespace
{
QString fileName(const QString &path)
{
    const QString name = QFileInfo(path).fileName();
    return name.isEmpty() ? path : name;
}

/// The title of a conflict that kept a copy beside the file.
QString copyTitle()
{
    return i18nc("@title notification", "Changed on both sides");
}
}

KNotificationSink::KNotificationSink(OpenWindow openWindow)
    : m_openWindow(std::move(openWindow))
{
}

void KNotificationSink::send(const Notice &notice)
{
    if (!notice.key.isEmpty()) {
        withdraw(notice.key);
    }
    auto *notification = new KNotification(notice.event);
    notification->setComponentName(QStringLiteral("konedrive"));
    notification->setTitle(notice.title);
    notification->setText(notice.text);
    if (!notice.iconName.isEmpty()) {
        notification->setIconName(notice.iconName);
    }
    for (const NoticeAction &action : notice.actions) {
        const std::function<void()> run = action.run;
        QObject::connect(notification->addAction(action.text), &KNotificationAction::activated, notification, [run] {
            if (run) {
                run();
            }
        });
    }
    if (!notice.showPath.isEmpty()) {
        const QString path = notice.showPath;
        KNotificationAction *show = notification->addAction(i18nc("@action:button", "Show in Folder"));
        QObject::connect(show, &KNotificationAction::activated, notification, [notification, path] {
            // The file manager's window gets the click's activation token.
            KIO::highlightInFileManager({QUrl::fromLocalFile(path)}, notification->xdgActivationToken().toUtf8());
        });
    }
    if (notice.link.isValid()) {
        const QUrl link = notice.link;
        const auto openLink = [notification, link] {
            // The browser's window gets the click's activation token.
            auto *job = new KIO::OpenUrlJob(link);
            job->setStartupId(notification->xdgActivationToken().toUtf8());
            job->start();
        };
        QObject::connect(notification->addDefaultAction(i18nc("@action", "Open Release Page")), &KNotificationAction::activated, notification, openLink);
        QObject::connect(notification->addAction(i18nc("@action:button", "Open Release Page")), &KNotificationAction::activated, notification, openLink);
    } else if (m_openWindow) {
        KNotificationAction *open = notification->addDefaultAction(i18nc("@action", "Open KOneDrive"));
        const OpenWindow openWindow = m_openWindow;
        const QString account = notice.account;
        const WindowSection section = notice.section;
        QObject::connect(open, &KNotificationAction::activated, notification, [notification, openWindow, account, section] {
            // The click hands over the right to raise a window (xdg-activation on Wayland).
            if (const QString token = notification->xdgActivationToken(); !token.isEmpty()) {
                KWindowSystem::setCurrentXdgActivationToken(token);
            }
            openWindow(account, section);
        });
    }
    if (!notice.key.isEmpty()) {
        m_keyed.insert(notice.key, notification);
    }
    notification->sendEvent();
}

void KNotificationSink::withdraw(const QString &key)
{
    const QPointer<KNotification> shown = m_keyed.take(key);
    if (shown) {
        shown->close();
    }
}

Notifier::Notifier(AccountController *account, SyncController *sync, NotificationSink *sink, Clock clock, QObject *parent)
    : QObject(parent)
    , m_account(account)
    , m_sync(sync)
    , m_sink(sink)
    , m_clock(std::move(clock))
    , m_timer(new QTimer(this))
    , m_signOutTimer(new QTimer(this))
    , m_accountState(account->state())
{
    m_signOutTimer->setSingleShot(true);
    connect(m_signOutTimer, &QTimer::timeout, this, &Notifier::announceSignOut);
    if (!m_clock) {
        auto elapsed = std::make_shared<QElapsedTimer>();
        elapsed->start();
        m_clock = [elapsed] {
            return elapsed->elapsed();
        };
    }
    m_timer->setSingleShot(true);
    connect(m_timer, &QTimer::timeout, this, &Notifier::flushDue);
    connect(m_sync, &SyncController::activityAdded, this, &Notifier::onActivity);
    connect(m_sync, &SyncController::syncChanged, this, &Notifier::onSyncChanged);
    connect(m_sync, &SyncController::serviceAvailableChanged, this, [this] {
        m_full = m_sync->serviceAvailable() ? int(m_sync->quotaFull()) : -1;
        m_waitRefusal = m_sync->serviceAvailable() ? m_sync->retryAfterRefusal() : QString();
    });
    if (m_sync->serviceAvailable()) {
        m_full = int(m_sync->quotaFull());
        m_waitRefusal = m_sync->retryAfterRefusal();
    }
    connect(m_account, &AccountController::accountChanged, this, &Notifier::onAccountChanged);
    connect(m_account, &AccountController::signOutRequested, this, [this] {
        m_signOutAsked = true;
    });
}

void Notifier::onActivity(qint64, const QString &kind, const QString &path, const QString &detail)
{
    // Plasma renders notification bodies as markup; filenames are untrusted text.
    const QString name = fileName(path).toHtmlEscaped();
    if (kind == QLatin1String("not-removed-here")) {
        // The path is the directory; rmdir itself could only say "not empty". Nothing to press.
        post({QStringLiteral("notRemovedHere"),
              WindowSection::NotInFolder,
              i18nc("@title notification", "Folder not removed"),
              i18n("%1 cannot be removed here: it holds items that can't be shown in the folder. Remove it in OneDrive.", name),
              {}});
        return;
    }
    if (kind == QLatin1String("name-taken-in-onedrive")) {
        // The path is the name asked for, the detail the reason of what holds it: mkdir or rename could only say "exists", and the folder shows nothing there.
        post({QStringLiteral("nameTaken"),
              WindowSection::NotInFolder,
              i18nc("@title notification", "Name taken in OneDrive"),
              i18n("%1 can't be used here: OneDrive has an item of this name that can't be shown in the folder. %2 Rename it in OneDrive to use the name.", name, skipReasonText(detail)),
              {}});
        return;
    }
    if (kind == QLatin1String("conflict")) {
        // A file changed on both sides: the daemon's detail is the copy kept beside it.
        post({QStringLiteral("conflict"), WindowSection::KeptBothVersions, copyTitle(), i18n("%1 changed here and in OneDrive. Kept both versions: yours is %2.", name, fileName(detail).toHtmlEscaped()), detail});
        return;
    }
    switch (classifyFailure(kind, detail)) {
    case ActivityFailure::None:
        return;
    case ActivityFailure::DiskFull:
        post({QStringLiteral("diskFull"), WindowSection::Status, i18nc("@title notification", "Not enough disk space"), i18n("%1 could not be downloaded: the disk is full.", name), {}});
        return;
    case ActivityFailure::Update:
        post({QStringLiteral("updateFailed"),
              WindowSection::History,
              i18nc("@title notification", "A file could not be updated"),
              i18n("%1 changed in OneDrive but could not be updated here: %2", name, detail.toHtmlEscaped()),
              {}});
        return;
    case ActivityFailure::Upload:
        // Blocked names and a full OneDrive are the usual ones; the detail is a reason code.
        if (detail == QLatin1String("quota-exceeded")) {
            post({QStringLiteral("uploadFailed"),
                  WindowSection::CantUpload,
                  i18nc("@title notification", "OneDrive is full"),
                  i18n("%1 can't upload until there is space in OneDrive.", name),
                  path});
            return;
        }
        post({QStringLiteral("uploadFailed"), WindowSection::CantUpload, i18nc("@title notification", "Can't upload"), i18n("%1 can't upload. %2", name, uploadReasonText(detail)), path});
        return;
    case ActivityFailure::Download:
        post({QStringLiteral("downloadFailed"),
              WindowSection::History,
              i18nc("@title notification", "Download failed"),
              detail.isEmpty() ? i18n("%1 could not be downloaded.", name) : i18n("%1 could not be downloaded: %2", name, detail.toHtmlEscaped()),
              {}});
        return;
    }
}

void Notifier::onSyncChanged()
{
    if (m_full < 0) {
        // The daemon's first answer sets the baseline (serviceAvailableChanged).
        return;
    }
    // OneDrive turned full: said once for the account, never per file (issue #2).
    const int full = int(m_sync->quotaFull());
    if (m_full == 0 && full == 1) {
        post({QStringLiteral("uploadFailed"),
              WindowSection::CantUpload,
              i18nc("@title notification", "OneDrive is full"),
              i18n("Nothing more is uploaded until there is space in OneDrive. Free up space there, then choose Sync Now in KOneDrive."),
              {}});
    }
    m_full = full;

    // OneDrive asked the account to wait, and a program was refused a file meanwhile: said
    // once for the wait, with the first file refused, never per read.
    const QString refused = m_sync->retryAfterRefusal();
    if (m_waitRefusal.isEmpty() && !refused.isEmpty()) {
        const QString name = fileName(refused).toHtmlEscaped();
        const int minutes = int((m_sync->retryAfter() + 59) / 60);
        post({QStringLiteral("throttled"),
              WindowSection::Status,
              i18nc("@title notification", "OneDrive asked to wait"),
              minutes > 0 ? i18np("OneDrive asked this device to wait about 1 minute. %2 opens after that.",
                                  "OneDrive asked this device to wait about %1 minutes. %2 opens after that.",
                                  minutes,
                                  name)
                          : i18n("OneDrive asked this device to wait. %1 opens after that.", name),
              {}});
    }
    m_waitRefusal = refused;
}

void Notifier::onAccountChanged()
{
    const QString previous = m_accountState;
    m_accountState = m_account->state();
    if (m_accountState != QLatin1String("signed-out") || previous != QLatin1String("signed-in")) {
        return;
    }
    if (m_signOutAsked) {
        m_signOutAsked = false;
        return;
    }
    // Accounts.Remove signs the account out, then unexports it and drops it
    // from Accounts: the row goes, and this Notifier with it, before the
    // notice is due. A sign-out that is news stays signed out until then.
    m_signOutTimer->start(m_signOutDelayMs);
}

void Notifier::setSignOutDelay(int ms)
{
    m_signOutDelayMs = ms;
}

void Notifier::announceSignOut()
{
    if (m_account->state() != QLatin1String("signed-out")) {
        return;
    }
    post({QStringLiteral("signedOut"), WindowSection::Status, i18nc("@title notification", "Signed out of OneDrive"), i18n("Sign in again to keep your OneDrive folder up to date."), {}});
}

void Notifier::setAccountName(std::function<QString()> name)
{
    m_accountName = std::move(name);
}

void Notifier::post(const Notice &event)
{
    Notice notice = event;
    notice.account = m_account->path();
    if (const QString name = m_accountName ? m_accountName() : QString(); !name.isEmpty()) {
        notice.title = i18nc("@title notification: what happened — the account's name", "%1 — %2", event.title, name);
    }
    flushDue();
    const qint64 now = m_clock();
    auto it = m_windows.find(notice.event);
    if (it == m_windows.end()) {
        m_sink->send(notice);
        m_windows.insert(notice.event, Window{now + WindowMs, 0, notice});
        arm();
        return;
    }
    ++it->pending;
    it->last = notice;
}

Notice Notifier::summary(const Window &window) const
{
    const int n = window.pending;
    Notice notice = window.last;
    notice.count = n;
    if (notice.event == QLatin1String("diskFull")) {
        notice.text = i18np("1 more file could not be downloaded: the disk is full.", "%1 more files could not be downloaded: the disk is full.", n);
    } else if (notice.event == QLatin1String("downloadFailed")) {
        notice.text = i18np("1 more file could not be downloaded.", "%1 more files could not be downloaded.", n);
    } else if (notice.event == QLatin1String("updateFailed")) {
        notice.text = i18np("1 more file changed in OneDrive could not be updated here.", "%1 more files changed in OneDrive could not be updated here.", n);
    } else if (notice.event == QLatin1String("conflict")) {
        // "Show in Folder" stays on the last copy kept.
        notice.text = i18np("1 more file changed here and in OneDrive; kept both versions.", "%1 more files changed here and in OneDrive; kept both versions.", n);
    } else if (notice.event == QLatin1String("notRemovedHere")) {
        notice.text = i18np("1 more folder cannot be removed here.", "%1 more folders cannot be removed here.", n);
    } else if (notice.event == QLatin1String("nameTaken")) {
        notice.text = i18np("1 more name is taken in OneDrive by an item that can't be shown here.", "%1 more names are taken in OneDrive by items that can't be shown here.", n);
    } else if (notice.event == QLatin1String("throttled")) {
        notice.text = i18np("1 more file could not be opened: OneDrive asked to wait.", "%1 more files could not be opened: OneDrive asked to wait.", n);
    } else if (notice.event == QLatin1String("uploadFailed")) {
        notice.text = i18np("1 more change can't upload.", "%1 more changes can't upload.", n);
    }
    return notice;
}

void Notifier::flushDue()
{
    const qint64 now = m_clock();
    for (auto it = m_windows.begin(); it != m_windows.end();) {
        if (it->endsAt > now) {
            ++it;
            continue;
        }
        if (it->pending == 0) {
            it = m_windows.erase(it);
            continue;
        }
        m_sink->send(summary(*it));
        it->pending = 0;
        it->endsAt = now + WindowMs;
        ++it;
    }
    arm();
}

void Notifier::arm()
{
    if (m_windows.isEmpty()) {
        m_timer->stop();
        return;
    }
    qint64 next = std::numeric_limits<qint64>::max();
    for (const Window &window : std::as_const(m_windows)) {
        next = qMin(next, window.endsAt);
    }
    m_timer->start(int(qBound<qint64>(0, next - m_clock(), WindowMs)));
}

GuardNotifier::GuardNotifier(DownloadGuardController *guard, NotificationSink *sink, QObject *parent)
    : QObject(parent)
    , m_guard(guard)
    , m_sink(sink)
{
    connect(guard, &DownloadGuardController::downloadsBlocked, this, &GuardNotifier::onBlocked);
    connect(guard, &DownloadGuardController::appsChanged, this, &GuardNotifier::reconcile);
    connect(guard, &DownloadGuardController::changed, this, &GuardNotifier::reconcile);
}

void GuardNotifier::onBlocked(const GuardApp &app)
{
    if (app.reason == QLatin1String("unidentified")) {
        post(app.identity,
             {QStringLiteral("downloadsBlocked"),
              WindowSection::GuardApps,
              i18nc("@title notification", "Download blocked"),
              i18n("A program was blocked from downloading from OneDrive because KOneDrive couldn't tell which program it was."),
              {}});
        return;
    }
    // A background reader is refused whatever it reads; an app always blocked was blocked by
    // the user. Neither is news.
    if (app.reason != QLatin1String("bulk") || !m_guard) {
        return;
    }
    // Plasma reads a notification's text as markup: the name is the program's own say.
    const QString name = app.name.toHtmlEscaped();
    const QString window = m_guard->windowText();
    QString text;
    if (app.files >= m_guard->maxFiles()) {
        text = i18nc("@info notification: the app, how many files, how long",
                     "%1 downloaded %2 from OneDrive in %3. To save space, its downloads are blocked until you allow them.",
                     name,
                     m_guard->filesText(app.files),
                     window);
    } else if (app.bytes > 0) {
        text = i18nc("@info notification: the app, how much, how long",
                     "%1 downloaded %2 from OneDrive in %3. To save space, its downloads are blocked until you allow them.",
                     name,
                     QLocale().formattedDataSize(qint64(app.bytes)),
                     window);
    } else {
        // One file over the byte limit on its own.
        text = i18nc("@info notification: the app, the byte limit",
                     "%1 tried to download a file larger than %2 from OneDrive. To save space, its downloads are blocked until you allow them.",
                     name,
                     DownloadGuardController::bytesText(m_guard->maxBytes()));
    }
    const QPointer<GuardNotifier> self(this);
    const QString identity = app.identity;
    const QString shownName = app.name;
    const QString iconName = app.iconName;
    const auto refusedThen = [self, identity, shownName](const QString &error) {
        if (self) {
            self->refused(identity, shownName, error);
        }
    };
    Notice notice{QStringLiteral("downloadsBlocked"), WindowSection::GuardApps, i18nc("@title notification", "Too many downloads by %1", app.name), text, {}};
    notice.iconName = iconName;
    notice.actions = {
        {i18nc("@action:button", "Allow for This Session"),
         [self, identity, refusedThen] {
             if (self && self->m_guard) {
                 self->m_guard->allowApp(identity, false, refusedThen);
             }
         }},
        {i18nc("@action:button", "Always Allow"),
         [self, identity, refusedThen] {
             if (self && self->m_guard) {
                 self->m_guard->allowApp(identity, true, refusedThen);
             }
         }},
        {i18nc("@action:button", "Always Block"),
         [self, identity, refusedThen] {
             if (self && self->m_guard) {
                 self->m_guard->blockApp(identity, refusedThen);
             }
         }},
        {i18nc("@action:button", "Free Up What It Downloaded"),
         [self, identity, shownName, iconName] {
             if (self) {
                 self->freeUp(identity, shownName, iconName);
             }
         }},
    };
    post(identity, notice);
}

void GuardNotifier::freeUp(const QString &identity, const QString &name, const QString &iconName)
{
    if (!m_guard) {
        return;
    }
    const QPointer<GuardNotifier> self(this);
    m_guard->freeUpAppDownloads(
        identity,
        [self, identity, name, iconName](qulonglong files, qulonglong bytes) {
            if (!self) {
                return;
            }
            Notice notice{QStringLiteral("downloadsBlocked"),
                          WindowSection::GuardApps,
                          i18nc("@title notification", "Freed up what %1 downloaded", name),
                          DownloadGuardController::freedText(files, bytes),
                          {}};
            notice.iconName = iconName;
            self->post(identity, notice);
        },
        [self, identity, name, iconName](const QString &error) {
            if (!self) {
                return;
            }
            Notice notice{QStringLiteral("downloadsBlocked"),
                          WindowSection::GuardApps,
                          i18nc("@title notification", "Could not free up what %1 downloaded", name),
                          error.toHtmlEscaped(),
                          {}};
            notice.iconName = iconName;
            self->post(identity, notice);
        });
}

void GuardNotifier::refused(const QString &identity, const QString &name, const QString &error)
{
    post(identity,
         {QStringLiteral("downloadsBlocked"),
          WindowSection::GuardApps,
          i18nc("@title notification", "Could not change what %1 may download", name),
          error.toHtmlEscaped(),
          {}});
}

void GuardNotifier::post(const QString &identity, Notice notice)
{
    notice.key = QStringLiteral("downloadGuard:") + identity;
    m_shown.insert(identity);
    m_sink->send(notice);
}

void GuardNotifier::reconcile()
{
    if (!m_guard) {
        return;
    }
    for (auto it = m_shown.begin(); it != m_shown.end();) {
        const GuardApp *app = m_guard->app(*it);
        const QString state = app ? m_guard->stateOf(*app) : QString();
        // Still blocked and waiting for the user, as the list says: what is shown stays.
        if (m_guard->enabled() && (state == QLatin1String("blocked") || state == QLatin1String("unidentified"))) {
            ++it;
            continue;
        }
        m_sink->withdraw(QStringLiteral("downloadGuard:") + *it);
        it = m_shown.erase(it);
    }
}
