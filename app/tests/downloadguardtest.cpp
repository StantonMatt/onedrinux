#include "downloadguardcontroller.h"
#include "fakedaemon.h"
#include "notifier.h"

#include <KLocalizedString>

#include <QDir>
#include <QFile>
#include <QLocale>
#include <QSignalSpy>
#include <QTemporaryDir>
#include <QTest>

#include <memory>

namespace
{
const QString Copier = QStringLiteral("app:org.example.Copier");
const QString Rg = QStringLiteral("exe:/usr/bin/rg");
const QString Backup = QStringLiteral("exe:/opt/backup/run-backup");
const QString Term = QStringLiteral("app:org.example.Term");
const QString Baloo = QStringLiteral("exe:/usr/bin/baloo_file");
const QString Cat = QStringLiteral("exe:/usr/bin/cat");
constexpr qulonglong GiB = 1ULL << 30;

/// The desktop's entries, for the tests: Copier's is "Copier" with an icon of its own; any
/// other app keeps the daemon's name, and an executable is named as resolve() names it.
GuardApp standIn(const QString &identity, const QString &displayName)
{
    GuardApp app;
    app.iconName = QStringLiteral("application-x-executable");
    if (identity == Copier) {
        app.name = QStringLiteral("Copier");
        app.iconName = QStringLiteral("copier-icon");
        return app;
    }
    if (identity.startsWith(QLatin1String("app:"))) {
        app.name = DownloadGuardController::cleanName(displayName);
        return app;
    }
    return DownloadGuardController::resolve(identity, displayName);
}

QStringList column(const QVariantList &rows, const char *key)
{
    QStringList values;
    for (const QVariant &row : rows) {
        values << row.toMap().value(QLatin1String(key)).toString();
    }
    return values;
}
}

/// Stands in for KNotification: what was sent, and what Plasma would show now, by key.
class RecordingSink : public NotificationSink
{
public:
    void send(const Notice &notice) override
    {
        sent << notice;
        if (!notice.key.isEmpty()) {
            shown.insert(notice.key, notice);
        }
    }
    void withdraw(const QString &key) override
    {
        withdrawn << key;
        shown.remove(key);
    }

    QList<Notice> sent;
    QHash<QString, Notice> shown;
    QStringList withdrawn;
};

/// DownloadGuardController and GuardNotifier against the fake daemon's DownloadGuard on the
/// private bus: what the rows say and in which order, the limits in words, what each control
/// and each button of a notification calls, which blocks notify, and that an app has one
/// notification at a time.
class DownloadGuardTest : public QObject
{
    Q_OBJECT

private:
    QTemporaryDir m_data;
    std::unique_ptr<FakeDaemon> m_daemon;
    std::unique_ptr<DownloadGuardController> m_guard;
    RecordingSink m_sink;
    std::unique_ptr<GuardNotifier> m_notifier;

    void start(const QList<FakeGuardApp> &apps = {})
    {
        m_daemon = std::make_unique<FakeDaemon>();
        m_daemon->guard->apps = apps;
        QVERIFY(m_daemon->start());
        m_guard = std::make_unique<DownloadGuardController>();
        m_guard->setResolver(standIn);
        m_notifier = std::make_unique<GuardNotifier>(m_guard.get(), &m_sink);
        QTRY_VERIFY(m_guard->serviceAvailable());
        QTRY_COMPARE(m_guard->listed().size(), apps.size());
    }

    /// The app's notification, as shown now.
    Notice shownFor(const QString &identity) const { return m_sink.shown.value(QStringLiteral("downloadGuard:") + identity); }

    static QStringList buttons(const Notice &notice)
    {
        QStringList texts;
        for (const NoticeAction &action : notice.actions) {
            texts << action.text;
        }
        return texts;
    }

private Q_SLOTS:
    void initTestCase()
    {
        // The expected names and data sizes below use English collation and formatting.
        QLocale::setDefault(QLocale(QLocale::English, QLocale::UnitedStates));
        KLocalizedString::setApplicationDomain("konedrive");
        // Desktop entries of the test's own, for KService.
        QVERIFY(m_data.isValid());
        qputenv("XDG_DATA_HOME", QFile::encodeName(m_data.path()));
        QVERIFY(QDir(m_data.path()).mkpath(QStringLiteral("applications")));
        QFile entry(m_data.filePath(QStringLiteral("applications/org.konedrive.test.Copier.desktop")));
        QVERIFY(entry.open(QIODevice::WriteOnly));
        entry.write("[Desktop Entry]\nType=Application\nName=Bulk Copier\nIcon=konedrive-test-copier\nExec=true\n");
        entry.close();
    }

