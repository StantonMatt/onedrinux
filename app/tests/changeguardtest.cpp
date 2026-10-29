#include "accountsmodel.h"
#include "appstatus.h"
#include "changeguardcontroller.h"
#include "daemoncontroller.h"
#include "fakedaemon.h"
#include "heldchanges.h"
#include "notifier.h"

#include <KLocalizedString>

#include <QDateTime>
#include <QElapsedTimer>
#include <QLocale>
#include <QTest>

#include <memory>

namespace
{
constexpr qulonglong MiB = 1ULL << 20;
constexpr qulonglong GiB = 1ULL << 30;

/// Today at hh:mm, in Unix seconds.
qint64 today(int hours, int minutes)
{
    return QDateTime(QDate::currentDate(), QTime(hours, minutes)).toSecsSinceEpoch();
}

QString timeOfDay(int hours, int minutes)
{
    return QLocale().toString(QTime(hours, minutes), QLocale::ShortFormat);
}

KonedriveHeldChange folder(const QString &kind, const QString &path, qulonglong bytes, qulonglong descendants, qint64 since)
{
    KonedriveHeldChange row = heldRow(kind, path, bytes, since);
    row.directory = true;
    row.descendants = descendants;
    return row;
}

KonedriveHeldChange sent(const QString &kind, const QString &path, qint64 since)
{
    KonedriveHeldChange row = heldRow(kind, path, 0, since);
    row.sent = true;
    row.state = QStringLiteral("sent");
    return row;
}

/// Three deletions (one a folder of one item), 1.5 MiB in all, since 14:02.
KonedriveHeldChangeList deletions()
{
    KonedriveHeldChange file = heldRow(QStringLiteral("removal"), QStringLiteral("Documents/a.txt"), MiB / 2, today(14, 2));
    file.item = 1;
    KonedriveHeldChange old = folder(QStringLiteral("removal"), QStringLiteral("Documents/Old"), MiB, 1, today(14, 3));
    old.item = 3;
    return {file, old};
}

KonedriveHeldChangeList rewrites()
{
    KonedriveHeldChange emptied = heldRow(QStringLiteral("rewrite"), QStringLiteral("Book/chapter-01.md"), 48 * 1024, today(14, 5));
    emptied.afterBytes = 0;
    return {emptied};
}

QString text(const QVariantList &rows, int index, const char *key)
{
    return rows.value(index).toMap().value(QLatin1String(key)).toString();
}

QStringList buttons(const Notice &notice)
{
    QStringList texts;
    if (!notice.openText.isEmpty()) {
        texts << notice.openText;
    }
    for (const NoticeAction &action : notice.actions) {
        texts << action.text;
    }
    return texts;
}
}

/// Stands in for KNotification: what was sent, changed in place, and taken back, and what
/// Plasma would show now, by key.
class RecordingSink : public NotificationSink
{
public:
    void send(const Notice &notice) override
    {
        sent << notice;
        shown.insert(notice.key, notice);
    }
    bool update(const Notice &notice) override
    {
        if (!shown.contains(notice.key)) {
            return false;
        }
        updated << notice;
        shown.insert(notice.key, notice);
        return true;
    }
    void withdraw(const QString &key) override
    {
        withdrawn << key;
        shown.remove(key);
    }
    QList<Notice> sent;
    QList<Notice> updated;
    QHash<QString, Notice> shown;
    QStringList withdrawn;
};

/// The change guard against the fake daemon's ChangeGuard on the private bus: the settings and
/// limits (ChangeGuardController), what an account holds and the page's words for it, Allow
/// and Restore on the listed token with what they did (HeldChanges), the notification, its
/// buttons and its results (ChangeNotifier), and Status and the tray while changes are held.
class ChangeGuardTest : public QObject
{
    Q_OBJECT

private:
    std::unique_ptr<FakeDaemon> m_daemon;
    QString m_id;

    void start()
    {
        m_daemon = std::make_unique<FakeDaemon>();
        QVERIFY(m_daemon->start());
        m_id = fake::idFor(1);
    }

    /// `held` lists what the fake holds now.
    void hold(HeldChanges *held, const KonedriveHeldChangeList &rows)
    {
        m_daemon->changeGuard->hold(m_id, rows);
        QTRY_COMPARE(held->token(), m_daemon->changeGuard->token());
    }

private Q_SLOTS:
    void initTestCase()
    {
        KLocalizedString::setApplicationDomain("konedrive");
        QLocale::setDefault(QLocale(QLocale::English, QLocale::UnitedStates));
    }

    void cleanup()
    {
        m_daemon.reset();
    }

    /// The switch and limits as the daemon has them, the switch's words, and what each
    /// control sends: a limit left as shown keeps the daemon's exact value.
    void theSettings()
    {
        start();
        m_daemon->changeGuard->thresholds.insert(QStringLiteral("removal-bytes"), GiB + 12345);
        ChangeGuardController guard;
        QTRY_VERIFY(guard.serviceAvailable());
        QVERIFY(guard.enabled());
        QCOMPARE(guard.description(),
                 QStringLiteral("More than 20 deletions, 50 changed files, 200 moves or 500 new files within 10 minutes wait for your review before they're sent to "
                                "OneDrive."));
        const QVariantMap limits = guard.limits();
        QCOMPARE(limits.value(QStringLiteral("removals")).toInt(), 20);
        QCOMPARE(limits.value(QStringLiteral("removalGiB")).toInt(), 1);
        QCOMPARE(limits.value(QStringLiteral("uploadGiB")).toInt(), 5);
        QCOMPARE(limits.value(QStringLiteral("minutes")).toInt(), 10);
        QCOMPARE(limits.value(QStringLiteral("dailyRemovals")).toInt(), 100);
        QCOMPARE(guard.windowText(), QStringLiteral("10 minutes"));
        QCOMPARE(guard.dailyWindowText(), QStringLiteral("a day"));

        auto &calls = m_daemon->changeGuard->calls;
        // Nothing differs: nothing is sent.
        guard.setLimits(limits);
        QVariantMap changed = limits;
        changed.insert(QStringLiteral("removals"), 30);
        changed.insert(QStringLiteral("minutes"), 15);
        guard.setLimits(changed);
        QTRY_VERIFY(calls.contains(QStringLiteral("SetThresholds:removals=30,window-seconds=900")));
        QCOMPARE(calls.filter(QStringLiteral("SetThresholds")).size(), 1);
        // The byte limit nobody changed is the daemon's to the byte.
        QCOMPARE(m_daemon->changeGuard->thresholds.value(QStringLiteral("removal-bytes")), GiB + 12345);
        QTRY_COMPARE(guard.limits().value(QStringLiteral("removals")).toInt(), 30);
        QTRY_VERIFY(guard.description().contains(QStringLiteral("More than 30 deletions")));

        guard.setEnabled(false);
        QTRY_VERIFY(!guard.enabled());
        QVERIFY(calls.contains(QStringLiteral("SetEnabled:off")));

        m_daemon->changeGuard->refuse = QStringLiteral("config.toml cannot be written");
        guard.setEnabled(true);
        QTRY_COMPARE(guard.actionError(), QStringLiteral("config.toml cannot be written"));
        QVERIFY(!guard.enabled());

        m_daemon->stop();
        QTRY_VERIFY(!guard.serviceAvailable());
    }

