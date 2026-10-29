#include "fakedaemon.h"
#include "notifier.h"
#include "qmlregistration.h"
#include "writescope.h"

#include <KLocalizedQmlContext>
#include <KLocalizedString>

#include <QDesktopServices>
#include <QQmlApplicationEngine>
#include <QQuickItem>
#include <QQuickStyle>
#include <QQuickWindow>
#include <QSignalSpy>
#include <QTemporaryDir>
#include <QTest>

#include <memory>

namespace
{
const QString Root = QStringLiteral("/home/u/OneDrive");
}

/// Stands in for KNotification: nothing reaches the desktop.
class RecordingSink : public NotificationSink
{
public:
    void send(const Notice &notice) override { sent << notice; }
    QList<Notice> sent;
};

/// The app's objects and its window, Main.qml loaded as main() loads it.
struct App {
    Autostart autostart;
    DownloadProgressSettings progress;
    PlacesSettings places;
    TraySettings trayIcons;
    DaemonController daemon;
    AccountsModel accounts{&daemon};
    CurrentAccount current{&accounts};
    ServiceUnit serviceUnit;
    RecordingSink releaseNotices;
    ReleaseCheck releaseCheck{QStringLiteral(KONEDRIVE_VERSION), &releaseNotices};
    DownloadGuardController guard;
    ChangeGuardController changeGuard;
    QQmlApplicationEngine engine;
    QQuickWindow *window = nullptr;

    bool load()
    {
        registerKonedriveQml(&daemon, &accounts, &current, &autostart, &progress, &places, &trayIcons, &serviceUnit, &releaseCheck, &guard, &changeGuard);
        KLocalization::setupLocalizedContext(&engine);
        engine.loadFromModule("org.konedrive.app.window", "Main");
        window = engine.rootObjects().isEmpty() ? nullptr : qobject_cast<QQuickWindow *>(engine.rootObjects().constFirst());
        if (window) {
            window->show();
        }
        return window != nullptr;
    }

    /// The first item of the window's visual tree called `name`; hidden ones too.
    QQuickItem *find(const QString &name, QQuickItem *root = nullptr) const
    {
        const auto children = (root ? root : window->contentItem())->childItems();
        for (QQuickItem *item : children) {
            if (item->objectName() == name) {
                return item;
            }
            if (QQuickItem *found = find(name, item)) {
                return found;
            }
        }
        return nullptr;
    }

    /// The shown items called `name` under `root`.
    QList<QQuickItem *> allShown(const QString &name, QQuickItem *root = nullptr) const
    {
        QList<QQuickItem *> found;
        const auto children = (root ? root : window->contentItem())->childItems();
        for (QQuickItem *item : children) {
            if (!item->isVisible()) {
                continue;
            }
            if (item->objectName() == name) {
                found << item;
            }
            found << allShown(name, item);
        }
        return found;
    }

    void showPage(const QString &name) const { QMetaObject::invokeMethod(window, "showPage", Q_ARG(QVariant, name)); }

    QString pageShown() const
    {
        auto *stack = window->property("pageStack").value<QObject *>();
        auto *page = stack ? stack->property("currentItem").value<QObject *>() : nullptr;
        return page ? page->objectName() : QString();
    }

    AccountController *account() const { return accounts.count() > 0 ? accounts.at(0)->account() : nullptr; }
};

/// Two-way by default and a trial's folders: Sign In Read-Only Instead while a two-way
/// sign-in waits, the Two-way sync texts, the folders that can be changed in OneDrive
/// Settings, the refused-change notification with Allow This Folder, and Status' line.
/// Against the fake daemon on the private bus; offscreen, with XDG_CONFIG_HOME a temporary
/// directory.
class TrialTest : public QObject
{
    Q_OBJECT

public Q_SLOTS:
    /// QDesktopServices' handler of https: what Qt.openUrlExternally was given.
    void openUrl(const QUrl &url) { m_opened << url; }

private:
    QTemporaryDir m_config;
    QList<QUrl> m_opened;

