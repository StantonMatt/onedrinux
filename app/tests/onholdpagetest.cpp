#include "fakedaemon.h"
#include "notifier.h"
#include "qmlregistration.h"

#include <KLocalizedQmlContext>
#include <KLocalizedString>

#include <QDateTime>
#include <QDir>
#include <QImage>
#include <QQmlApplicationEngine>
#include <QQuickItem>
#include <QQuickStyle>
#include <QQuickWindow>
#include <QTemporaryDir>
#include <QTest>

namespace
{
qint64 today(int hours, int minutes)
{
    return QDateTime(QDate::currentDate(), QTime(hours, minutes)).toSecsSinceEpoch();
}

KonedriveHeldChangeList deletions(int count = 3)
{
    KonedriveHeldChangeList rows;
    for (int i = 0; i < count; ++i) {
        rows << heldRow(QStringLiteral("removal"), QStringLiteral("Documents/file-%1.txt").arg(i), 1024, today(14, 2));
    }
    return rows;
}

KonedriveHeldChangeList rewrites()
{
    return {heldRow(QStringLiteral("rewrite"), QStringLiteral("Book/chapter-01.md"), 48 * 1024, today(14, 5))};
}
}

/// Drops the release check's notification: it never asks GitHub here.
class DroppedNotices : public NotificationSink
{
public:
    void send(const Notice &) override { }
};

/// The app's objects and its window, Main.qml loaded as main() loads it, with the change
/// guard's controller its own.
struct App {
    Autostart autostart;
    DownloadProgressSettings progress;
    PlacesSettings places;
    TraySettings trayIcons;
    DaemonController daemon;
    AccountsModel accounts{&daemon};
    CurrentAccount current{&accounts};
    ServiceUnit serviceUnit;
    DroppedNotices releaseNotices;
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

    /// A shown item called `name` under `root`.
    QQuickItem *shown(const QString &name, QQuickItem *root = nullptr) const
    {
        const auto children = (root ? root : window->contentItem())->childItems();
        for (QQuickItem *item : children) {
            if (item->objectName() == name && item->isVisible()) {
                return item;
            }
            if (QQuickItem *found = shown(name, item)) {
                return found;
            }
        }
        return nullptr;
    }

    void showPage(const QString &name) const { QMetaObject::invokeMethod(window, "showPage", Q_ARG(QVariant, name)); }
    void showSection(const QString &name) const { QMetaObject::invokeMethod(window, "showSection", Q_ARG(QVariant, name)); }

    QString pageShown() const
    {
        auto *stack = window->property("pageStack").value<QObject *>();
        auto *page = stack ? stack->property("currentItem").value<QObject *>() : nullptr;
        return page ? page->objectName() : QString();
    }

    HeldChanges *held() const { return accounts.count() > 0 ? accounts.at(0)->held() : nullptr; }
};

/// The change guard in the window, against the fake daemon: On Hold in the sidebar while
/// something is held, its groups and their buttons on the listed token, the open group's rows,
/// Status' card, and Settings' switch, which asks before it turns off, and its limits.
/// Offscreen, with XDG_CONFIG_HOME a temporary directory.
class OnHoldPageTest : public QObject
{
    Q_OBJECT

private:
    QTemporaryDir m_config;

    static void prepare(FakeDaemon &fake)
    {
        // Changes are held only where they are made: two-way.
        fake.account->set({{QStringLiteral("State"), QStringLiteral("signed-in")}, {QStringLiteral("Mode"), QStringLiteral("read-write")}});
        // The whole window is loaded, and would open a sign-in address in the browser.
        fake.account->readWriteFails = QStringLiteral("cannot listen on localhost");
        fake.sync->folder->set({{QStringLiteral("Path"), QStringLiteral("/home/u/OneDrive")},
                                {QStringLiteral("State"), QStringLiteral("ready")},
                                {QStringLiteral("Source"), QStringLiteral("onedrive")}});
        fake.sync->folder->decide("ok", "up-to-date");
    }

    static void click(QQuickItem *button) { QMetaObject::invokeMethod(button, "clicked"); }