    /// What is held, counted as the daemon counts it (a folder's items too), in the page's
    /// order and words, and what was sent before the hold.
    void whatIsHeld()
    {
        start();
        HeldChanges held(QDBusConnection::sessionBus(), fake::FirstAccount);
        QTRY_VERIFY(held.serviceAvailable());
        QCOMPARE(held.count(), 0);
        QVERIFY(held.headline().isEmpty());

        hold(&held, rewrites() + deletions() + KonedriveHeldChangeList{sent(QStringLiteral("removal"), QStringLiteral("x"), today(13, 58))});
        QCOMPARE(held.count(), 4);
        const QVariantList kinds = held.kinds();
        QCOMPARE(kinds.size(), 2);
        QCOMPARE(text(kinds, 0, "kind"), QStringLiteral("removal"));
        QCOMPARE(text(kinds, 0, "title"), QStringLiteral("Deletions"));
        QCOMPARE(text(kinds, 0, "detail"), QStringLiteral("3 items · 1.5 MiB · since %1").arg(timeOfDay(14, 2)));
        QCOMPARE(text(kinds, 0, "countText"), QStringLiteral("3 deletions"));
        // Each button says how many it acts on, whatever the page's search shows.
        QCOMPARE(text(kinds, 0, "allowText"), QStringLiteral("Delete 3 in OneDrive"));
        QCOMPARE(text(kinds, 0, "restoreText"), QStringLiteral("Restore 3"));
        QVERIFY(text(kinds, 0, "note").isEmpty());
        QCOMPARE(text(kinds, 1, "title"), QStringLiteral("Changed files"));
        QCOMPARE(text(kinds, 1, "allowText"), QStringLiteral("Upload 1 Change"));
        QCOMPARE(text(kinds, 1, "restoreText"), QStringLiteral("Restore 1"));
        QCOMPARE(text(kinds, 1, "note"), QStringLiteral("Restore puts back OneDrive's version and keeps yours beside it as a copy."));
        QCOMPARE(held.headline(), QStringLiteral("3 deletions and 1 changed file are on hold"));
        QCOMPARE(held.summary(), QStringLiteral("4 changes on hold"));
        // Allow All's question names everything it sends.
        QCOMPARE(held.sendAllTitle(), QStringLiteral("Send All 4 Changes to OneDrive?"));
        QCOMPARE(held.sendAllText(), QStringLiteral("3 items will be deleted in OneDrive and 1 changed file will be uploaded."));
        QVERIFY(held.headDetail().startsWith(QStringLiteral("They were made in the folder since %1").arg(timeOfDay(14, 2))));
        QCOMPARE(held.sentKinds().size(), 1);
        QCOMPARE(text(held.sentKinds(), 0, "title"), QStringLiteral("1 deletion"));
        QVERIFY(held.removalsSent());

        hold(&held, deletions());
        QCOMPARE(held.summary(), QStringLiteral("3 deletions on hold"));
        // Moves and new files: their own words and buttons.
        KonedriveHeldChange move = heldRow(QStringLiteral("move"), QStringLiteral("Work/report-final.odt"), 0, today(15, 31));
        move.oldPath = QStringLiteral("Work/report.odt");
        hold(&held, {move, folder(QStringLiteral("upload"), QStringLiteral("site/node_modules"), 180 * MiB, 11204, today(16, 10))});
        QCOMPARE(text(held.kinds(), 0, "detail"), QStringLiteral("1 item · since %1").arg(timeOfDay(15, 31)));
        QCOMPARE(text(held.kinds(), 0, "restoreText"), QStringLiteral("Move 1 Back"));
        QCOMPARE(text(held.kinds(), 0, "allowText"), QStringLiteral("Move 1 in OneDrive"));
        QCOMPARE(text(held.kinds(), 1, "detail"), QStringLiteral("11,205 files · 180 MiB · since %1").arg(timeOfDay(16, 10)));
        QCOMPARE(text(held.kinds(), 1, "restoreText"), QStringLiteral("Don't Upload 11,205"));
        QCOMPARE(text(held.kinds(), 1, "allowText"), QStringLiteral("Upload 11,205"));
        QCOMPARE(held.sendAllText(), QStringLiteral("1 item will be moved in OneDrive and 11,205 new files will be uploaded."));
        QCOMPARE(text(held.kinds(), 1, "note"), QStringLiteral("Don't Upload keeps them on this computer only; Issues lists them."));

        m_daemon->stop();
        QTRY_VERIFY(!held.serviceAvailable());
        QCOMPARE(held.count(), 11206); // last known holds survive service loss
    }