    /// Signed in, two-way, with its folder at `root`.
    static void prepare(FakeDaemon &fake, const QString &root = Root)
    {
        fake.account->set({{QStringLiteral("State"), QStringLiteral("signed-in")}, {QStringLiteral("Mode"), QStringLiteral("read-write")}});
        // The whole window is loaded, and would open a sign-in address in the browser.
        fake.account->readWriteFails = QStringLiteral("cannot listen on localhost");
        fake.sync->folder->set({{QStringLiteral("Path"), root}, {QStringLiteral("State"), QStringLiteral("ready")}, {QStringLiteral("Source"), QStringLiteral("onedrive")}});
        fake.sync->folder->decide("ok", "up-to-date");
    }

    static void click(QQuickItem *button) { QMetaObject::invokeMethod(button, "clicked"); }

    static void toggle(QQuickItem *control, bool to)
    {
        control->setProperty("checked", to);
        QMetaObject::invokeMethod(control, "toggled");
    }

private Q_SLOTS:
    void initTestCase()
    {
        QVERIFY(m_config.isValid());
        qputenv("XDG_CONFIG_HOME", QFile::encodeName(m_config.path()));
        KLocalizedString::setApplicationDomain("konedrive");
        QQuickStyle::setStyle(QStringLiteral("org.kde.desktop"));
        QDesktopServices::setUrlHandler(QStringLiteral("https"), this, "openUrl");
    }

    void init() { m_opened.clear(); }

    /// A folder picked is relative to the OneDrive folder; adding one a folder listed covers
    /// changes nothing, and one that covers folders listed takes their place.
    void thePaths()
    {
        QCOMPARE(folderInside(Root, Root + QStringLiteral("/Projects/agent-sandbox")), QStringLiteral("Projects/agent-sandbox"));
        QCOMPARE(folderInside(Root, Root + QStringLiteral("/Projects/")), QStringLiteral("Projects"));
        QCOMPARE(folderInside(Root, Root), QString());
        QCOMPARE(folderInside(Root, Root + QStringLiteral("/")), QString());
        QCOMPARE(folderInside(Root, QStringLiteral("/home/u/OneDrive2/Projects")), QString());
        QCOMPARE(folderInside(Root, QStringLiteral("/home/u")), QString());
        QCOMPARE(folderInside(Root, Root + QStringLiteral("/a/../b")), QStringLiteral("b"));

        const QStringList listed{QStringLiteral("Projects/agent-sandbox"), QStringLiteral("Documents/Notes")};
        QCOMPARE(writeScopeWith(listed, QStringLiteral("Projects/agent-sandbox/src")), listed);
        QCOMPARE(writeScopeWith(listed, QStringLiteral("Documents/Notes")), listed);
        QCOMPARE(writeScopeWith(listed, QStringLiteral("Projects")), (QStringList{QStringLiteral("Documents/Notes"), QStringLiteral("Projects")}));
        // A name that only starts like a folder listed is another folder.
        QCOMPARE(writeScopeWith(listed, QStringLiteral("Documents/Notes 2")),
                 (QStringList{QStringLiteral("Projects/agent-sandbox"), QStringLiteral("Documents/Notes"), QStringLiteral("Documents/Notes 2")}));
    }