    /// Optional captures use the same fake-service states as the regressions, with the
    /// caller's isolated color scheme. Ordinary test runs write no images.
    static void capture(App &app, const QString &name)
    {
        const QString out = qEnvironmentVariable("KONEDRIVE_SHOTS_DIR");
        if (out.isEmpty()) {
            return;
        }
        QVERIFY(QDir().mkpath(out));
        for (const int width : {828, 360}) {
            app.window->resize(width, 648);
            QTest::qWait(250);
            const QString path = QStringLiteral("%1/%2-%3-%4.png").arg(out, name, QString::number(width), qEnvironmentVariable("KONEDRIVE_SHOT_THEME"));
            QVERIFY(app.window->grabWindow().save(path));
            qInfo() << "saved" << path;
        }
    }

private Q_SLOTS:
    void initTestCase()
    {
        QVERIFY(m_config.isValid());
        if (qEnvironmentVariableIsEmpty("KONEDRIVE_SHOTS_DIR")) {
            qputenv("XDG_CONFIG_HOME", QFile::encodeName(m_config.path()));
        }
        KLocalizedString::setApplicationDomain("konedrive");
        QQuickStyle::setStyle(QStringLiteral("org.kde.desktop"));
    }

    /// On Hold is in the sidebar only while something is held, with the count; once its last
    /// change is resolved there, it stays until another page is shown.
    void theSidebarEntry()
    {
        FakeDaemon fake;
        prepare(fake);
        QVERIFY(fake.start());
        App app;
        QVERIFY(app.load());
        QTRY_VERIFY(app.held() && app.held()->serviceAvailable());
        QQuickItem *entry = app.find(QStringLiteral("sidebar-onHold"));
        QVERIFY(entry);
        QTRY_VERIFY(!entry->isVisible());
        // Nothing held: no page to show.
        app.showSection(QStringLiteral("onHold"));
        QTRY_COMPARE(app.pageShown(), QStringLiteral("statusPage"));

        fake.changeGuard->hold(fake::idFor(1), deletions());
        QTRY_VERIFY(entry->isVisible());
        QTRY_COMPARE(entry->property("badge").toInt(), 3);
        app.showSection(QStringLiteral("onHold"));
        QTRY_COMPARE(app.pageShown(), QStringLiteral("onHoldPage"));

        fake.changeGuard->hold(fake::idFor(1), {});
        QTRY_COMPARE(entry->property("badge").toInt(), 0);
        QVERIFY(entry->isVisible());
        QCOMPARE(app.pageShown(), QStringLiteral("onHoldPage"));
        QTRY_VERIFY(app.shown(QStringLiteral("nothingHeldCard")));
        app.showPage(QStringLiteral("status"));
        QTRY_VERIFY(!entry->isVisible());
    }

    /// The groups: one card per held kind, its two buttons on the listed token; Allow All and
    /// Restore All only for several kinds; an open group lists its rows.
    void theGroups()
    {
        FakeDaemon fake;
        prepare(fake);
        fake.changeGuard->held.insert(fake::idFor(1), deletions(12) + rewrites());
        QVERIFY(fake.start());
        App app;
        QVERIFY(app.load());
        QTRY_VERIFY(app.held() && app.held()->count() == 13);
        app.showPage(QStringLiteral("onHold"));
        QTRY_COMPARE(app.pageShown(), QStringLiteral("onHoldPage"));
        QTRY_VERIFY(app.shown(QStringLiteral("heldGroup-removal")));
        QVERIFY(app.shown(QStringLiteral("heldGroup-rewrite")));
        QCOMPARE(app.shown(QStringLiteral("heldHeadline"))->property("text").toString(), QStringLiteral("12 deletions and 1 changed file are on hold"));
        QVERIFY(app.shown(QStringLiteral("allowAllButton")));
        QVERIFY(app.shown(QStringLiteral("restoreAllButton")));

        // Opened: a search (more than 10 rows) and the rows, built by the list.
        QQuickItem *page = app.find(QStringLiteral("onHoldPage"));
        QMetaObject::invokeMethod(page, "toggle", Q_ARG(QVariant, QStringLiteral("removal")), Q_ARG(QVariant, false));
        QTRY_VERIFY(app.shown(QStringLiteral("heldSearch")));
        auto *list = app.find(QStringLiteral("onHoldList"));
        QTRY_COMPARE(list->property("count").toInt(), 12);
        page->setProperty("searchText", QStringLiteral("FILE-1"));
        QTRY_COMPARE(list->property("count").toInt(), 3); // file-1, file-10, file-11

        // A group's Restore acts on what the page listed.
        const QString token = app.held()->token();
        QQuickItem *group = app.shown(QStringLiteral("heldGroup-removal"));
        click(app.shown(QStringLiteral("restoreButton"), group));
        QTRY_VERIFY(fake.changeGuard->calls.contains(QStringLiteral("RestoreHeld:%1:%2:removal").arg(fake::idFor(1), token)));
        QTRY_VERIFY(app.shown(QStringLiteral("heldResult")));
        QCOMPARE(app.shown(QStringLiteral("heldResult"))->property("text").toString(), QStringLiteral("Restored 12 items: they are back in the folder."));
        // One kind left: no Allow All.
        QTRY_VERIFY(!app.shown(QStringLiteral("heldGroup-removal")));
        QVERIFY(!app.shown(QStringLiteral("allowAllButton")));
        QCOMPARE(list->property("count").toInt(), 0);

        // The rest, allowed.
        click(app.shown(QStringLiteral("allowButton"), app.shown(QStringLiteral("heldGroup-rewrite"))));
        QTRY_VERIFY(fake.changeGuard->calls.contains(QStringLiteral("AllowHeld:%1:%2:rewrite").arg(fake::idFor(1), app.held()->token())));
        QTRY_VERIFY(app.shown(QStringLiteral("nothingHeldCard")));
    }