    /// The open group's rows: by path as Dolphin sorts names, each with its folder, a folder's
    /// items, a changed file's sizes, where a move came from, and what Dolphin can show.
    void theRowsOfTheOpenGroup()
    {
        start();
        HeldChanges held(QDBusConnection::sessionBus(), fake::FirstAccount);
        held.setRootPath(QStringLiteral("/home/ann/OneDrive"));
        KonedriveHeldChange renamed = heldRow(QStringLiteral("move"), QStringLiteral("Work/report-final.odt"), 0, today(15, 31));
        renamed.oldPath = QStringLiteral("Work/report.odt");
        KonedriveHeldChange moved = heldRow(QStringLiteral("move"), QStringLiteral("Pictures/2026/IMG_10.jpg"), 0, today(15, 31));
        moved.oldPath = QStringLiteral("Pictures/Phone/IMG_10.jpg");
        KonedriveHeldChange moved9 = heldRow(QStringLiteral("move"), QStringLiteral("Pictures/2026/IMG_9.jpg"), 0, today(15, 31));
        moved9.oldPath = QStringLiteral("Pictures/Phone/IMG_9.jpg");
        QTRY_VERIFY(held.serviceAvailable());
        hold(&held, deletions() + rewrites() + KonedriveHeldChangeList{renamed, moved, moved9});

        HeldRowsModel *rows = held.rows();
        QCOMPARE(rows->rowCount(), 0);
        QVERIFY(!rows->folders());
        const auto at = [rows](int row, int role) {
            return rows->data(rows->index(row), role);
        };
        held.showRows(QStringLiteral("move"));
        QCOMPARE(rows->rowCount(), 3);
        // IMG_9 before IMG_10, as Dolphin sorts them.
        QCOMPARE(at(0, HeldRowsModel::NameRole).toString(), QStringLiteral("IMG_9.jpg"));
        QCOMPARE(at(1, HeldRowsModel::NameRole).toString(), QStringLiteral("IMG_10.jpg"));
        QCOMPARE(at(0, HeldRowsModel::DetailRole).toString(), QStringLiteral("Pictures/Phone → Pictures/2026"));
        QCOMPARE(at(2, HeldRowsModel::DetailRole).toString(), QStringLiteral("Renamed from report.odt · Work"));
        QCOMPARE(at(2, HeldRowsModel::LocalPathRole).toString(), QStringLiteral("/home/ann/OneDrive/Work/report-final.odt"));

        QVERIFY(!rows->folders());
        held.showRows(QStringLiteral("removal"));
        QCOMPARE(rows->rowCount(), 2);
        // A folder stands for what was in it, which the search does not look into.
        QVERIFY(rows->folders());
        QCOMPARE(at(1, HeldRowsModel::NameRole).toString(), QStringLiteral("Old"));
        QCOMPARE(at(1, HeldRowsModel::IconNameRole).toString(), QStringLiteral("folder"));
        QCOMPARE(at(1, HeldRowsModel::CountRole).toString(), QStringLiteral("1 item"));
        QCOMPARE(at(1, HeldRowsModel::DetailRole).toString(), QStringLiteral("Documents"));
        // A deleted item isn't there to show.
        QVERIFY(at(1, HeldRowsModel::LocalPathRole).toString().isEmpty());

        held.showRows(QStringLiteral("rewrite"));
        QCOMPARE(at(0, HeldRowsModel::SizeRole).toString(), QStringLiteral("48 KiB"));
        QCOMPARE(at(0, HeldRowsModel::AfterRole).toString(), QStringLiteral("0 B"));
        QVERIFY(at(0, HeldRowsModel::EmptiedRole).toBool());

        // What is resolved leaves the open group.
        hold(&held, deletions());
        QCOMPARE(rows->rowCount(), 0);
        held.showRows(QString());
        QCOMPARE(rows->rowCount(), 0);
    }

    /// Allow and Restore act on the token of what was listed, and say what they did.
    void decisionsOnTheListedToken()
    {
        start();
        HeldChanges held(QDBusConnection::sessionBus(), fake::FirstAccount);
        QTRY_VERIFY(held.serviceAvailable());
        hold(&held, deletions() + rewrites());
        const QString token = held.token();
        auto &calls = m_daemon->changeGuard->calls;

        held.restore(QStringLiteral("removal"));
        QTRY_COMPARE(held.results().size(), 1);
        QVERIFY(calls.contains(QStringLiteral("RestoreHeld:%1:%2:removal").arg(m_id, token)));
        QCOMPARE(text(held.results(), 0, "type"), QStringLiteral("positive"));
        QCOMPARE(text(held.results(), 0, "text"), QStringLiteral("Restored 3 items: they are back in the folder."));
        QTRY_COMPARE(held.count(), 1);

        // Every kind at once, each saying what it became.
        hold(&held, deletions() + rewrites());
        held.allow();
        QTRY_COMPARE(held.results().size(), 3);
        QVERIFY(calls.contains(QStringLiteral("AllowHeld:%1:%2:removal,rewrite").arg(m_id, held.token())));
        QCOMPARE(text(held.results(), 0, "text"), QStringLiteral("Deleting 3 items in OneDrive: they go to its recycle bin."));
        QCOMPARE(text(held.results(), 1, "text"), QStringLiteral("Uploading 1 changed file."));

        // OneDrive out of reach: they wait.
        hold(&held, deletions());
        m_daemon->changeGuard->answer = KonedriveGuardResult{0, 3, 0, 0, {}};
        held.allow(QStringLiteral("removal"));
        QTRY_COMPARE(held.results().size(), 4);
        QCOMPARE(text(held.results(), 0, "type"), QStringLiteral("information"));
        QCOMPARE(text(held.results(), 0, "text"), QStringLiteral("3 items wait to be deleted in OneDrive: they are deleted once OneDrive can be reached."));

        // Some failed: what failed stays on hold, and the daemon says why.
        hold(&held, deletions());
        m_daemon->changeGuard->answer = KonedriveGuardResult{2, 0, 1, 0, {{7, QStringLiteral("Documents/a.txt"), QStringLiteral("Close the writer, then try again.")}}};
        held.restore(QStringLiteral("removal"));
        QTRY_COMPARE(held.results().size(), 5);
        QCOMPARE(text(held.results(), 0, "type"), QStringLiteral("warning"));
        QCOMPARE(text(held.results(), 0, "text"), QStringLiteral("Restored 2 of 3 items. 1 is still on hold. Close the writer, then try again."));

        // The daemon's reason for each: one shared is said in the summary, and each row says
        // its own, until the next decision of its kind or until it is no longer held.
        KonedriveHeldChange report = heldRow(QStringLiteral("removal"), QStringLiteral("Documents/report.odt"), 10, today(14, 2));
        report.item = 2;
        hold(&held, deletions() + KonedriveHeldChangeList{report});
        const QString open = QStringLiteral("it is open in another program");
        const QString taken = QStringLiteral("a file of the same name is in its place");
        m_daemon->changeGuard->answer = KonedriveGuardResult{2, 0, 2, 0, {{1, QStringLiteral("Documents/a.txt"), open}, {2, QStringLiteral("Documents/report.odt"), open}}};
        held.restore(QStringLiteral("removal"));
        QTRY_COMPARE(held.results().size(), 6);
        QCOMPARE(text(held.results(), 0, "text"), QStringLiteral("Restored 2 of 4 items. 2 are still on hold, each because it is open in another program."));
        QTRY_COMPARE(held.count(), 2);
        held.showRows(QStringLiteral("removal"));
        HeldRowsModel *rows = held.rows();
        QCOMPARE(rows->rowCount(), 2);
        QCOMPARE(rows->data(rows->index(0), HeldRowsModel::NameRole).toString(), QStringLiteral("a.txt"));
        QCOMPARE(rows->data(rows->index(0), HeldRowsModel::FailureRole).toString(), QStringLiteral("It is open in another program"));
        // Its path stays, for the tooltip.
        QCOMPARE(rows->data(rows->index(0), HeldRowsModel::DetailToolTipRole).toString(), QStringLiteral("Documents/a.txt"));

        m_daemon->changeGuard->answer = KonedriveGuardResult{0, 0, 2, 0, {{1, QStringLiteral("Documents/a.txt"), open}, {2, QStringLiteral("Documents/report.odt"), taken}}};
        held.restore(QStringLiteral("removal"));
        QTRY_COMPARE(held.results().size(), 7);
        QCOMPARE(text(held.results(), 0, "text"), QStringLiteral("Restored 0 of 2 items. 2 are still on hold, each with its reason in the list."));
        QTRY_COMPARE(rows->data(rows->index(1), HeldRowsModel::FailureRole).toString(), QStringLiteral("A file of the same name is in its place"));
        QCOMPARE(rows->data(rows->index(0), HeldRowsModel::FailureRole).toString(), QStringLiteral("It is open in another program"));
        // No longer held: its reason goes with it.
        m_daemon->changeGuard->answer.reset();
        hold(&held, KonedriveHeldChangeList{report});
        QCOMPARE(rows->rowCount(), 1);
        QCOMPARE(rows->data(rows->index(0), HeldRowsModel::FailureRole).toString(), QStringLiteral("A file of the same name is in its place"));
        hold(&held, KonedriveHeldChangeList{report, heldRow(QStringLiteral("removal"), QStringLiteral("Documents/a.txt"), 10, today(14, 9))});
        QCOMPARE(rows->rowCount(), 2);
        QVERIFY(rows->data(rows->index(0), HeldRowsModel::FailureRole).toString().isEmpty());
        held.showRows(QString());

        // Refused: nothing was done.
        m_daemon->changeGuard->answer.reset();
        hold(&held, deletions());
        m_daemon->changeGuard->refuse = QStringLiteral("unknown change batch token; review the held changes again");
        held.restore(QString());
        QTRY_COMPARE(held.results().size(), 8);
        QCOMPARE(text(held.results(), 0, "type"), QStringLiteral("error"));
        QCOMPARE(text(held.results(), 0, "text"), QStringLiteral("Nothing was restored: unknown change batch token; review the held changes again"));

        const int id = held.results().constFirst().toMap().value(QStringLiteral("id")).toInt();
        held.closeResult(id);
        QCOMPARE(held.results().size(), 7);
    }