    /// The controller shows what the daemon says, and asks the scope with each change made
    /// from what it asked last; the last folder stays; a refusal says why.
    void theController()
    {
        FakeDaemon fake;
        prepare(fake);
        QVERIFY(fake.start());
        AccountController account(fake::FirstAccount);
        QTRY_VERIFY(account.serviceAvailable());
        QVERIFY(!account.writeScopeEnabled());

        // Off: the folder starts a trial.
        account.allowFolder(QStringLiteral("Projects/agent-sandbox"));
        QVERIFY(account.writeScopeChanging());
        QTRY_VERIFY(account.writeScopeEnabled());
        QTRY_VERIFY(!account.writeScopeChanging());
        QCOMPARE(account.writePaths(), QStringList{QStringLiteral("Projects/agent-sandbox")});
        QCOMPARE(fake.account->calls.last(), QStringLiteral("SetWriteScope:Projects/agent-sandbox"));

        // Covered already: nothing asked.
        const int asked = fake.account->calls.size();
        account.allowFolder(QStringLiteral("Projects/agent-sandbox/src"));
        QCOMPARE(fake.account->calls.size(), asked);

        // Two quick changes: the second starts from the first.
        account.allowFolder(QStringLiteral("Documents/Notes"));
        account.allowFolder(QStringLiteral("Pictures"));
        QTRY_COMPARE(account.writePaths(), (QStringList{QStringLiteral("Projects/agent-sandbox"), QStringLiteral("Documents/Notes"), QStringLiteral("Pictures")}));
        QTRY_VERIFY(!account.writeScopeChanging());

        account.removeFolder(QStringLiteral("Pictures"));
        account.removeFolder(QStringLiteral("Documents/Notes"));
        QTRY_COMPARE(account.writePaths(), QStringList{QStringLiteral("Projects/agent-sandbox")});
        QTRY_VERIFY(!account.writeScopeChanging());
        // The last one stays.
        const int before = fake.account->calls.size();
        account.removeFolder(QStringLiteral("Projects/agent-sandbox"));
        QCOMPARE(fake.account->calls.size(), before);

        // Refused: why, and the list as the daemon keeps it.
        fake.account->refuseWriteScope = QStringLiteral("config.toml cannot be written");
        account.allowFolder(QStringLiteral("Music"));
        QTRY_COMPARE(account.actionError(), QStringLiteral("The folders that can be changed were not saved: config.toml cannot be written"));
        QTRY_VERIFY(!account.writeScopeChanging());
        QCOMPARE(account.writePaths(), QStringList{QStringLiteral("Projects/agent-sandbox")});
        account.clearWriteScope();
        QTRY_COMPARE(account.actionError(), QStringLiteral("Changes were not allowed everywhere: config.toml cannot be written"));
        fake.account->refuseWriteScope.clear();

        // Everywhere: the list is forgotten.
        account.clearWriteScope();
        QTRY_VERIFY(!account.writeScopeEnabled());
        QVERIFY(account.writePaths().isEmpty());
        QCOMPARE(fake.account->calls.last(), QStringLiteral("ClearWriteScope"));
        QVERIFY(account.actionError().isEmpty());
    }

    /// Sign In is two-way, the daemon's default. While it waits, a read-only sign-in can be
    /// asked instead: the two-way one is cancelled first, its end ends nothing, and the new
    /// one opens the browser and adds the account.
    void readOnlyInstead()
    {
        FakeDaemon fake({});
        QVERIFY(fake.start());
        DaemonController daemon;
        AccountsModel model(&daemon);
        QSignalSpy opened(&model, &AccountsModel::openUrlRequested);
        QSignalSpy added(&model, &AccountsModel::accountAdded);
        QTRY_VERIFY(daemon.serviceAvailable());

        model.addAccount(QString());
        QTRY_COMPARE(opened.size(), 1);
        QCOMPARE(fake.manager->calls, QStringList{QStringLiteral("SignIn")});
        QVERIFY(model.addingMode().isEmpty());
        const uint twoWay = fake.signIn;

        model.addReadOnlyInstead();
        QCOMPARE(model.addingMode(), QStringLiteral("read-only"));
        QVERIFY(model.signInUrl().isEmpty());
        QTRY_COMPARE(opened.size(), 2);
        QCOMPARE(fake.manager->calls,
                 (QStringList{QStringLiteral("SignIn"), QStringLiteral("CancelSignIn:%1").arg(twoWay), QStringLiteral("Add:read-only")}));
        QVERIFY(opened.last().at(0).toString().contains(QStringLiteral("mode=read-only")));
        QCOMPARE(model.signInUrl(), opened.last().at(0).toString());
        QVERIFY(model.adding());
        QVERIFY(model.addError().isEmpty());
        // Asked once: the read-only sign-in has no Instead.
        model.addReadOnlyInstead();
        QTest::qWait(50);
        QCOMPARE(fake.manager->calls.size(), 3);

        fake.finishSignIn(QStringLiteral("signed-in"), QStringLiteral("ann@example.com"));
        QTRY_COMPARE(added.size(), 1);
        QVERIFY(!model.adding());
        QVERIFY(model.addingMode().isEmpty());

        // The two-way sign-in ended by itself meanwhile (the browser had finished): no
        // read-only sign-in, and the resulting account is switched before onboarding.
        fake.manager->calls.clear();
        fake.manager->cancelEnds = false;
        model.addAccount(QString());
        QTRY_COMPARE(opened.size(), 3);
        model.addReadOnlyInstead();
        QTRY_VERIFY(fake.manager->calls.contains(QStringLiteral("CancelSignIn:%1").arg(fake.signIn)));
        QTest::qWait(50);
        QCOMPARE(model.addingMode(), QStringLiteral("read-only"));
        QVERIFY(model.adding());
        fake.finishSignIn(QStringLiteral("signed-in"), QStringLiteral("bob@example.com"));
        QTRY_COMPARE(added.size(), 2);
        QCOMPARE(fake.objects.last()->account->mode(), QStringLiteral("read-only"));
        QVERIFY(!fake.manager->calls.contains(QStringLiteral("Add:read-only")));

        // Cancelled while the read-only one starts: nothing is left waiting.
        fake.manager->calls.clear();
        fake.manager->cancelEnds = true;
        model.addAccount(QString());
        QTRY_COMPARE(opened.size(), 4);
        model.addReadOnlyInstead();
        model.cancelAdd();
        QTRY_VERIFY(!model.adding());
        QTest::qWait(100);
        QCOMPARE(fake.signIn, 0u);
        QVERIFY(model.addError().isEmpty());
    }