    /// Filename markup stays literal, and daemon error markup is escaped at InlineMessage.
    void untrustedTextStaysLiteral()
    {
        FakeDaemon fake;
        prepare(fake);
        const QString path = QStringLiteral("/home/u/OneDrive/<b>report & notes.txt");
        fake.sync->keptBack = {{QStringLiteral("per-file"), QStringLiteral("local-only"), 1, 20}};
        fake.sync->keptBackFiles.insert(QStringLiteral("local-only"), {{path, QStringLiteral("local-only")}});
        fake.changeGuard->held.insert(fake::idFor(1), deletions());
        QVERIFY(fake.start());
        App app;
        QVERIFY(app.load());
        QTRY_VERIFY(app.held() && app.held()->count() == 3);
        app.showPage(QStringLiteral("issues"));
        QTRY_COMPARE(app.pageShown(), QStringLiteral("issuesPage"));
        QQuickItem *page = app.find(QStringLiteral("issuesPage"));
        QMetaObject::invokeMethod(page, "setOpened", Q_ARG(QVariant, QStringLiteral("local-only")), Q_ARG(QVariant, true));
        QTRY_VERIFY(app.shown(QStringLiteral("localOnlyRow")));
        auto *row = app.shown(QStringLiteral("localOnlyRow"));
        QObject *label = row->property("textItem").value<QObject *>();
        QVERIFY(label);
        QCOMPARE(label->property("text").toString(), path);
        QCOMPARE(label->property("textFormat").toInt(), 0); // Text.PlainText
        fake.changeGuard->refuse = QStringLiteral("<b>failure</b> & retry");
        click(app.shown(QStringLiteral("uploadLocalButton")));
        QTRY_VERIFY(app.shown(QStringLiteral("uploadLocalError")));
        QCOMPARE(app.shown(QStringLiteral("uploadLocalError"))->property("text").toString(), QStringLiteral("&lt;b&gt;failure&lt;/b&gt; &amp; retry"));

        capture(app, QStringLiteral("u4-local-only"));
        app.showPage(QStringLiteral("onHold"));
        QTRY_VERIFY(app.shown(QStringLiteral("heldGroup-removal")));
        click(app.shown(QStringLiteral("restoreButton"), app.shown(QStringLiteral("heldGroup-removal"))));
        QTRY_VERIFY(app.shown(QStringLiteral("heldResult")));
        QCOMPARE(app.shown(QStringLiteral("heldResult"))->property("text").toString(),
                 QStringLiteral("Nothing was restored: &lt;b&gt;failure&lt;/b&gt; &amp; retry"));
        capture(app, QStringLiteral("u4-held-error"));
    }