    /// Thousands of reasons for thousands of held folders: a listing keeps each with its row
    /// at once, never comparing every reason with every row.
    void manyReasonsListQuickly()
    {
        start();
        HeldChanges held(QDBusConnection::sessionBus(), fake::FirstAccount);
        QTRY_VERIFY(held.serviceAvailable());
        const int n = 20000;
        const QString open = QStringLiteral("it is open in another program");
        KonedriveHeldChangeList rows;
        KonedriveGuardResult answer;
        answer.failed = n;
        for (int i = 0; i < n; ++i) {
            KonedriveHeldChange row = heldRow(QStringLiteral("removal"), QStringLiteral("Folder-%1").arg(i), 0, today(14, 2));
            row.item = i;
            row.directory = true;
            rows << row;
            answer.failures << KonedriveGuardFailure{i, row.path, open};
        }
        hold(&held, rows);
        m_daemon->changeGuard->answer = answer;
        held.restore(QStringLiteral("removal"));
        QTRY_COMPARE_WITH_TIMEOUT(held.results().size(), 1, 120000);
        // The listing after the decision has them all.
        QTRY_VERIFY_WITH_TIMEOUT(!held.working(), 120000);

        QElapsedTimer timer;
        timer.start();
        m_daemon->changeGuard->hold(m_id, rows);
        QTRY_COMPARE_WITH_TIMEOUT(held.token(), m_daemon->changeGuard->token(), 120000);
        QVERIFY2(timer.elapsed() < 2000, qPrintable(QStringLiteral("listing took %1 ms").arg(timer.elapsed())));
        held.showRows(QStringLiteral("removal"));
        QCOMPARE(held.rows()->rowCount(), n);
        QCOMPARE(held.rows()->data(held.rows()->index(n - 1), HeldRowsModel::FailureRole).toString(), QStringLiteral("It is open in another program"));
    }

    /// A reason goes with its own change: a deletion and a new file at one path keep their own,
    /// and a folder that stands for what failed in it says the reason nearest its top.
    void reasonsGoWithTheirOwnChange()
    {
        start();
        HeldChanges held(QDBusConnection::sessionBus(), fake::FirstAccount);
        QTRY_VERIFY(held.serviceAvailable());
        KonedriveHeldChange removed = heldRow(QStringLiteral("removal"), QStringLiteral("Documents/notes.txt"), 10, today(14, 2));
        removed.item = 5;
        KonedriveHeldChange added = heldRow(QStringLiteral("upload"), QStringLiteral("Documents/notes.txt"), 10, today(14, 3));
        added.item = 6;
        KonedriveHeldChange site = folder(QStringLiteral("upload"), QStringLiteral("site"), MiB, 3, today(14, 4));
        site.item = 7;
        hold(&held, {removed, added, site});
        m_daemon->changeGuard->answer = KonedriveGuardResult{0,
                                                             0,
                                                             5,
                                                             0,
                                                             {{5, QStringLiteral("Documents/notes.txt"), QStringLiteral("it is open in another program")},
                                                              {6, QStringLiteral("Documents/notes.txt"), QStringLiteral("a file of the same name is in its place")},
                                                              {8, QStringLiteral("site/src/deep.txt"), QStringLiteral("the folder it belongs in is missing")},
                                                              {9, QStringLiteral("site/b.txt"), QStringLiteral("it changed since")},
                                                              {10, QStringLiteral("site/a.txt"), QStringLiteral("it is busy")}}};
        held.allow();
        QTRY_VERIFY(!held.working());
        QTRY_COMPARE(held.token(), m_daemon->changeGuard->token());
        held.showRows(QStringLiteral("removal"));
        HeldRowsModel *rows = held.rows();
        QCOMPARE(rows->rowCount(), 1);
        QCOMPARE(rows->data(rows->index(0), HeldRowsModel::FailureRole).toString(), QStringLiteral("It is open in another program"));
        held.showRows(QStringLiteral("upload"));
        QCOMPARE(rows->rowCount(), 2);
        QCOMPARE(rows->data(rows->index(0), HeldRowsModel::FailureRole).toString(), QStringLiteral("A file of the same name is in its place"));
        // Of what failed in the folder: the shortest path, and of those the first by path.
        QCOMPARE(rows->data(rows->index(1), HeldRowsModel::NameRole).toString(), QStringLiteral("site"));
        QCOMPARE(rows->data(rows->index(1), HeldRowsModel::FailureRole).toString(), QStringLiteral("It is busy"));
    }