    void readOnlyWinsBrowserCompletion_data()
    {
        QTest::addColumn<bool>("signalFirst");
        QTest::newRow("reply-first") << false;
        QTest::newRow("signal-first") << true;
    }

    /// Choosing read-only survives either ordering of the cancellation reply and browser
    /// completion. accountAdded is emitted only once Mode has been verified on the daemon.
    void readOnlyWinsBrowserCompletion()
    {
        QFETCH(bool, signalFirst);
        FakeDaemon fake({});
        fake.manager->cancelEnds = false;
        fake.manager->finishOnCancel = signalFirst;
        QVERIFY(fake.start());
        DaemonController daemon;
        AccountsModel model(&daemon);
        QSignalSpy added(&model, &AccountsModel::accountAdded);
        bool readOnlyWhenAdded = false;
        connect(&model, &AccountsModel::accountAdded, &model, [&] {
            readOnlyWhenAdded = fake.objects.last()->account->mode() == QLatin1String("read-only");
        });
        QTRY_VERIFY(daemon.serviceAvailable());
        model.addAccount();
        QTRY_VERIFY(!model.signInUrl().isEmpty());
        model.addReadOnlyInstead();
        QTRY_VERIFY(!fake.manager->calls.filter(QStringLiteral("CancelSignIn:")).isEmpty());
        if (!signalFirst) {
            QTest::qWait(50); // the false reply has arrived, the browser has not
            QCOMPARE(model.addingMode(), QStringLiteral("read-only"));
            QVERIFY(added.isEmpty());
            fake.finishSignIn(QStringLiteral("signed-in"), QStringLiteral("race@example.com"));
        }
        QTRY_COMPARE(added.size(), 1);
        QVERIFY(readOnlyWhenAdded);
        QVERIFY(!model.adding());
        QVERIFY(model.addError().isEmpty());
        QCOMPARE(fake.objects.last()->account->calls, QStringList{QStringLiteral("SetMode:read-only:force")});
        QTRY_COMPARE(model.find(added.first().at(0).toString())->account()->mode(), QStringLiteral("read-only"));
        QVERIFY(!fake.manager->calls.contains(QStringLiteral("Add:read-only")));
    }