    void init()
    {
        m_sink = RecordingSink();
    }

    void cleanup()
    {
        m_notifier.reset();
        m_guard.reset();
        if (m_daemon) {
            m_daemon->stop();
        }
        m_daemon.reset();
    }

    /// The rows: blocked first, the newest block on top, then allowed for this session,
    /// always allowed, always blocked, by name within each. An app counted as usual and a
    /// reader always refused have none; Status has only those that wait for the user.
    void theRowsAndTheirOrder()
    {
        start({
            {Cat, QStringLiteral("cat")},
            {Term, QStringLiteral("org.example.Term"), QStringLiteral("allowed-always")},
            {Backup, QStringLiteral("run-backup"), QStringLiteral("blocked-always"), true, QStringLiteral("user")},
            {Rg, QStringLiteral("rg"), QStringLiteral("allowed-session")},
            {Baloo, QStringLiteral("baloo_file"), QStringLiteral("automatic"), true, QStringLiteral("background")},
            {DownloadGuardController::Unidentified, QStringLiteral("Unknown process"), QStringLiteral("automatic"), true, QStringLiteral("unidentified")},
            {Copier, QStringLiteral("org.example.Copier"), QStringLiteral("automatic"), true, QStringLiteral("bulk"), 30, 900ULL << 20},
            {QStringLiteral("exe:/usr/bin/big"), QStringLiteral("big"), QStringLiteral("automatic"), true, QStringLiteral("bulk"), 4, 1536ULL << 20},
        });
        QVERIFY(m_guard->enabled());
        QCOMPARE(m_guard->maxFiles(), 30u);
        QCOMPARE(m_guard->maxBytes(), 2 * GiB);
        QCOMPARE(m_guard->windowSeconds(), 600u);

        const QVariantList rows = m_guard->apps();
        QCOMPARE(column(rows, "name"), QStringList({QStringLiteral("big"),
                                                    QStringLiteral("Copier"),
                                                    QStringLiteral("Unknown program"),
                                                    QStringLiteral("rg"),
                                                    QStringLiteral("org.example.Term"),
                                                    QStringLiteral("run-backup")}));
        QCOMPARE(column(rows, "state"), QStringList({QStringLiteral("blocked"),
                                                     QStringLiteral("blocked"),
                                                     QStringLiteral("unidentified"),
                                                     QStringLiteral("allowed-session"),
                                                     QStringLiteral("allowed-always"),
                                                     QStringLiteral("blocked-always")}));
        const QLocale locale;
        QCOMPARE(column(rows, "stateText"), QStringList({QStringLiteral("Blocked after ") + locale.formattedDataSize(1536LL << 20),
                                                         QStringLiteral("Blocked after 30 files"),
                                                         QStringLiteral("Blocked: it couldn't be identified"),
                                                         QStringLiteral("Allowed for this session"),
                                                         QStringLiteral("Always allowed"),
                                                         QStringLiteral("Always blocked")}));
        QCOMPARE(rows.at(1).toMap().value(QStringLiteral("iconName")).toString(), QStringLiteral("copier-icon"));
        QCOMPARE(column(m_guard->blockedApps(), "name"), QStringList({QStringLiteral("big"), QStringLiteral("Copier"), QStringLiteral("Unknown program")}));

        // A block that comes now goes on top; the list follows the daemon's.
        m_daemon->guard->block(Rg, QStringLiteral("rg"), 30, 0);
        QTRY_COMPARE(column(m_guard->blockedApps(), "name").value(0), QStringLiteral("rg"));
        QTRY_VERIFY(m_daemon->guard->calls.count(QStringLiteral("ListApps")) >= 2);
        // Blocked by the limits and then always blocked: the rule is what holds.
        m_daemon->guard->find(Copier)->policy = QStringLiteral("blocked-always");
        m_daemon->guard->sendAppsChanged();
        QTRY_COMPARE(column(m_guard->apps(), "name").mid(4), QStringList({QStringLiteral("Copier"), QStringLiteral("run-backup")}));
        QCOMPARE(column(m_guard->apps(), "state").mid(4), QStringList({QStringLiteral("blocked-always"), QStringLiteral("blocked-always")}));
    }