    /// A restore under way: the kind is busy, with how far it is, until the answer comes.
    void aRestoreUnderWay()
    {
        start();
        HeldChanges held(QDBusConnection::sessionBus(), fake::FirstAccount);
        QTRY_VERIFY(held.serviceAvailable());
        hold(&held, deletions() + rewrites());
        m_daemon->changeGuard->wait = true;
        held.restore(QStringLiteral("removal"));
        QTRY_VERIFY(held.working());
        QTRY_COMPARE(m_daemon->changeGuard->waiting.size(), 1);
        QVERIFY(held.kinds().constFirst().toMap().value(QStringLiteral("working")).toBool());
        QVERIFY(!held.kinds().constLast().toMap().value(QStringLiteral("working")).toBool());
        m_daemon->changeGuard->sendProgress(m_id, held.token(), {QStringLiteral("removal")}, 1, 3);
        QTRY_COMPARE(text(held.kinds(), 0, "workingText"), QStringLiteral("Restoring… 1 of 3"));
        QCOMPARE(held.kinds().constFirst().toMap().value(QStringLiteral("progress")).toDouble(), 1.0 / 3.0);
        m_daemon->changeGuard->finishWaiting();
        QTRY_VERIFY(!held.working());
        QTRY_COMPARE(held.count(), 1);
    }

    /// One notification per account for what comes on hold, with Review… and the kind's
    /// Restore; more of a kind changes it in place, nothing held takes it back, and what was
    /// held when the app started is no news.
    void theNotification()
    {
        start();
        HeldChanges held(QDBusConnection::sessionBus(), fake::FirstAccount);
        QTRY_VERIFY(held.serviceAvailable());
        hold(&held, rewrites());
        AccountController account(fake::FirstAccount);
        ChangeGuardController guard;
        QTRY_VERIFY(guard.serviceAvailable());
        RecordingSink sink;
        ChangeNotifier notifier(&account, &held, &sink);
        notifier.setGuard(&guard);
        // Held before: the baseline.
        hold(&held, rewrites());
        QTest::qWait(50);
        QVERIFY(sink.sent.isEmpty());
        hold(&held, {});
        QVERIFY(sink.sent.isEmpty());

        KonedriveHeldChangeList many;
        for (int i = 0; i < 25; ++i) {
            many << heldRow(QStringLiteral("removal"), QStringLiteral("Old/f%1.txt").arg(i), 1024, today(14, 2));
        }
        hold(&held, many);
        QTRY_COMPARE(sink.sent.size(), 1);
        Notice notice = sink.sent.constLast();
        QCOMPARE(notice.event, QStringLiteral("changesHeld"));
        QCOMPARE(notice.key, QStringLiteral("changeGuard:") + m_id);
        QCOMPARE(notice.section, WindowSection::OnHold);
        QCOMPARE(notice.account, fake::FirstAccount);
        QCOMPARE(notice.title, QStringLiteral("25 deletions on hold"));
        // Where they were deleted: the folder they were all in.
        QCOMPARE(notice.text, QStringLiteral("25 items were deleted in “Old” within 10 minutes. They stay in OneDrive until you review them."));
        // Allowing is on the On Hold page only.
        QCOMPARE(buttons(notice), QStringList({QStringLiteral("Review…"), QStringLiteral("Restore")}));

        // More of the same kind: the words change in place.
        many << heldRow(QStringLiteral("removal"), QStringLiteral("Old/more.txt"), 1024, today(14, 4));
        hold(&held, many);
        QTRY_COMPARE(sink.updated.size(), 1);
        QCOMPARE(sink.sent.size(), 1);
        QCOMPARE(sink.updated.constLast().title, QStringLiteral("26 deletions on hold"));

        // Another kind: shown again, with every kind.
        hold(&held, many + rewrites());
        QTRY_COMPARE(sink.sent.size(), 2);
        notice = sink.sent.constLast();
        QCOMPARE(notice.title, QStringLiteral("27 changes on hold"));
        QCOMPARE(notice.text, QStringLiteral("26 deletions and 1 changed file in the OneDrive folder are not sent to OneDrive until you allow them."));
        QCOMPARE(buttons(notice), QStringList({QStringLiteral("Review…"), QStringLiteral("Restore All")}));

        // Resolved in the window: it is taken back.
        hold(&held, {});
        QTRY_COMPARE(sink.withdrawn.size(), 1);
        QVERIFY(sink.shown.isEmpty());
    }

    /// The notification's Restore acts on what it said, and what it did takes its place.
    void theNotificationsButton()
    {
        start();
        HeldChanges held(QDBusConnection::sessionBus(), fake::FirstAccount);
        QTRY_VERIFY(held.serviceAvailable());
        AccountController account(fake::FirstAccount);
        RecordingSink sink;
        ChangeNotifier notifier(&account, &held, &sink);
        notifier.setAccountName([] {
            return QStringLiteral("Work");
        });
        hold(&held, {});
        hold(&held, deletions());
        QTRY_COMPARE(sink.sent.size(), 1);
        QCOMPARE(sink.sent.constLast().title, QStringLiteral("3 deletions on hold — Work"));
        const QString shownToken = held.token();
        auto &calls = m_daemon->changeGuard->calls;

        sink.sent.constLast().actions.constFirst().run();
        QTRY_COMPARE(sink.sent.size(), 2);
        QVERIFY(calls.contains(QStringLiteral("RestoreHeld:%1:%2:removal").arg(m_id, shownToken)));
        Notice result = sink.sent.constLast();
        QCOMPARE(result.key, QStringLiteral("changeGuard:") + m_id);
        QCOMPARE(result.title, QStringLiteral("Restored 3 items — Work"));
        QCOMPARE(result.text, QStringLiteral("They are back in the OneDrive folder."));
        QVERIFY(buttons(result).isEmpty());
        // Nothing held any more: what the button did stays.
        QTest::qWait(400);
        QVERIFY(sink.withdrawn.isEmpty());
        // The window's decisions notify nothing.
        QVERIFY(held.results().isEmpty());

        // Some failed.
        hold(&held, {});
        hold(&held, deletions());
        QTRY_COMPARE(sink.sent.size(), 3);
        m_daemon->changeGuard->answer = KonedriveGuardResult{2, 0, 1, 0, {{7, QStringLiteral("Documents/a.txt"), QStringLiteral("Close the writer, then try again.")}}};
        sink.sent.constLast().actions.constFirst().run();
        QTRY_COMPARE(sink.sent.size(), 4);
        result = sink.sent.constLast();
        QCOMPARE(result.title, QStringLiteral("Restored 2 of 3 items — Work"));
        QCOMPARE(result.text, QStringLiteral("1 is still on hold. Close the writer, then try again."));
        QCOMPARE(buttons(result), QStringList({QStringLiteral("Review…")}));

        // The daemon's reasons, as it words them: one for all, or each its own on the page.
        hold(&held, {});
        hold(&held, deletions());
        QTRY_COMPARE(sink.sent.size(), 5);
        m_daemon->changeGuard->answer = KonedriveGuardResult{1, 0, 2, 0, {{1, QStringLiteral("Documents/a.txt"), QStringLiteral("it is <open> in another program")}}};
        sink.sent.constLast().actions.constFirst().run();
        QTRY_COMPARE(sink.sent.size(), 6);
        QCOMPARE(sink.sent.constLast().text, QStringLiteral("2 are still on hold, each because it is &lt;open&gt; in another program."));
        hold(&held, {});
        hold(&held, deletions());
        QTRY_COMPARE(sink.sent.size(), 7);
        m_daemon->changeGuard->answer = KonedriveGuardResult{1,
                                                             0,
                                                             2,
                                                             0,
                                                             {{1, QStringLiteral("Documents/a.txt"), QStringLiteral("it is open in another program")},
                                                              {2, QStringLiteral("Documents/Old"), QStringLiteral("the folder it belongs in is missing or has moved")}}};
        sink.sent.constLast().actions.constFirst().run();
        QTRY_COMPARE(sink.sent.size(), 8);
        QCOMPARE(sink.sent.constLast().text, QStringLiteral("2 are still on hold for different reasons: On Hold shows why for each."));

        // Refused.
        m_daemon->changeGuard->answer.reset();
        hold(&held, {});
        hold(&held, rewrites());
        QTRY_COMPARE(sink.sent.size(), 9);
        m_daemon->changeGuard->refuse = QStringLiteral("<b>no</b>");
        sink.sent.constLast().actions.constFirst().run();
        QTRY_COMPARE(sink.sent.size(), 10);
        result = sink.sent.constLast();
        QCOMPARE(result.title, QStringLiteral("Could not restore the changes on hold — Work"));
        // Plasma reads the text as markup.
        QCOMPARE(result.text, QStringLiteral("&lt;b&gt;no&lt;/b&gt;"));
    }