    /// A change refused outside the trial: once, where, with Allow This Folder (which adds the
    /// folder); none at the top. Within the next 10 minutes the rest are counted and said once,
    /// without a button.
    void theRefusalNotice()
    {
        FakeDaemon fake;
        prepare(fake);
        fake.account->setWriteScope({QStringLiteral("Projects/agent-sandbox")});
        QVERIFY(fake.start());
        AccountController account(fake::FirstAccount);
        SyncController sync(fake::FirstAccount);
        RecordingSink sink;
        qint64 now = 1000000;
        Notifier notifier(&account, &sync, &sink, [&now] {
            return now;
        });
        QTRY_VERIFY(account.serviceAvailable() && account.writeScopeEnabled());

        fake.account->refuseWrite(QStringLiteral("Documents/Taxes/2025/return.pdf"), QStringLiteral("Documents/Taxes/2025"));
        QTRY_COMPARE(sink.sent.size(), 1);
        Notice first = sink.sent.first();
        QCOMPARE(first.event, QStringLiteral("writeRefused"));
        QCOMPARE(first.section, WindowSection::WriteScope);
        QCOMPARE(first.account, fake::FirstAccount);
        QCOMPARE(first.title, QStringLiteral("Change refused"));
        QCOMPARE(first.text, QStringLiteral("“Documents/Taxes/2025” can't be changed on this computer: only the folders you chose can."));
        QCOMPARE(first.actions.size(), 1);
        QCOMPARE(first.actions.first().text, QStringLiteral("Allow This Folder"));
        first.actions.first().run();
        QTRY_COMPARE(account.writePaths(), (QStringList{QStringLiteral("Projects/agent-sandbox"), QStringLiteral("Documents/Taxes/2025")}));

        // The daemon's later reports, counted: nothing more for 10 minutes, then one summary.
        fake.account->refuseWrite(QStringLiteral("Music/a.mp3"), QStringLiteral("Music"), 20);
        fake.account->refuseWrite(QStringLiteral("Music/b.mp3"), QStringLiteral("Music"), 3);
        QTest::qWait(200);
        QCOMPARE(sink.sent.size(), 1);
        now += Notifier::WindowMs;
        notifier.flushDue();
        QCOMPARE(sink.sent.size(), 1);
        now += Notifier::RefusedWindowMs;
        notifier.flushDue();
        QCOMPARE(sink.sent.size(), 2);
        QCOMPARE(sink.sent.last().title, QStringLiteral("Change refused"));
        QCOMPARE(sink.sent.last().text, QStringLiteral("23 more changes were refused outside the folders that can be changed."));
        QVERIFY(sink.sent.last().actions.isEmpty());

        // A new window: at the top, no button; a report of several counts the rest.
        now += 2 * Notifier::RefusedWindowMs;
        notifier.flushDue();
        fake.account->refuseWrite(QStringLiteral("notes.txt"), QString(), 4, QStringLiteral("exe:/usr/bin/python3"));
        QTRY_COMPARE(sink.sent.size(), 3);
        QCOMPARE(sink.sent.last().text, QStringLiteral("The top of your OneDrive can't be changed on this computer: only the folders you chose can."));
        QVERIFY(sink.sent.last().actions.isEmpty());
        now += Notifier::RefusedWindowMs;
        notifier.flushDue();
        QCOMPARE(sink.sent.size(), 4);
        QCOMPARE(sink.sent.last().text, QStringLiteral("3 more changes were refused outside the folders that can be changed."));

        // Allow This Folder once the trial has ended: nothing is asked.
        now += 2 * Notifier::RefusedWindowMs;
        notifier.flushDue();
        fake.account->refuseWrite(QStringLiteral("Videos/x.mov"), QStringLiteral("Videos"));
        QTRY_COMPARE(sink.sent.size(), 5);
        account.clearWriteScope();
        QTRY_VERIFY(!account.writeScopeEnabled());
        const int asked = fake.account->calls.size();
        sink.sent.last().actions.first().run();
        QTest::qWait(50);
        QCOMPARE(fake.account->calls.size(), asked);

        // Escaped: the folder's name is the drive's own text.
        now += 2 * Notifier::RefusedWindowMs;
        notifier.flushDue();
        fake.account->refuseWrite(QStringLiteral("<b>x</b>/y"), QStringLiteral("<b>x</b>"));
        QTRY_COMPARE(sink.sent.size(), 6);
        QVERIFY(sink.sent.last().text.contains(QStringLiteral("&lt;b&gt;x&lt;/b&gt;")));
    }