    /// A single file over the byte limit is refused before anything is counted.
    void aFileOverTheLimitOnItsOwn()
    {
        start({{Rg, QStringLiteral("rg"), QStringLiteral("automatic"), true, QStringLiteral("bulk"), 0, 0}});
        QCOMPARE(column(m_guard->apps(), "stateText"), QStringList(QStringLiteral("Blocked at a file larger than ") + QLocale().formattedDataSize(qint64(2 * GiB), 0)));
    }

    /// The switch's description says the limits the daemon keeps, as they change.
    void theLimitsInWords()
    {
        start();
        const QLocale locale;
        QCOMPARE(m_guard->description(),
                 QStringLiteral("Indexing and previews never download files. An app that downloads more than 30 files or ") + locale.formattedDataSize(qint64(2 * GiB), 0)
                     + QStringLiteral(" within 10 minutes is blocked until you allow it."));
        QSignalSpy changed(m_guard.get(), &DownloadGuardController::changed);
        m_daemon->guard->set({{QStringLiteral("MaxFiles"), 1u}, {QStringLiteral("MaxBytes"), QVariant::fromValue<qulonglong>(1536ULL << 20)}, {QStringLiteral("WindowSeconds"), 90u}});
        QTRY_COMPARE(m_guard->windowSeconds(), 90u);
        QCOMPARE(m_guard->description(),
                 QStringLiteral("Indexing and previews never download files. An app that downloads more than 1 file or ") + locale.formattedDataSize(1536LL << 20)
                     + QStringLiteral(" within 90 seconds is blocked until you allow it."));
        // The dialog's whole numbers.
        QCOMPARE(m_guard->maxGiB(), 2u);
        QCOMPARE(m_guard->windowMinutes(), 2u);
        QVERIFY(!changed.isEmpty());
    }

    /// Each control calls its method; the daemon's answer is what shows.
    void eachControlCallsItsMethod()
    {
        start({{Copier, QStringLiteral("org.example.Copier"), QStringLiteral("automatic"), true, QStringLiteral("bulk"), 30, 0},
               {Rg, QStringLiteral("rg"), QStringLiteral("allowed-session")}});
        auto &calls = m_daemon->guard->calls;

        m_guard->setEnabled(false);
        QTRY_VERIFY(!m_guard->enabled());
        QVERIFY(calls.contains(QStringLiteral("SetEnabled:off")));

        m_guard->setLimits(50, 3, 15);
        QTRY_VERIFY(calls.contains(QStringLiteral("SetThresholds:50:%1:900").arg(3 * GiB)));
        QTRY_COMPARE(m_guard->maxFiles(), 50u);
        // A number left as the dialog showed it keeps the daemon's exact value.
        m_daemon->guard->set({{QStringLiteral("MaxBytes"), QVariant::fromValue<qulonglong>(1536ULL << 20)}, {QStringLiteral("WindowSeconds"), 90u}});
        QTRY_COMPARE(m_guard->windowSeconds(), 90u);
        m_guard->setLimits(60, m_guard->maxGiB(), m_guard->windowMinutes());
        QTRY_VERIFY(calls.contains(QStringLiteral("SetThresholds:60:%1:90").arg(1536ULL << 20)));

        m_guard->allowApp(Copier, false);
        QTRY_VERIFY(calls.contains(QStringLiteral("AllowApp:%1:session").arg(Copier)));
        QTRY_COMPARE(column(m_guard->apps(), "state"), QStringList({QStringLiteral("allowed-session"), QStringLiteral("allowed-session")}));
        m_guard->allowApp(Copier, true);
        QTRY_VERIFY(calls.contains(QStringLiteral("AllowApp:%1:always").arg(Copier)));
        m_guard->blockApp(Copier);
        QTRY_VERIFY(calls.contains(QStringLiteral("BlockApp:") + Copier));
        m_guard->resetApp(Rg);
        QTRY_VERIFY(calls.contains(QStringLiteral("ResetApp:") + Rg));
        QTRY_COMPARE(column(m_guard->apps(), "name"), QStringList(QStringLiteral("Copier")));
        QVERIFY(m_guard->actionError().isEmpty());

        // Free Up What It Downloaded: under way, then what it did, until it is cleared.
        m_daemon->guard->freedFiles = 3;
        m_daemon->guard->freedBytes = 5ULL << 20;
        m_guard->freeUpAppDownloads(Copier);
        QCOMPARE(m_guard->freeingUp(), Copier);
        QTRY_VERIFY(m_guard->freeingUp().isEmpty());
        QVERIFY(calls.contains(QStringLiteral("FreeUpAppDownloads:") + Copier));
        QCOMPARE(m_guard->freeUpResult(), QStringLiteral("Freed 3 files (%1).").arg(QLocale().formattedDataSize(5LL << 20)));
        m_guard->clearFreeUpResult();
        QVERIFY(m_guard->freeUpResult().isEmpty());
        m_daemon->guard->freedFiles = 0;
        m_guard->freeUpAppDownloads(Copier);
        QTRY_VERIFY(m_guard->freeUpResult().startsWith(QStringLiteral("Nothing was freed up.")));
    }