    /// Retained actions cannot decide a replacement notice, and one action is consumed once.
    void obsoleteNotificationActions()
    {
        start();
        HeldChanges held(QDBusConnection::sessionBus(), fake::FirstAccount);
        QTRY_VERIFY(held.serviceAvailable());
        AccountController account(fake::FirstAccount);
        RecordingSink sink;
        ChangeNotifier notifier(&account, &held, &sink);
        hold(&held, {});
        hold(&held, deletions());
        QTRY_COMPARE(sink.sent.size(), 1);
        const Notice old = sink.sent.constLast();
        const auto more = deletions() + KonedriveHeldChangeList{heldRow(QStringLiteral("removal"), QStringLiteral("Old/more.txt"), 10, today(14, 4))};
        hold(&held, more);
        QTRY_COMPARE(sink.updated.size(), 1);
        const Notice updated = sink.updated.constLast();
        hold(&held, more + rewrites());
        QTRY_COMPARE(sink.sent.size(), 2);
        const Notice current = sink.sent.constLast();
        const QString token = held.token();
        auto &calls = m_daemon->changeGuard->calls;
        const int before = calls.filter(QStringLiteral("RestoreHeld:")).size();
        old.actions.constFirst().run();
        updated.actions.constFirst().run();
        QTest::qWait(50);
        QCOMPARE(calls.filter(QStringLiteral("RestoreHeld:")).size(), before);
        current.actions.constFirst().run();
        current.actions.constFirst().run();
        QTRY_COMPARE(calls.filter(QStringLiteral("RestoreHeld:")).size(), before + 1);
        QVERIFY(calls.contains(QStringLiteral("RestoreHeld:%1:%2:removal,rewrite").arg(m_id, token)));
    }

    void olderRestoreKeepsNewerNotice_data()
    {
        QTest::addColumn<bool>("refused");
        QTest::newRow("success") << false;
        QTest::newRow("refused") << true;
    }

    /// A delayed completion for A must preserve B and B's subsequent in-place updates.
    void olderRestoreKeepsNewerNotice()
    {
        QFETCH(bool, refused);
        start();
        HeldChanges held(QDBusConnection::sessionBus(), fake::FirstAccount);
        QTRY_VERIFY(held.serviceAvailable());
        AccountController account(fake::FirstAccount);
        RecordingSink sink;
        ChangeNotifier notifier(&account, &held, &sink);
        hold(&held, {});
        hold(&held, deletions());
        QTRY_COMPARE(sink.sent.size(), 1);
        m_daemon->changeGuard->wait = true;
        sink.sent.constLast().actions.constFirst().run();
        QTRY_COMPARE(m_daemon->changeGuard->waiting.size(), 1);
        hold(&held, deletions() + rewrites());
        QTRY_COMPARE(sink.sent.size(), 2);
        if (refused) {
            m_daemon->changeGuard->refuse = QStringLiteral("restore failed");
        }
        m_daemon->changeGuard->finishWaiting();
        QTest::qWait(400);
        QCOMPARE(sink.sent.size(), 2);
        const QString key = QStringLiteral("changeGuard:") + m_id;
        QVERIFY(sink.shown.value(key).title.endsWith(QStringLiteral("on hold")));
        auto rows = held.heldRows();
        rows << heldRow(QStringLiteral("rewrite"), QStringLiteral("Book/another.md"), 40, today(14, 6));
        hold(&held, rows);
        QTRY_VERIFY(sink.shown.value(key).title.startsWith(refused ? QStringLiteral("5 changes") : QStringLiteral("2 changed files")));
    }

    /// Which limit held the changes: their size, or (deletions) the day's.
    void whichLimitTheWordsSay()
    {
        start();
        HeldChanges held(QDBusConnection::sessionBus(), fake::FirstAccount);
        QTRY_VERIFY(held.serviceAvailable());
        AccountController account(fake::FirstAccount);
        ChangeGuardController guard;
        QTRY_VERIFY(guard.serviceAvailable());
        RecordingSink sink;
        ChangeNotifier notifier(&account, &held, &sink);
        notifier.setGuard(&guard);
        hold(&held, {});
        hold(&held, {heldRow(QStringLiteral("removal"), QStringLiteral("big.mov"), 3 * GiB, today(14, 2))});
        QTRY_COMPARE(sink.sent.size(), 1);
        QCOMPARE(sink.sent.constLast().text, QStringLiteral("1 item, 3 GiB in all, was deleted in the OneDrive folder within 10 minutes. It stays in OneDrive until you allow it."));
        hold(&held, {});
        KonedriveHeldChangeList daily;
        for (int i = 0; i < 30; ++i) {
            daily << heldRow(QStringLiteral("removal"), QStringLiteral("f%1").arg(i), 10, today(0, 0) + i * 900);
        }
        hold(&held, daily);
        QTRY_COMPARE(sink.sent.size(), 2);
        QCOMPARE(sink.sent.constLast().text, QStringLiteral("30 items were deleted in the OneDrive folder within a day. They stay in OneDrive until you allow it."));
    }