    /// The sign-in page: Sign In is two-way, and only its wait offers a read-only sign-in.
    void theSignInPage()
    {
        FakeDaemon fake({});
        QVERIFY(fake.start());
        App app;
        QVERIFY(app.load());
        QTRY_VERIFY(app.daemon.serviceAvailable());
        QTRY_COMPARE(app.pageShown(), QStringLiteral("signInPage"));
        QQuickItem *intro = app.find(QStringLiteral("addIntro"));
        QQuickItem *signIn = app.find(QStringLiteral("signInButton"));
        QQuickItem *instead = app.find(QStringLiteral("signInReadOnly"));
        QQuickItem *cancel = app.find(QStringLiteral("cancelSignIn"));
        QVERIFY(intro && signIn && instead && cancel);
        QVERIFY(intro->property("explanation").toString().endsWith(QStringLiteral("Files download when you open them, and what you change there changes in OneDrive too.")));
        QVERIFY(!instead->isVisible());

        click(signIn);
        QTRY_COMPARE(m_opened.size(), 1);
        QCOMPARE(fake.manager->calls, QStringList{QStringLiteral("SignIn")});
        QTRY_VERIFY(instead->isVisible() && instead->isEnabled());
        QVERIFY(cancel->isVisible());

        click(instead);
        QTRY_COMPARE(m_opened.size(), 2);
        QVERIFY(fake.manager->calls.contains(QStringLiteral("Add:read-only")));
        QTRY_VERIFY(!instead->isVisible());
        QVERIFY(cancel->isVisible());
        QVERIFY(app.find(QStringLiteral("copySignInLink"))->isEnabled());

        click(cancel);
        QTRY_VERIFY(intro->isVisible());
        QVERIFY(!instead->isVisible());
    }