    /// A refused call says why, and the rows are read again.
    void aRefusalSaysWhy()
    {
        start({{Copier, QStringLiteral("org.example.Copier"), QStringLiteral("automatic"), true, QStringLiteral("bulk"), 30, 0}});
        m_daemon->guard->refuse = QStringLiteral("config.toml cannot be written");
        const qsizetype listed = m_daemon->guard->calls.count(QStringLiteral("ListApps"));
        m_guard->allowApp(Copier, true);
        QTRY_COMPARE(m_guard->actionError(), QStringLiteral("config.toml cannot be written"));
        QTRY_VERIFY(m_daemon->guard->calls.count(QStringLiteral("ListApps")) > listed);
        QCOMPARE(column(m_guard->apps(), "state"), QStringList(QStringLiteral("blocked")));
        // The next call starts clean.
        m_daemon->guard->refuse.clear();
        m_guard->allowApp(Copier, true);
        QTRY_VERIFY(m_guard->actionError().isEmpty());
        // Zero limits are the daemon's InvalidArgs.
        m_guard->setLimits(0, 1, 1);
        QTRY_VERIFY(!m_guard->actionError().isEmpty());
    }

    /// The daemon gone takes its blocks with it; back, its list is read again.
    void aDaemonRestart()
    {
        start({{Copier, QStringLiteral("org.example.Copier"), QStringLiteral("automatic"), true, QStringLiteral("bulk"), 30, 0}});
        m_daemon->stop();
        QTRY_VERIFY(!m_guard->serviceAvailable());
        QVERIFY(m_guard->apps().isEmpty());
        m_daemon->guard->apps = {{Rg, QStringLiteral("rg"), QStringLiteral("allowed-always")}};
        QVERIFY(m_daemon->start());
        QTRY_VERIFY(m_guard->serviceAvailable());
        QTRY_COMPARE(column(m_guard->apps(), "name"), QStringList(QStringLiteral("rg")));
    }

    /// An app by its desktop entry, an executable by its file's name, and a name that is the
    /// program's own say cleaned of what could disguise it.
    void namesAndIcons()
    {
        const GuardApp entry = DownloadGuardController::resolve(QStringLiteral("app:org.konedrive.test.Copier"), QStringLiteral("org.konedrive.test.Copier"));
        QCOMPARE(entry.name, QStringLiteral("Bulk Copier"));
        QCOMPARE(entry.iconName, QStringLiteral("konedrive-test-copier"));
        const GuardApp none = DownloadGuardController::resolve(QStringLiteral("app:org.konedrive.test.Missing"), QStringLiteral("org.konedrive.test.Missing"));
        QCOMPARE(none.name, QStringLiteral("org.konedrive.test.Missing"));
        QCOMPARE(none.iconName, QStringLiteral("application-x-executable"));
        const GuardApp exe = DownloadGuardController::resolve(Rg, QString());
        QCOMPARE(exe.name, QStringLiteral("rg"));
        QCOMPARE(DownloadGuardController::resolve(DownloadGuardController::Unidentified, QStringLiteral("Unknown process")).name, QStringLiteral("Unknown program"));

        QCOMPARE(DownloadGuardController::cleanName(QStringLiteral("evil") + QChar(0x202E) + QStringLiteral("txt.exe\nname\t ")), QStringLiteral("evil txt.exe name"));
        const QString long_ = DownloadGuardController::cleanName(QString(200, QLatin1Char('a')));
        QCOMPARE(long_.size(), 80);
        QVERIFY(long_.endsWith(QChar(0x2026)));
    }