    /// ListHeld failures keep the known holds visible with an error and a working Retry;
    /// missing interfaces disable decisions, while operational errors keep the service known.
    void listingFailureKeepsHolds()
    {
        FakeDaemon fake;
        prepare(fake);
        fake.changeGuard->held.insert(fake::idFor(1), deletions());
        QVERIFY(fake.start());
        App app;
        QVERIFY(app.load());
        QTRY_VERIFY(app.held() && app.held()->count() == 3);
        app.showPage(QStringLiteral("onHold"));
        QTRY_VERIFY(app.shown(QStringLiteral("heldGroup-removal")));
        fake.changeGuard->listError = QStringLiteral("Could not read <ledger> & state");
        fake.changeGuard->sendHeldChanged();
        QTRY_COMPARE(app.held()->listingError(), fake.changeGuard->listError);
        QVERIFY(app.held()->serviceAvailable());
        QCOMPARE(app.held()->count(), 3);
        QVERIFY(app.shown(QStringLiteral("heldGroup-removal")));
        QVERIFY(!app.shown(QStringLiteral("nothingHeldCard")));
        QTRY_VERIFY(app.shown(QStringLiteral("heldListingError")));
        QCOMPARE(app.shown(QStringLiteral("heldListingError"))->property("text").toString(),
                 QStringLiteral("Could not read &lt;ledger&gt; &amp; state"));
        QVERIFY(!app.shown(QStringLiteral("restoreButton"))->isEnabled());
        capture(app, QStringLiteral("u5-listing-error"));
        auto *retry = app.window->findChild<QObject *>(QStringLiteral("retryHeldListing"));
        QVERIFY(retry);
        QCOMPARE(retry->property("text").toString(), QStringLiteral("Retry"));
        fake.changeGuard->listErrorName = QStringLiteral("org.freedesktop.DBus.Error.UnknownMethod");
        QMetaObject::invokeMethod(retry, "trigger");
        QTRY_VERIFY(!app.held()->serviceAvailable());
        QCOMPARE(app.held()->count(), 3);
        QVERIFY(app.shown(QStringLiteral("heldGroup-removal")));
        QVERIFY(!app.shown(QStringLiteral("nothingHeldCard")));
        fake.changeGuard->listError.clear();
        QMetaObject::invokeMethod(retry, "trigger");
        QTRY_VERIFY(app.held()->listingError().isEmpty());
        QTRY_VERIFY(app.held()->serviceAvailable());
        QCOMPARE(app.held()->count(), 3);
        QVERIFY(!app.shown(QStringLiteral("heldListingError")));
        QVERIFY(app.shown(QStringLiteral("restoreButton"))->isEnabled());
        capture(app, QStringLiteral("u5-retried"));
        // Only a successful empty listing can resolve the cached holds.
        fake.changeGuard->hold(fake::idFor(1), {});
        QTRY_VERIFY(app.shown(QStringLiteral("nothingHeldCard")));
    }

    /// Status: the card of what is held, its head, and Review…, which leads to On Hold.
    void theStatusCard()
    {
        FakeDaemon fake;
        prepare(fake);
        QVERIFY(fake.start());
        App app;
        QVERIFY(app.load());
        QTRY_VERIFY(app.held() && app.held()->serviceAvailable());
        app.showPage(QStringLiteral("status"));
        QQuickItem *card = app.find(QStringLiteral("heldCard"));
        QVERIFY(card);
        QTRY_VERIFY(!card->isVisible());

        fake.changeGuard->hold(fake::idFor(1), deletions() + rewrites());
        QTRY_VERIFY(card->isVisible());
        QTRY_COMPARE(app.find(QStringLiteral("headline"))->property("text").toString(), QStringLiteral("Changes on hold"));
        QCOMPARE(app.find(QStringLiteral("headDetail"))->property("text").toString(), QStringLiteral("Everything else is up to date."));
        QTRY_VERIFY(app.shown(QStringLiteral("heldKindRow"), card));
        QCOMPARE(app.shown(QStringLiteral("heldKindRow"), card)->property("text").toString(), QStringLiteral("3 deletions"));
        click(app.find(QStringLiteral("reviewHeldButton")));
        QTRY_COMPARE(app.pageShown(), QStringLiteral("onHoldPage"));
    }