    /// OneDrive Settings: Two-way sync's words follow the mode and Hold large changes; the
    /// trial switch only two-way. Its folders, the last one's Remove off, Add Folder… through
    /// the folder dialog in the OneDrive folder (one outside asks again), and turning it off,
    /// which asks first. A refused change's notification leads to this card.
    void theSyncCard()
    {
        // The dialog opens in the OneDrive folder, which is there.
        QTemporaryDir home;
        QVERIFY(home.isValid());
        const QString Root = home.path() + QStringLiteral("/OneDrive");
        QVERIFY(QDir().mkpath(Root + QStringLiteral("/Projects/agent-sandbox")));
        FakeDaemon fake;
        prepare(fake, Root);
        QVERIFY(fake.start());
        App app;
        QVERIFY(app.load());
        QTRY_VERIFY(app.account() && app.account()->serviceAvailable() && app.changeGuard.serviceAvailable());
        QTRY_VERIFY(app.accounts.at(0)->sync()->rootPath() == Root);
        app.showPage(QStringLiteral("oneDriveSettings"));
        QTRY_COMPARE(app.pageShown(), QStringLiteral("oneDriveSettingsPage"));
        QQuickItem *twoWay = app.find(QStringLiteral("twoWaySwitch"));
        QQuickItem *notHeld = app.find(QStringLiteral("notHeldLine"));
        QQuickItem *trial = app.find(QStringLiteral("trialSwitch"));
        QQuickItem *add = app.find(QStringLiteral("addTrialFolder"));
        QVERIFY(twoWay && notHeld && trial && add);

        QTRY_COMPARE(twoWay->property("description").toString(),
                     QStringLiteral("What you add, change, move or delete here happens in OneDrive too. Large changes wait for your review, and deleted files go to OneDrive's recycle bin."));
        QVERIFY(!notHeld->isVisible());
        QVERIFY(trial->isVisible());
        QVERIFY(!trial->property("checked").toBool());
        QCOMPARE(trial->property("description").toString(),
                 QStringLiteral("For a trial: changes are allowed only in folders you choose, and the rest of your OneDrive stays read-only here."));
        QVERIFY(!add->isVisible());

        // Hold large changes off in Settings.
        fake.changeGuard->setEnabled(false);
        QTRY_COMPARE(twoWay->property("description").toString(),
                     QStringLiteral("What you add, change, move or delete here happens in OneDrive too. Deleted files go to OneDrive's recycle bin."));
        QTRY_VERIFY(notHeld->isVisible());
        fake.changeGuard->setEnabled(true);
        QTRY_VERIFY(!notHeld->isVisible());

        // Turning it on picks the first folder; one outside the OneDrive asks again.
        auto *dialog = app.window->findChild<QObject *>(QStringLiteral("trialFolderDialog"));
        QVERIFY(dialog);
        toggle(trial, true);
        QVERIFY(!trial->property("checked").toBool());
        QCOMPARE(dialog->property("title").toString(), QStringLiteral("Choose a Folder That Can Be Changed"));
        QCOMPARE(dialog->property("currentFolder").toUrl(), QUrl::fromLocalFile(Root));
        QMetaObject::invokeMethod(dialog, "pick", Q_ARG(QVariant, QUrl::fromLocalFile(home.path())));
        QCOMPARE(dialog->property("title").toString(), QStringLiteral("Choose a Folder Inside Your OneDrive Folder"));
        QCOMPARE(dialog->property("currentFolder").toUrl(), QUrl::fromLocalFile(Root));
        QVERIFY(!fake.account->calls.join(QLatin1Char(' ')).contains(QStringLiteral("SetWriteScope")));
        QMetaObject::invokeMethod(dialog, "pick", Q_ARG(QVariant, QUrl::fromLocalFile(Root)));
        QVERIFY(!fake.account->calls.join(QLatin1Char(' ')).contains(QStringLiteral("SetWriteScope")));
        QMetaObject::invokeMethod(dialog, "pick", Q_ARG(QVariant, QUrl::fromLocalFile(Root + QStringLiteral("/Projects/agent-sandbox"))));
        QTRY_VERIFY(fake.account->calls.contains(QStringLiteral("SetWriteScope:Projects/agent-sandbox")));
        QTRY_VERIFY(trial->property("checked").toBool());
        QCOMPARE(trial->property("description").toString(), QStringLiteral("The rest of your OneDrive stays read-only on this computer."));
        QTRY_VERIFY(add->isVisible());

        // One folder: its Remove is off.
        QTRY_COMPARE(app.allShown(QStringLiteral("trialFolder")).size(), 1);
        QQuickItem *row = app.allShown(QStringLiteral("trialFolder")).first();
        QCOMPARE(row->property("text").toString(), QStringLiteral("Projects/agent-sandbox"));
        QVERIFY(!app.find(QStringLiteral("removeTrialFolder"), row)->isEnabled());

        // Add Folder…: a folder that covers the one listed takes its place.
        click(add);
        QCOMPARE(dialog->property("title").toString(), QStringLiteral("Choose a Folder That Can Be Changed"));
        QMetaObject::invokeMethod(dialog, "pick", Q_ARG(QVariant, QUrl::fromLocalFile(Root + QStringLiteral("/Documents/Notes"))));
        QTRY_COMPARE(app.allShown(QStringLiteral("trialFolder")).size(), 2);
        click(add);
        QMetaObject::invokeMethod(dialog, "pick", Q_ARG(QVariant, QUrl::fromLocalFile(Root + QStringLiteral("/Projects"))));
        QTRY_VERIFY(fake.account->calls.contains(QStringLiteral("SetWriteScope:Documents/Notes|Projects")));
        QTRY_COMPARE(app.account()->writePaths(), (QStringList{QStringLiteral("Documents/Notes"), QStringLiteral("Projects")}));

        // Two: each can be removed.
        QTRY_COMPARE(app.allShown(QStringLiteral("trialFolder")).size(), 2);
        row = app.allShown(QStringLiteral("trialFolder")).first();
        QTRY_VERIFY(app.find(QStringLiteral("removeTrialFolder"), row)->isEnabled());
        click(app.find(QStringLiteral("removeTrialFolder"), row));
        QTRY_VERIFY(fake.account->calls.contains(QStringLiteral("SetWriteScope:Projects")));
        QTRY_COMPARE(app.allShown(QStringLiteral("trialFolder")).size(), 1);

        // Off asks first: Cancel keeps the trial.
        auto *prompt = app.window->findChild<QObject *>(QStringLiteral("allowEverywhereDialog"));
        QVERIFY(prompt);
        toggle(trial, false);
        QTRY_VERIFY(prompt->property("opened").toBool());
        QVERIFY(trial->property("checked").toBool());
        QCOMPARE(prompt->property("title").toString(), QStringLiteral("Allow Changes Everywhere?"));
        QCOMPARE(prompt->property("subtitle").toString(),
                 QStringLiteral("Every folder in this OneDrive can then be changed from this computer, and the list of folders is forgotten. Large changes are still held for your review."));
        QMetaObject::invokeMethod(prompt, "close");
        QTRY_VERIFY(!prompt->property("opened").toBool());
        QVERIFY(!fake.account->calls.contains(QStringLiteral("ClearWriteScope")));

        // A refused change leads here.
        app.showPage(QStringLiteral("status"));
        QTRY_COMPARE(app.pageShown(), QStringLiteral("statusPage"));
        QMetaObject::invokeMethod(app.window, "showSection", Q_ARG(QVariant, sectionName(WindowSection::WriteScope)));
        QTRY_COMPARE(app.pageShown(), QStringLiteral("oneDriveSettingsPage"));

        // Allow Everywhere.
        trial = app.find(QStringLiteral("trialSwitch"));
        toggle(trial, false);
        QTRY_VERIFY(prompt->property("opened").toBool());
        QObject *everywhere = prompt->findChild<QObject *>(QStringLiteral("allowEverywhere"));
        QVERIFY(everywhere);
        QMetaObject::invokeMethod(everywhere, "trigger");
        QTRY_VERIFY(fake.account->calls.contains(QStringLiteral("ClearWriteScope")));
        QTRY_VERIFY(!trial->property("checked").toBool());
        QTRY_COMPARE(app.allShown(QStringLiteral("trialFolder")).size(), 0);
        QVERIFY(!add->isVisible());

        // Read-only: no trial switch, and its own words.
        fake.account->set({{QStringLiteral("Mode"), QStringLiteral("read-only")}});
        QTRY_VERIFY(!trial->isVisible());
        QCOMPARE(twoWay->property("description").toString(),
                 QStringLiteral("Your files download and open as usual, but can't be changed here, and nothing is uploaded. Turning it on opens your browser once more to allow KOneDrive to change your files."));
    }