    /// What the deletions notification names: the folder deleted with what was in it, by its
    /// name, or the folder they were all deleted in; at the top of the drive, no folder. Plasma reads the
    /// text as markup: a path is escaped.
    void whatTheDeletionsNotificationNames()
    {
        start();
        HeldChanges held(QDBusConnection::sessionBus(), fake::FirstAccount);
        QTRY_VERIFY(held.serviceAvailable());
        AccountController account(fake::FirstAccount);
        ChangeGuardController guard;
        QTRY_VERIFY(guard.serviceAvailable());
        RecordingSink sink;
        ChangeNotifier notifier(&account, &held, &sink);
        notifier.setGuard(&guard);
        hold(&held, {});

        hold(&held, {folder(QStringLiteral("removal"), QStringLiteral("Projects/site-redesign/Deliverables"), 400 * MiB, 1979, today(14, 2))});
        QTRY_COMPARE(sink.sent.size(), 1);
        QCOMPARE(sink.sent.constLast().title, QStringLiteral("1,980 deletions on hold"));
        QCOMPARE(sink.sent.constLast().text,
                 QStringLiteral("“Deliverables” and the 1,979 items in it were deleted in the OneDrive folder within 10 minutes. They stay in OneDrive until you review "
                                "them."));

        hold(&held, {});
        hold(&held, {folder(QStringLiteral("removal"), QStringLiteral("Videos/<b>2026 & more"), 3 * GiB, 5, today(14, 2))});
        QTRY_COMPARE(sink.sent.size(), 2);
        QCOMPARE(sink.sent.constLast().text,
                 QStringLiteral("“&lt;b&gt;2026 &amp; more” and the 5 items in it, 3 GiB in all, were deleted in the OneDrive folder within 10 minutes. They stay in "
                                "OneDrive until you review them."));

        KonedriveHeldChangeList spread;
        for (int i = 0; i < 12; ++i) {
            spread << heldRow(QStringLiteral("removal"), QStringLiteral("Projects/site-redesign/src/f%1.ts").arg(i), 10, today(14, 2));
            spread << heldRow(QStringLiteral("removal"), QStringLiteral("Projects/site-redesign/f%1.md").arg(i), 10, today(14, 2));
        }
        spread << folder(QStringLiteral("removal"), QStringLiteral("Projects/site-redesign/node_modules"), 10, 1000, today(14, 2));
        hold(&held, {});
        hold(&held, spread);
        QTRY_COMPARE(sink.sent.size(), 3);
        QCOMPARE(sink.sent.constLast().text, QStringLiteral("1,025 items were deleted in “Projects/site-redesign” within 10 minutes. They stay in OneDrive until you review them."));

        // Some at the top: the whole OneDrive folder is where.
        spread << heldRow(QStringLiteral("removal"), QStringLiteral("notes.txt"), 10, today(14, 2));
        hold(&held, {});
        hold(&held, spread);
        QTRY_COMPARE(sink.sent.size(), 4);
        QCOMPARE(sink.sent.constLast().text, QStringLiteral("1,026 items were deleted in the OneDrive folder within 10 minutes. They stay in OneDrive until you allow it."));
    }

    /// A folder's name with direction or control characters in it is named without them, so
    /// that it cannot turn the notification's words around.
    void theDeletionsNotificationCleansTheName()
    {
        start();
        HeldChanges held(QDBusConnection::sessionBus(), fake::FirstAccount);
        QTRY_VERIFY(held.serviceAvailable());
        AccountController account(fake::FirstAccount);
        RecordingSink sink;
        ChangeNotifier notifier(&account, &held, &sink);
        hold(&held, {});

        hold(&held, {folder(QStringLiteral("removal"), QStringLiteral("Videos/Holiday\u202Egpj.exe\n<b>"), 10, 30, today(14, 2))});
        QTRY_COMPARE(sink.sent.size(), 1);
        QCOMPARE(sink.sent.constLast().text,
                 QStringLiteral("“Holiday gpj.exe &lt;b&gt;” and the 30 items in it were deleted in the OneDrive folder within 10 minutes. They stay in OneDrive until you "
                                "review them."));

        KonedriveHeldChangeList spread;
        for (int i = 0; i < 25; ++i) {
            spread << heldRow(QStringLiteral("removal"), QStringLiteral("Work/a\u202Eb/f%1.txt").arg(i), 10, today(14, 2));
        }
        hold(&held, {});
        hold(&held, spread);
        QTRY_COMPARE(sink.sent.size(), 2);
        QCOMPARE(sink.sent.constLast().text, QStringLiteral("25 items were deleted in “Work/a b” within 10 minutes. They stay in OneDrive until you review them."));
    }

    /// What of the held kinds went out before the hold is not brought back by a restore: the
    /// notification says where it is, before and after Restore, with Open Recycle Bin for
    /// deletions, and so does the page's result.
    void whatWentOutBeforeTheHold()
    {
        start();
        HeldChanges held(QDBusConnection::sessionBus(), fake::FirstAccount);
        QTRY_VERIFY(held.serviceAvailable());
        AccountController account(fake::FirstAccount);
        RecordingSink sink;
        ChangeNotifier notifier(&account, &held, &sink);
        int recycleBinOpened = 0;
        notifier.setOpenRecycleBin([&recycleBinOpened] {
            ++recycleBinOpened;
        });
        hold(&held, {});

        KonedriveHeldChangeList rows;
        for (int i = 0; i < 25; ++i) {
            rows << heldRow(QStringLiteral("removal"), QStringLiteral("Old/f%1.txt").arg(i), 1024, today(14, 2));
        }
        const KonedriveHeldChangeList before{sent(QStringLiteral("removal"), QStringLiteral("Old/a.txt"), today(13, 58)),
                                             sent(QStringLiteral("removal"), QStringLiteral("Old/b.txt"), today(13, 59))};
        hold(&held, rows + before);
        QTRY_COMPARE(sink.sent.size(), 1);
        QCOMPARE(sink.sent.constLast().text,
                 QStringLiteral("25 items were deleted in “Old” within 10 minutes. They stay in OneDrive until you review them. 2 deleted before the hold are already in "
                                "OneDrive's recycle bin."));

        sink.sent.constLast().actions.constFirst().run();
        QTRY_COMPARE(sink.sent.size(), 2);
        const Notice result = sink.sent.constLast();
        QCOMPARE(result.title, QStringLiteral("Restored 25 items"));
        QCOMPARE(result.text, QStringLiteral("They are back in the OneDrive folder. 2 deleted before the hold are already in OneDrive's recycle bin."));
        QCOMPARE(buttons(result), QStringList(QStringLiteral("Open Recycle Bin")));
        result.actions.constFirst().run();
        QCOMPARE(recycleBinOpened, 1);
        QTRY_COMPARE(held.count(), 0);

        // The page's Restore says it too.
        hold(&held, rows + before);
        held.restore(QStringLiteral("removal"));
        QTRY_COMPARE(held.results().size(), 1);
        QCOMPARE(text(held.results(), 0, "text"), QStringLiteral("Restored 25 items: they are back in the folder. 2 deleted before the hold are already in OneDrive's recycle bin."));

        // Several kinds sent before the hold: said together.
        hold(&held, {});
        hold(&held, rows + rewrites() + before + KonedriveHeldChangeList{sent(QStringLiteral("rewrite"), QStringLiteral("Book/notes.md"), today(14, 1))});
        QTRY_COMPARE(sink.sent.size(), 4);
        QCOMPARE(sink.sent.constLast().text,
                 QStringLiteral("25 deletions and 1 changed file in the OneDrive folder are not sent to OneDrive until you allow them. 2 deletions and 1 changed file made "
                                "before the hold were already sent to OneDrive."));
        held.restore(QString());
        QTRY_COMPARE(held.results().size(), 3);
        QCOMPARE(text(held.results(), 0, "text"), QStringLiteral("Restored 25 items: they are back in the folder. 2 deleted before the hold are already in OneDrive's recycle bin."));
        QCOMPARE(text(held.results(), 1, "text"),
                 QStringLiteral("Restored 1 file to OneDrive's version. Yours is kept beside it, with this computer's name added. 1 changed before the hold is already "
                                "uploaded."));
    }