    /// An app blocked for downloading too much: one notification, naming no account, with its
    /// four buttons; the text says which limit it went over.
    void aBulkBlockNotifies()
    {
        start();
        m_daemon->guard->block(Copier, QStringLiteral("org.example.Copier"), 30, 900ULL << 20);
        QTRY_COMPARE(m_sink.sent.size(), 1);
        const Notice notice = m_sink.sent.constFirst();
        QCOMPARE(notice.event, QStringLiteral("downloadsBlocked"));
        QCOMPARE(notice.section, WindowSection::GuardApps);
        QVERIFY(notice.account.isEmpty());
        QCOMPARE(notice.title, QStringLiteral("Too many downloads by Copier"));
        QCOMPARE(notice.text, QStringLiteral("Copier downloaded 30 files from OneDrive in 10 minutes. To save space, its downloads are blocked until you allow them."));
        QCOMPARE(notice.iconName, QStringLiteral("copier-icon"));
        QCOMPARE(buttons(notice),
                 QStringList({QStringLiteral("Allow for This Session"),
                              QStringLiteral("Always Allow"),
                              QStringLiteral("Always Block"),
                              QStringLiteral("Free Up What It Downloaded")}));

        m_daemon->guard->block(Rg, QStringLiteral("rg"), 7, 2 * GiB - 1);
        QTRY_COMPARE(m_sink.sent.size(), 2);
        QCOMPARE(m_sink.sent.at(1).text,
                 QStringLiteral("rg downloaded %1 from OneDrive in 10 minutes. To save space, its downloads are blocked until you allow them.")
                     .arg(QLocale().formattedDataSize(qint64(2 * GiB - 1))));
        QCOMPARE(m_sink.sent.at(1).iconName, QStringLiteral("application-x-executable"));

        m_daemon->guard->block(Backup, QStringLiteral("run-backup"), 0, 0);
        QTRY_COMPARE(m_sink.sent.size(), 3);
        QCOMPARE(m_sink.sent.at(2).text,
                 QStringLiteral("run-backup tried to download a file larger than %1 from OneDrive. To save space, its downloads are blocked until you allow them.")
                     .arg(QLocale().formattedDataSize(qint64(2 * GiB), 0)));
    }

    void disablingTheGuardWithdrawsBlockedNotices()
    {
        start();
        m_daemon->guard->block(Copier, QStringLiteral("org.example.Copier"), 30, 0);
        m_daemon->guard->block(DownloadGuardController::Unidentified, QStringLiteral("Unknown process"), 0, 0, QStringLiteral("unidentified"));
        QTRY_COMPARE(m_sink.shown.size(), 2);
        m_daemon->guard->set({{QStringLiteral("Enabled"), false}});
        QTRY_VERIFY(!m_guard->enabled());
        QTRY_VERIFY(m_sink.shown.isEmpty());
        QCOMPARE(m_sink.withdrawn.size(), 2);
        QCOMPARE(m_guard->listed().size(), 2); // No AppsChanged is needed to withdraw them.
    }

    /// Each button calls its method; Free Up says what it freed.
    void eachButtonCallsItsMethod()
    {
        start();
        m_daemon->guard->block(Copier, QStringLiteral("org.example.Copier"), 30, 0);
        QTRY_COMPARE(m_sink.sent.size(), 1);
        const QList<NoticeAction> actions = m_sink.sent.constFirst().actions;
        QCOMPARE(actions.size(), 4);
        auto &calls = m_daemon->guard->calls;

        actions.at(0).run();
        QTRY_VERIFY(calls.contains(QStringLiteral("AllowApp:%1:session").arg(Copier)));
        actions.at(1).run();
        QTRY_VERIFY(calls.contains(QStringLiteral("AllowApp:%1:always").arg(Copier)));
        actions.at(2).run();
        QTRY_VERIFY(calls.contains(QStringLiteral("BlockApp:") + Copier));

        m_daemon->guard->freedFiles = 12;
        m_daemon->guard->freedBytes = 300ULL << 20;
        const qsizetype sent = m_sink.sent.size();
        actions.at(3).run();
        QTRY_VERIFY(calls.contains(QStringLiteral("FreeUpAppDownloads:") + Copier));
        QTRY_COMPARE(m_sink.sent.size(), sent + 1);
        const Notice freed = m_sink.sent.constLast();
        QCOMPARE(freed.title, QStringLiteral("Freed up what Copier downloaded"));
        QCOMPARE(freed.text, QStringLiteral("Freed 12 files (%1).").arg(QLocale().formattedDataSize(300LL << 20)));
        QVERIFY(freed.actions.isEmpty());
    }