    /// Status during a trial: one line on the head card, two-way only.
    void theStatusLine()
    {
        FakeDaemon fake;
        prepare(fake);
        fake.account->setWriteScope({QStringLiteral("Projects/agent-sandbox"), QStringLiteral("Documents/Notes")});
        QVERIFY(fake.start());
        App app;
        QVERIFY(app.load());
        QTRY_VERIFY(app.account() && app.account()->writeScopeEnabled());
        app.showPage(QStringLiteral("status"));
        QQuickItem *line = app.find(QStringLiteral("trialLine"));
        QVERIFY(line);
        QTRY_VERIFY(line->isVisible());
        QCOMPARE(line->property("text").toString(), QStringLiteral("Changes are allowed only in 2 folders. OneDrive Settings lists them."));
        fake.account->setWriteScope({QStringLiteral("Projects/agent-sandbox")});
        QTRY_COMPARE(line->property("text").toString(), QStringLiteral("Changes are allowed only in 1 folder. OneDrive Settings lists it."));
        fake.account->set({{QStringLiteral("Mode"), QStringLiteral("read-only")}});
        QTRY_VERIFY(!line->isVisible());
        fake.account->set({{QStringLiteral("Mode"), QStringLiteral("read-write")}, {QStringLiteral("WriteScopeEnabled"), false}, {QStringLiteral("WritePaths"), QStringList()}});
        QTest::qWait(100);
        QVERIFY(!line->isVisible());
    }
};

QTEST_MAIN(TrialTest)
#include "trialtest.moc"