    /// A restore that partly failed still says what was sent before the hold, and the
    /// notification still offers the recycle bin.
    void aPartlyFailedRestoreSaysWhatWentBefore()
    {
        start();
        HeldChanges held(QDBusConnection::sessionBus(), fake::FirstAccount);
        QTRY_VERIFY(held.serviceAvailable());
        AccountController account(fake::FirstAccount);
        RecordingSink sink;
        ChangeNotifier notifier(&account, &held, &sink);
        int recycleBinOpened = 0;
        notifier.setOpenRecycleBin([&recycleBinOpened] {
            ++recycleBinOpened;
        });
        hold(&held, {});

        KonedriveHeldChangeList rows;
        for (int i = 0; i < 25; ++i) {
            rows << heldRow(QStringLiteral("removal"), QStringLiteral("Old/f%1.txt").arg(i), 1024, today(14, 2));
        }
        const KonedriveHeldChangeList before{sent(QStringLiteral("removal"), QStringLiteral("Old/a.txt"), today(13, 58)),
                                             sent(QStringLiteral("removal"), QStringLiteral("Old/b.txt"), today(13, 59))};
        hold(&held, rows + before);
        QTRY_COMPARE(sink.sent.size(), 1);
        m_daemon->changeGuard->answer = KonedriveGuardResult{24, 0, 1, 0, {{1, QStringLiteral("Old/f0.txt"), QStringLiteral("it is open in another program")}}};

        sink.sent.constLast().actions.constFirst().run();
        QTRY_COMPARE(sink.sent.size(), 2);
        const Notice result = sink.sent.constLast();
        QCOMPARE(result.title, QStringLiteral("Restored 24 of 25 items"));
        QCOMPARE(result.text,
                 QStringLiteral("1 is still on hold because it is open in another program. 2 deleted before the hold are already in OneDrive's recycle bin."));
        QCOMPARE(buttons(result), QStringList({QStringLiteral("Review…"), QStringLiteral("Open Recycle Bin")}));
        result.actions.constLast().run();
        QCOMPARE(recycleBinOpened, 1);

        // The page's Restore says it too.
        hold(&held, rows + before);
        held.restore(QStringLiteral("removal"));
        QTRY_COMPARE(held.results().size(), 1);
        QCOMPARE(text(held.results(), 0, "text"),
                 QStringLiteral("Restored 24 of 25 items. 1 is still on hold because it is open in another program. 2 deleted before the hold are already in OneDrive's "
                                "recycle bin."));
    }

    /// Issues' Upload for a file Don't Upload kept here: its path relative to the drive, and a
    /// refusal in actionError.
    void uploadingWhatWasKept()
    {
        start();
        HeldChanges held(QDBusConnection::sessionBus(), fake::FirstAccount);
        held.setRootPath(QStringLiteral("/home/ann/OneDrive"));
        QTRY_VERIFY(held.serviceAvailable());
        held.uploadLocal(QStringLiteral("/home/ann/OneDrive/site/node_modules"));
        QTRY_VERIFY(m_daemon->changeGuard->calls.contains(QStringLiteral("UploadLocal:%1:site/node_modules").arg(m_id)));
        QVERIFY(held.actionError().isEmpty());
        m_daemon->changeGuard->refuse = QStringLiteral("no such local-only entry");
        held.uploadLocal(QStringLiteral("/home/ann/OneDrive/gone.txt"));
        QTRY_COMPARE(held.actionError(), QStringLiteral("no such local-only entry"));
    }

    /// While changes are held and nothing worse goes on, Status says so and the tray shows the
    /// error state; a pause keeps its own words.
    void statusWhileHeld()
    {
        start();
        m_daemon->account->set({{QStringLiteral("State"), QStringLiteral("signed-in")}, {QStringLiteral("Mode"), QStringLiteral("read-write")}});
        m_daemon->sync->folder->set({{QStringLiteral("Path"), QStringLiteral("/home/ann/OneDrive")}, {QStringLiteral("State"), QStringLiteral("ready")}});
        m_daemon->sync->folder->decide("ok", "up-to-date");
        DaemonController manager;
        AccountsModel accounts(&manager);
        AppStatus app(&accounts);
        QTRY_COMPARE(accounts.count(), 1);
        AccountItem *item = accounts.at(0);
        QTRY_COMPARE(item->status()->state(), QStringLiteral("ok"));

        m_daemon->changeGuard->hold(m_id, deletions());
        QTRY_COMPARE(item->status()->state(), QStringLiteral("held"));
        QCOMPARE(item->status()->iconName(), QStringLiteral("state-error"));
        QCOMPARE(item->status()->headline(), QStringLiteral("Changes on hold"));
        QCOMPARE(item->status()->detail(), QStringLiteral("Everything else is up to date."));
        QCOMPARE(item->status()->text(), QStringLiteral("3 deletions on hold"));
        QTRY_COMPARE(app.state(), QStringLiteral("held"));
        QCOMPARE(app.toolTip(), QStringLiteral("3 deletions on hold"));
        QCOMPARE(app.onlyAccountNeedingAttention(), item);
        QTRY_COMPARE(accounts.heldCount(), 3);

        m_daemon->sync->folder->set({{QStringLiteral("Paused"), true}});
        QTRY_COMPARE(item->status()->headline(), QStringLiteral("Paused until you resume"));
        QVERIFY(item->status()->state() != QLatin1String("held"));

        m_daemon->sync->folder->set({{QStringLiteral("Paused"), false}});
        m_daemon->changeGuard->hold(m_id, {});
        QTRY_COMPARE(item->status()->state(), QStringLiteral("ok"));
        QTRY_COMPARE(accounts.heldCount(), 0);
    }
};

QTEST_GUILESS_MAIN(ChangeGuardTest)
#include "changeguardtest.moc"