    /// Hold large changes: on with Change Limits…; turning it off asks first and says what
    /// stays on hold; off, a warning instead of the limits. The limits dialog sends what differs.
    void theSettings()
    {
        FakeDaemon fake;
        prepare(fake);
        fake.changeGuard->held.insert(fake::idFor(1), deletions());
        QVERIFY(fake.start());
        App app;
        QVERIFY(app.load());
        QTRY_VERIFY(app.changeGuard.serviceAvailable());
        QTRY_COMPARE(app.accounts.heldCount(), 3);
        app.showPage(QStringLiteral("settings"));
        auto *holdSwitch = app.find(QStringLiteral("holdSwitch"));
        auto *limits = app.find(QStringLiteral("changeHoldLimitsButton"));
        auto *offLine = app.find(QStringLiteral("holdOffLine"));
        QVERIFY(holdSwitch && limits && offLine);
        QTRY_VERIFY(holdSwitch->isVisible());
        QVERIFY(holdSwitch->property("checked").toBool());
        QCOMPARE(holdSwitch->property("description").toString(), app.changeGuard.description());
        QVERIFY(limits->isVisible());
        QVERIFY(!offLine->isVisible());

        // Off asks first: Cancel keeps it on.
        auto *prompt = app.window->findChild<QObject *>(QStringLiteral("stopHoldingDialog"));
        QVERIFY(prompt);
        holdSwitch->setProperty("checked", false);
        QMetaObject::invokeMethod(holdSwitch, "toggled");
        QTRY_VERIFY(prompt->property("opened").toBool());
        QVERIFY(holdSwitch->property("checked").toBool());
        QVERIFY(prompt->property("subtitle").toString().endsWith(QStringLiteral("The 3 changes on hold now stay on hold until you allow or restore them.")));
        QMetaObject::invokeMethod(prompt, "close");
        QTRY_VERIFY(!prompt->property("opened").toBool());
        QVERIFY(!fake.changeGuard->calls.contains(QStringLiteral("SetEnabled:off")));

        // Turn Off.
        holdSwitch->setProperty("checked", false);
        QMetaObject::invokeMethod(holdSwitch, "toggled");
        QTRY_VERIFY(prompt->property("opened").toBool());
        QObject *turnOff = prompt->findChild<QObject *>(QStringLiteral("turnOffHolding"));
        QVERIFY(turnOff);
        QMetaObject::invokeMethod(turnOff, "trigger");
        QTRY_VERIFY(fake.changeGuard->calls.contains(QStringLiteral("SetEnabled:off")));
        QTRY_VERIFY(!holdSwitch->property("checked").toBool());
        QTRY_VERIFY(offLine->isVisible());
        QCOMPARE(offLine->property("text").toString(), QStringLiteral("Any app can delete or overwrite your whole OneDrive at once."));
        QVERIFY(!limits->isVisible());
        // What is held stays held.
        QCOMPARE(app.accounts.heldCount(), 3);

        // On again: no question.
        holdSwitch->setProperty("checked", true);
        QMetaObject::invokeMethod(holdSwitch, "toggled");
        QTRY_VERIFY(fake.changeGuard->calls.contains(QStringLiteral("SetEnabled:on")));
        QTRY_VERIFY(holdSwitch->property("checked").toBool());

        // The limits: Save only once something differs; what differs is sent.
        auto *dialog = app.window->findChild<QObject *>(QStringLiteral("holdLimitsDialog"));
        QVERIFY(dialog);
        QTRY_VERIFY(limits->isVisible());
        click(limits);
        QTRY_VERIFY(dialog->property("opened").toBool());
        QVERIFY(!dialog->property("differs").toBool());
        auto *removals = app.window->findChild<QQuickItem *>(QStringLiteral("removalsBox"));
        QVERIFY(removals);
        removals->property("contentItem").value<QObject *>()->setProperty("text", QStringLiteral("0"));
        QTRY_VERIFY(!dialog->property("valid").toBool());
        removals->property("contentItem").value<QObject *>()->setProperty("text", QStringLiteral("35"));
        QTRY_VERIFY(dialog->property("valid").toBool() && dialog->property("differs").toBool());
        QMetaObject::invokeMethod(dialog, "save");
        QTRY_VERIFY(fake.changeGuard->calls.contains(QStringLiteral("SetThresholds:removals=35")));
        QTRY_VERIFY(holdSwitch->property("description").toString().startsWith(QStringLiteral("More than 35 deletions")));
    }
};

QTEST_MAIN(OnHoldPageTest)
#include "onholdpagetest.moc"