    /// A button the daemon refuses says so.
    void aRefusedButtonSaysSo()
    {
        start();
        m_daemon->guard->block(Copier, QStringLiteral("org.example.Copier"), 30, 0);
        QTRY_COMPARE(m_sink.sent.size(), 1);
        m_daemon->guard->refuse = QStringLiteral("config.toml cannot be written");
        m_sink.sent.constFirst().actions.at(1).run();
        QTRY_COMPARE(m_sink.sent.size(), 2);
        QCOMPARE(m_sink.sent.at(1).title, QStringLiteral("Could not change what Copier may download"));
        QCOMPARE(m_sink.sent.at(1).text, QStringLiteral("config.toml cannot be written"));
        m_sink.sent.constFirst().actions.at(3).run();
        QTRY_COMPARE(m_sink.sent.size(), 3);
        QCOMPARE(m_sink.sent.at(2).title, QStringLiteral("Could not free up what Copier downloaded"));
    }

    /// A reader always refused and an app always blocked are no news; a program that could not
    /// be identified is, with nothing to press.
    void whichBlocksNotify()
    {
        start();
        m_daemon->guard->block(Baloo, QStringLiteral("baloo_file"), 0, 0, QStringLiteral("background"));
        m_daemon->guard->block(Backup, QStringLiteral("run-backup"), 0, 0, QStringLiteral("user"));
        m_daemon->guard->block(DownloadGuardController::Unidentified, QStringLiteral("Unknown process"), 0, 0, QStringLiteral("unidentified"));
        QTRY_COMPARE(m_sink.sent.size(), 1);
        QTest::qWait(100);
        QCOMPARE(m_sink.sent.size(), 1);
        const Notice notice = m_sink.sent.constFirst();
        QCOMPARE(notice.title, QStringLiteral("Download blocked"));
        QCOMPARE(notice.text, QStringLiteral("A program was blocked from downloading from OneDrive because KOneDrive couldn't tell which program it was."));
        QVERIFY(notice.actions.isEmpty());
        QCOMPARE(notice.section, WindowSection::GuardApps);
    }

    /// One notification per app at a time: another block takes the place of the one shown,
    /// and one whose app is no longer blocked is withdrawn, also when the daemon goes.
    void oneNotificationPerApp()
    {
        start();
        m_daemon->guard->block(Copier, QStringLiteral("org.example.Copier"), 30, 0);
        m_daemon->guard->block(Rg, QStringLiteral("rg"), 30, 0);
        QTRY_COMPARE(m_sink.sent.size(), 2);
        // Reset and blocked again before the first was dismissed.
        m_daemon->guard->block(Copier, QStringLiteral("org.example.Copier"), 30, 0);
        QTRY_COMPARE(m_sink.sent.size(), 3);
        QCOMPARE(m_sink.shown.size(), 2);

        // Allowed in the window: its notification goes; the other app's stays.
        m_guard->allowApp(Copier, false);
        QTRY_VERIFY(m_sink.withdrawn.contains(QStringLiteral("downloadGuard:") + Copier));
        QCOMPARE(m_sink.shown.keys(), QStringList(QStringLiteral("downloadGuard:") + Rg));
        QCOMPARE(shownFor(Rg).title, QStringLiteral("Too many downloads by rg"));

        m_daemon->stop();
        QTRY_VERIFY(m_sink.shown.isEmpty());
    }

    /// Plasma reads the text as markup: the program's own name is escaped there, and the
    /// title, plain text, keeps it as it is.
    void aNameIsNeverMarkup()
    {
        start();
        const QString name = QStringLiteral("<b>Evil</b> & co");
        m_daemon->guard->block(QStringLiteral("exe:/tmp/x"), name, 30, 0);
        QTRY_COMPARE(m_sink.sent.size(), 1);
        QCOMPARE(m_sink.sent.constFirst().title, QStringLiteral("Too many downloads by <b>Evil</b> & co"));
        QVERIFY(m_sink.sent.constFirst().text.startsWith(QStringLiteral("&lt;b&gt;Evil&lt;/b&gt; &amp; co downloaded")));
    }
};

QTEST_GUILESS_MAIN(DownloadGuardTest)
#include "downloadguardtest.moc"
