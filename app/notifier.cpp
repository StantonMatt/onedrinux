#include "notifier.h"

#include "accountcontroller.h"
#include "activitymodel.h"
#include "changeguardcontroller.h"
#include "downloadguardcontroller.h"
#include "heldchanges.h"
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

#include <algorithm>
#include <limits>
#include <memory>

namespace
{
QString fileName(const QString &path)
{
    const QString name = QFileInfo(path).fileName();
    return name.isEmpty() ? path : name;
}

/// Which of a kind's limits its held rows passed within one window.
enum class Trip { None, Count, Size };

/// Whether `kind`'s rows in `rows` came more than `countLimit` (or more than `byteLimit`
/// bytes) within `windowSeconds` of each other: Count before Size; None when neither, as when
/// the day's limit held them.
Trip heldTrip(KonedriveHeldChangeList rows, const QString &kind, qulonglong countLimit, qulonglong byteLimit, qint64 windowSeconds)
{
    rows.removeIf([&kind](const KonedriveHeldChange &row) {
        return row.kind != kind;
    });
    std::sort(rows.begin(), rows.end(), [](const KonedriveHeldChange &a, const KonedriveHeldChange &b) {
        return a.since < b.since;
    });
    Trip trip = Trip::None;
    qulonglong count = 0;
    qulonglong bytes = 0;
    qsizetype first = 0;
    for (const KonedriveHeldChange &row : std::as_const(rows)) {
        count += 1 + (kind == QLatin1String("removal") || kind == QLatin1String("upload") ? row.descendants : 0);
        bytes += row.bytes;
        while (row.since - rows.at(first).since > windowSeconds) {
            const KonedriveHeldChange &gone = rows.at(first++);
            count -= 1 + (kind == QLatin1String("removal") || kind == QLatin1String("upload") ? gone.descendants : 0);
            bytes -= gone.bytes;
        }
        if (count > countLimit) {
            return Trip::Count;
        }
        if (bytes > byteLimit) {
            trip = Trip::Size;
        }
    }
    // Moves and new files have no day's limit: what holds them is the count.
    return trip == Trip::None && kind != QLatin1String("removal") ? Trip::Count : trip;
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

/// The notice's buttons on `notification`: the one that opens the window first, then its own,
/// then Show in Folder.
static void addButtons(KNotification *notification, const Notice &notice, const KNotificationSink::OpenWindow &openWindow)
{
    if (!notice.openText.isEmpty() && openWindow) {
        const QString account = notice.account;
        const WindowSection section = notice.section;
        QObject::connect(notification->addAction(notice.openText), &KNotificationAction::activated, notification, [notification, openWindow, account, section] {
            // The click hands over the right to raise a window (xdg-activation on Wayland).
            if (const QString token = notification->xdgActivationToken(); !token.isEmpty()) {
                KWindowSystem::setCurrentXdgActivationToken(token);
            }
            openWindow(account, section);
        });
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
    addButtons(notification, notice, m_openWindow);
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

bool KNotificationSink::update(const Notice &notice)
{
    const QPointer<KNotification> shown = m_keyed.value(notice.key);
    if (!shown) {
        m_keyed.remove(notice.key);
        return false;
    }
    // KNotification sends what changed to the shown notification, which keeps its place.
    shown->setTitle(notice.title);
    shown->setText(notice.text);
    shown->clearActions();
    addButtons(shown, notice, m_openWindow);
    return true;
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
    connect(m_account, &AccountController::writeRefused, this, &Notifier::onWriteRefused);
}

void Notifier::onWriteRefused(const QString &, const QString &folder, const QString &, qulonglong count)
{
    // The folder is the drive's own path: untrusted text in markup.
    Notice notice{QStringLiteral("writeRefused"),
                  WindowSection::WriteScope,
                  i18nc("@title notification", "Change refused"),
                  folder.isEmpty() ? i18n("The top of your OneDrive can't be changed on this computer: only the folders you chose can.")
                                   : i18n("“%1” can't be changed on this computer: only the folders you chose can.", folder.toHtmlEscaped()),
                  {}};
    notice.count = int(qBound<qulonglong>(1, count, std::numeric_limits<int>::max()));
    // Not at the top, where it would allow every folder; nor once the trial has ended.
    if (!folder.isEmpty()) {
        const QPointer<AccountController> account = m_account;
        notice.actions = {{i18nc("@action:button", "Allow This Folder"), [account, folder] {
                               if (account && account->writeScopeEnabled()) {
                                   account->allowFolder(folder);
                               }
                           }}};
    }
    post(notice);
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
    // A report of several (refused changes) is shown as its first; the rest are counted.
    const int more = notice.count - 1;
    notice.count = 1;
    auto it = m_windows.find(notice.event);
    if (it == m_windows.end()) {
        m_sink->send(notice);
        m_windows.insert(notice.event, Window{now + windowMs(notice.event), more, notice});
        arm();
        return;
    }
    it->pending += more + 1;
    it->last = notice;
}

qint64 Notifier::windowMs(const QString &event)
{
    return event == QLatin1String("writeRefused") ? RefusedWindowMs : WindowMs;
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
    } else if (notice.event == QLatin1String("writeRefused")) {
        // Of several folders: none to allow.
        notice.text = i18np("1 more change was refused outside the folders that can be changed.",
                            "%1 more changes were refused outside the folders that can be changed.",
                            n);
        notice.actions.clear();
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
        it->endsAt = now + windowMs(it.key());
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
    };
    post(identity, notice);
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

ChangeNotifier::ChangeNotifier(AccountController *account, HeldChanges *held, NotificationSink *sink, QObject *parent)
    : QObject(parent)
    , m_account(account)
    , m_held(held)
    , m_sink(sink)
{
    connect(held, &HeldChanges::changed, this, &ChangeNotifier::onChanged);
    onChanged();
}

void ChangeNotifier::setAccountName(std::function<QString()> name)
{
    m_accountName = std::move(name);
}

void ChangeNotifier::setGuard(ChangeGuardController *guard)
{
    m_guard = guard;
}

QString ChangeNotifier::key() const
{
    return QStringLiteral("changeGuard:") + m_account->id();
}

void ChangeNotifier::onChanged()
{
    // Away: what is shown stays, until the daemon says again.
    if (!m_held || !m_held->serviceAvailable() || !m_held->listed()) {
        return;
    }
    QStringList kinds;
    for (const HeldChanges::Kind &kind : m_held->held()) {
        kinds << kind.kind;
    }
    if (!m_kinds) {
        // What was held before the app started is no news: Status and the tray say it.
        m_kinds = kinds;
        return;
    }
    const QStringList before = *m_kinds;
    m_kinds = kinds;
    const bool news = std::any_of(kinds.cbegin(), kinds.cend(), [&before](const QString &kind) {
        return !before.contains(kind);
    });
    if (news) {
        post(heldNotice(), false);
        m_shown = Shown::Held;
        return;
    }
    // What a button did stays until it is closed.
    if (m_shown != Shown::Held) {
        return;
    }
    if (kinds.isEmpty()) {
        m_sink->withdraw(key());
        m_shown = Shown::Nothing;
        return;
    }
    const Notice notice = heldNotice();
    if (notice.title == m_shownTitle && notice.text == m_shownText) {
        return;
    }
    // Closed meanwhile: more of the same kinds is no reason to show it again.
    if (!post(notice, true)) {
        m_shown = Shown::Nothing;
    }
}

Notice ChangeNotifier::heldNotice() const
{
    const QList<HeldChanges::Kind> kinds = m_held->held();
    Notice notice{QStringLiteral("changesHeld"), WindowSection::OnHold, m_held->summary(), QString(), {}};
    notice.openText = i18nc("@action:button", "Review…");
    const auto threshold = [this](const QString &key, qulonglong fallback) {
        return m_guard ? m_guard->threshold(key) : fallback;
    };
    if (kinds.size() == 1) {
        const HeldChanges::Kind &kind = kinds.constFirst();
        const qulonglong n = kind.count;
        const QString size = HeldChanges::sizeText(kind.bytes);
        QString window = m_guard ? m_guard->windowText() : i18ncp("@info a time window", "1 minute", "%1 minutes", 10);
        const qint64 windowSeconds = qint64(threshold(QStringLiteral("window-seconds"), 600));
        // Which limit tripped: more than the count within the window says it; else the size,
        // else (deletions) the day's.
        const auto passed = [&](const QString &countKey, qulonglong countFallback, const QString &bytesKey, qulonglong bytesFallback) {
            return heldTrip(m_held->heldRows(), kind.kind, threshold(countKey, countFallback), threshold(bytesKey, bytesFallback), windowSeconds);
        };
        bool bySize = false;
        if (kind.kind == QLatin1String("removal")) {
            const Trip trip = passed(QStringLiteral("removals"), 20, QStringLiteral("removal-bytes"), 1ULL << 30);
            bySize = trip == Trip::Size;
            if (trip == Trip::None) {
                window = m_guard ? m_guard->dailyWindowText() : i18nc("@info a time window: within…", "a day");
            }
            notice.text = bySize ? i18np("1 item, %3 in all, was deleted in the OneDrive folder within %2. It stays in OneDrive until you allow it.",
                                         "%1 items, %3 in all, were deleted in the OneDrive folder within %2. They stay in OneDrive until you allow it.",
                                         n,
                                         window,
                                         size)
                                 : i18np("1 item was deleted in the OneDrive folder within %2. It stays in OneDrive until you allow it.",
                                         "%1 items were deleted in the OneDrive folder within %2. They stay in OneDrive until you allow it.",
                                         n,
                                         window);
    } else if (kind.kind == QLatin1String("rewrite")) {
            bySize = passed(QStringLiteral("rewrites"), 50, QStringLiteral("rewrite-bytes"), 1ULL << 30) == Trip::Size;
            notice.text = bySize ? i18np("1 file, %3 in all, was changed in the OneDrive folder within %2. The change is not uploaded until you allow it.",
                                         "%1 files, %3 in all, were changed in the OneDrive folder within %2. The changes are not uploaded until you allow them.",
                                         n,
                                         window,
                                         size)
                                 : i18np("1 file was changed in the OneDrive folder within %2. The change is not uploaded until you allow it.",
                                         "%1 files were changed in the OneDrive folder within %2. The changes are not uploaded until you allow them.",
                                         n,
                                         window);
        } else if (kind.kind == QLatin1String("move")) {
            notice.text = i18np("1 item was moved or renamed in the OneDrive folder within %2. It is not moved in OneDrive until you allow it.",
                                "%1 items were moved or renamed in the OneDrive folder within %2. They are not moved in OneDrive until you allow it.",
                                n,
                                window);
        } else {
            bySize = passed(QStringLiteral("uploads"), 500, QStringLiteral("upload-bytes"), 5ULL << 30) == Trip::Size;
            notice.text = bySize ? i18np("1 file, %3 in all, was added to the OneDrive folder within %2. It is not uploaded until you allow it.",
                                         "%1 files, %3 in all, were added to the OneDrive folder within %2. They are not uploaded until you allow it.",
                                         n,
                                         window,
                                         size)
                                 : i18np("1 file was added to the OneDrive folder within %2. It is not uploaded until you allow it.",
                                         "%1 files were added to the OneDrive folder within %2. They are not uploaded until you allow it.",
                                         n,
                                         window);
        }
    } else {
        notice.text = i18nc("@info notification: the held kinds, joined", "%1 in the OneDrive folder are not sent to OneDrive until you allow them.", HeldChanges::kindsText(kinds));
    }
    QString restoreText;
    if (kinds.size() > 1) {
        restoreText = i18nc("@action:button", "Restore All");
    } else if (kinds.constFirst().kind == QLatin1String("move")) {
        restoreText = i18nc("@action:button", "Move Back");
    } else if (kinds.constFirst().kind == QLatin1String("upload")) {
        restoreText = i18nc("@action:button", "Don't Upload");
    } else {
        restoreText = i18nc("@action:button", "Restore");
    }
    notice.actions = {{restoreText, {}}};
    return notice;
}

void ChangeNotifier::restore(quint64 generation, const QString &token, const QList<HeldChanges::Kind> &counts)
{
    if (!m_held || m_shown != Shown::Held || generation != m_generation) {
        return;
    }
    // What the notification says, by its token: anything held after it stays on hold.
    QStringList kinds;
    for (const HeldChanges::Kind &kind : counts) {
        kinds << kind.kind;
    }
    m_shown = Shown::Result;
    const QPointer<ChangeNotifier> self(this);
    m_held->decide(
        true,
        kinds,
        token,
        [self, counts, generation](const KonedriveGuardResult &result) {
            if (self && self->m_generation == generation) {
                self->result(counts, result);
            }
        },
        [self, generation](const QString &error) {
            if (self && self->m_generation == generation) {
                self->refused(error);
            }
        });
}

void ChangeNotifier::result(const QList<HeldChanges::Kind> &kinds, const KonedriveGuardResult &result)
{
    Notice notice{QStringLiteral("changesHeld"), WindowSection::OnHold, QString(), QString(), {}};
    const qulonglong n = result.done;
    if (result.failed > 0) {
        notice.title = i18nc("@title notification", "Restored %1 of %2 items", n, n + result.failed);
        const QString why = HeldChanges::failureText(result);
        const QString stays = i18ncp("@info changes that stay on hold", "1 is still on hold.", "%1 are still on hold.", result.failed);
        notice.text = (why.isEmpty() ? stays : i18nc("@info what stays, why", "%1 %2", stays, why)).toHtmlEscaped();
        notice.openText = i18nc("@action:button", "Review…");
    } else if (kinds.size() == 1) {
        const QString kind = kinds.constFirst().kind;
        if (kind == QLatin1String("removal")) {
            notice.title = i18ncp("@title notification", "Restored 1 item", "Restored %1 items", n);
            notice.text = i18np("It is back in the OneDrive folder.", "They are back in the OneDrive folder.", n);
        } else if (kind == QLatin1String("rewrite")) {
            notice.title = i18ncp("@title notification", "Restored 1 file to OneDrive's version", "Restored %1 files to OneDrive's version", n);
            notice.text = i18np("Yours is kept beside it, with this computer's name added.", "Yours are kept beside them, with this computer's name added.", n);
        } else if (kind == QLatin1String("move")) {
            notice.title = i18ncp("@title notification", "Moved back 1 item", "Moved back %1 items", n);
            notice.text = i18np("It is back where it was.", "They are back where they were.", n);
        } else {
            notice.title = i18ncp("@title notification", "Kept 1 new file on this computer", "Kept %1 new files on this computer", n);
            notice.text = i18np("It is not uploaded. Issues lists it.", "They are not uploaded. Issues lists them.", n);
        }
    } else {
        // Each kind back as it was; what is kept says where.
        notice.title = i18ncp("@title notification", "Restored 1 change", "Restored %1 changes", n);
        QList<HeldChanges::Kind> undone;
        bool rewrites = false;
        bool uploads = false;
        for (const HeldChanges::Kind &kind : kinds) {
            if (kind.kind == QLatin1String("upload")) {
                uploads = true;
                continue;
            }
            rewrites = rewrites || kind.kind == QLatin1String("rewrite");
            undone << kind;
        }
        QStringList parts;
        if (!undone.isEmpty()) {
            parts << i18nc("@info notification: the held kinds, joined", "%1 were undone.", HeldChanges::kindsText(undone));
        }
        if (rewrites) {
            parts << i18n("Your versions of the changed files are kept beside them, with this computer's name added.");
        }
        if (uploads) {
            parts << i18n("The new files stay on this computer only; Issues lists them.");
        }
        notice.text = parts.join(QLatin1Char(' '));
    }
    post(notice, false);
    m_shown = Shown::Result;
}

void ChangeNotifier::refused(const QString &error)
{
    Notice notice{QStringLiteral("changesHeld"), WindowSection::OnHold, i18nc("@title notification", "Could not restore the changes on hold"), error.toHtmlEscaped(), {}};
    notice.openText = i18nc("@action:button", "Review…");
    post(notice, false);
    m_shown = Shown::Result;
}

bool ChangeNotifier::post(Notice notice, bool update)
{
    notice.key = key();
    notice.account = m_account->path();
    const QString title = notice.title;
    if (const QString name = m_accountName ? m_accountName() : QString(); !name.isEmpty()) {
        notice.title = i18nc("@title notification: what happened — the account's name", "%1 — %2", notice.title, name);
    }
    const quint64 generation = ++m_generation;
    if (!notice.actions.isEmpty() && m_held) {
        const QString token = m_held->token();
        const QList<HeldChanges::Kind> counts = m_held->held();
        const QPointer<ChangeNotifier> self(this);
        notice.actions[0].run = [self, generation, token, counts] {
            if (self) {
                self->restore(generation, token, counts);
            }
        };
    }
    if (update) {
        if (!m_sink->update(notice)) {
            return false;
        }
    } else {
        m_sink->send(notice);
    }
    m_shownTitle = title;
    m_shownText = notice.text;
    return true;
}
