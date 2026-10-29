#include "fakedaemon.h"
#include "notifier.h"
#include "qmlregistration.h"

#include <KLocalizedQmlContext>
#include <KLocalizedString>

#include <QLocale>
#include <QQmlApplicationEngine>
#include <QQuickItem>
#include <QQuickStyle>
#include <QQuickWindow>
#include <QTemporaryDir>
#include <QTest>

namespace
{
const QString Copier = QStringLiteral("app:org.example.Copier");
const QString Rg = QStringLiteral("exe:/usr/bin/rg");
const QString Term = QStringLiteral("app:org.example.Term");
constexpr qulonglong GiB = 1ULL << 30;

/// The desktop's entries, for the tests: an app keeps the daemon's name, Copier is "Copier".
GuardApp standIn(const QString &identity, const QString &displayName)
{
    GuardApp app = identity.startsWith(QLatin1String("app:")) ? GuardApp{} : DownloadGuardController::resolve(identity, displayName);
    if (identity.startsWith(QLatin1String("app:"))) {
        app.name = identity == Copier ? QStringLiteral("Copier") : DownloadGuardController::cleanName(displayName);
        app.iconName = QStringLiteral("application-x-executable");
    }
    return app;
}

FakeGuardApp blocked(const QString &identity, const QString &name)
{
    return {identity, name, QStringLiteral("automatic"), true, QStringLiteral("bulk"), 30, 900ULL << 20};
}
}

/// Drops the release check's notification: it never asks GitHub here.
class DroppedNotices : public NotificationSink
{
public:
    void send(const Notice &) override { }
};

/// The app's objects and its window, Main.qml loaded as main() loads it, with the guard's
/// controller its own.
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
    QQmlApplicationEngine engine;
    QQuickWindow *window = nullptr;

    bool load()
    {
        guard.setResolver(standIn);
        registerKonedriveQml(&daemon, &accounts, &current, &autostart, &progress, &places, &trayIcons, &serviceUnit, &releaseCheck, &guard);
        KLocalization::setupLocalizedContext(&engine);
        engine.loadFromModule("org.konedrive.app.window", "Main");
        window = engine.rootObjects().isEmpty() ? nullptr : qobject_cast<QQuickWindow *>(engine.rootObjects().constFirst());
        if (window) {
            window->show();
        }
        return window != nullptr;
    }

    template<typename T = QQuickItem>
    T *find(const char *name) const
    {
        return window->findChild<T *>(QLatin1String(name));
    }

    void showPage(const QString &name) const { QMetaObject::invokeMethod(window, "showPage", Q_ARG(QVariant, name)); }

    QString pageShown() const
    {
        auto *stack = window->property("pageStack").value<QObject *>();
        auto *page = stack ? stack->property("currentItem").value<QObject *>() : nullptr;
        return page ? page->objectName() : QString();
    }
};

/// The download guard in the window, against the fake daemon: Settings' switch, its limits
/// and its list of apps with each row's buttons, the limits dialog, Status' blocked apps, and
/// the previews' warning on OneDrive Settings. Offscreen, with XDG_CONFIG_HOME a temporary
/// directory.
class GuardPagesTest : public QObject
{
    Q_OBJECT

private:
    QTemporaryDir m_config;

    /// The first item under `root`, by the visual tree, called `name`; hidden ones too.
    static QQuickItem *named(QQuickItem *root, const QString &name)
    {
        const auto children = root->childItems();
        for (QQuickItem *item : children) {
            if (item->objectName() == name) {
                return item;
            }
            if (QQuickItem *found = named(item, name)) {
                return found;
            }
        }
        return nullptr;
    }

    /// The rows of the app list under `card`, by the visual tree, in order.
    static QList<QQuickItem *> rows(QQuickItem *card)
    {
        QList<QQuickItem *> found;
        const std::function<void(QQuickItem *)> walk = [&](QQuickItem *item) {
            const auto children = item->childItems();
            for (QQuickItem *child : children) {
                if (child->objectName() == QLatin1String("guardAppRow") && child->isVisible()) {
                    found << child;
                    continue;
                }
                walk(child);
            }
        };
        walk(card);
        std::sort(found.begin(), found.end(), [card](QQuickItem *a, QQuickItem *b) {
            return a->mapToItem(card, QPointF()).y() < b->mapToItem(card, QPointF()).y();
        });
        return found;
    }

    static QString identityOf(const QQuickItem *row) { return row->property("app").toMap().value(QStringLiteral("identity")).toString(); }

    static QQuickItem *rowFor(QQuickItem *card, const QString &identity)
    {
        for (QQuickItem *row : rows(card)) {
            if (identityOf(row) == identity) {
                return row;
            }
        }
        return nullptr;
    }

    /// The buttons of `row` that show, by name.
    static QStringList shownButtons(QQuickItem *row)
    {
        QStringList shown;
        for (const char *name : {"allowSessionButton", "allowAlwaysButton", "moreButton", "removeButton"}) {
            QQuickItem *button = named(row, QLatin1String(name));
            if (button && button->isVisible()) {
                shown << QLatin1String(name);
            }
        }
        return shown;
    }

    /// The fake's account, signed in with a folder, and the guard's apps.
    static void prepare(FakeDaemon &fake, const QList<FakeGuardApp> &apps = {})
    {
        fake.account->set({{QStringLiteral("State"), QStringLiteral("signed-in")}});
        // The whole window is loaded, and would open a sign-in address in the browser.
        fake.account->readWriteFails = QStringLiteral("cannot listen on localhost");
        fake.sync->folder->set({{QStringLiteral("Path"), QStringLiteral("/home/u/OneDrive")},
                                {QStringLiteral("State"), QStringLiteral("ready")},
                                {QStringLiteral("Source"), QStringLiteral("onedrive")}});
        fake.guard->apps = apps;
    }

private Q_SLOTS:
    void initTestCase()
    {
        QVERIFY(m_config.isValid());
        qputenv("XDG_CONFIG_HOME", QFile::encodeName(m_config.path()));
        KLocalizedString::setApplicationDomain("konedrive");
        QQuickStyle::setStyle(QStringLiteral("org.kde.desktop"));
    }

    /// Block bulk downloads: on with Change Limits…; off with its warning, and no list.
    void theSwitch()
    {
        FakeDaemon fake;
        prepare(fake, {blocked(Copier, QStringLiteral("org.example.Copier"))});
        QVERIFY(fake.start());
        App app;
        QVERIFY(app.load());
        QTRY_VERIFY(app.guard.serviceAvailable() && app.guard.listed().size() == 1);
        app.showPage(QStringLiteral("settings"));
        auto *guardSwitch = app.find("guardSwitch");
        auto *limits = app.find("changeLimitsButton");
        auto *offLine = app.find("guardOffLine");
        auto *card = app.find("guardAppsCard");
        QVERIFY(guardSwitch && limits && offLine && card);
        QTRY_VERIFY(guardSwitch->isVisible());
        QVERIFY(guardSwitch->property("checked").toBool());
        QCOMPARE(guardSwitch->property("description").toString(), app.guard.description());
        QVERIFY(limits->isVisible());
        QVERIFY(!offLine->isVisible());
        QTRY_VERIFY(card->isVisible());

        // The user's turn asks the daemon; its answer is what shows.
        guardSwitch->setProperty("checked", false);
        QMetaObject::invokeMethod(guardSwitch, "toggled");
        QTRY_VERIFY(fake.guard->calls.contains(QStringLiteral("SetEnabled:off")));
        QTRY_VERIFY(!guardSwitch->property("checked").toBool());
        QVERIFY(!limits->isVisible());
        QVERIFY(offLine->isVisible());
        QCOMPARE(offLine->property("text").toString(), QStringLiteral("Any app can download your whole OneDrive."));
        // Off, the rules stay, out of sight.
        QVERIFY(!card->isVisible());

        // Changed elsewhere (konedrivectl), the page follows.
        fake.guard->set({{QStringLiteral("Enabled"), true}});
        QTRY_VERIFY(guardSwitch->property("checked").toBool());
        QTRY_VERIFY(card->isVisible());

        // A refusal shows in the page's message.
        fake.guard->refuse = QStringLiteral("config.toml cannot be written");
        guardSwitch->setProperty("checked", false);
        QMetaObject::invokeMethod(guardSwitch, "toggled");
        auto *error = app.find("settingsError");
        QVERIFY(error);
        QTRY_VERIFY(error->isVisible());
        QCOMPARE(error->property("text").toString(), QStringLiteral("config.toml cannot be written"));
        QVERIFY(guardSwitch->property("checked").toBool());
    }

    /// The list: only with an app in it; each row's state and its buttons, which call the
    /// daemon.
    void theListAndItsButtons()
    {
        FakeDaemon fake;
        prepare(fake);
        QVERIFY(fake.start());
        App app;
        QVERIFY(app.load());
        QTRY_VERIFY(app.guard.serviceAvailable());
        app.showPage(QStringLiteral("settings"));
        auto *card = app.find("guardAppsCard");
        auto *header = app.find("guardAppsHeader");
        QVERIFY(card && header);
        QTRY_VERIFY(app.find("guardSwitch")->isVisible());
        QVERIFY(!card->isVisible());
        QVERIFY(!header->isVisible());

        fake.guard->apps = {blocked(Copier, QStringLiteral("org.example.Copier")),
                            {Rg, QStringLiteral("rg"), QStringLiteral("allowed-session")},
                            {Term, QStringLiteral("org.example.Term"), QStringLiteral("allowed-always")},
                            {DownloadGuardController::Unidentified, QStringLiteral("Unknown process"), QStringLiteral("automatic"), true, QStringLiteral("unidentified")},
                            {QStringLiteral("exe:/usr/bin/cat"), QStringLiteral("cat")}};
        fake.guard->sendAppsChanged();
        QTRY_VERIFY(card->isVisible());
        QVERIFY(header->isVisible());
        QTRY_COMPARE(rows(card).size(), 4);
        const QList<QQuickItem *> shown = rows(card);
        QStringList names;
        QStringList states;
        for (QQuickItem *row : shown) {
            names << named(row, QStringLiteral("guardAppName"))->property("text").toString();
            states << named(row, QStringLiteral("guardAppState"))->property("text").toString();
        }
        QCOMPARE(names, QStringList({QStringLiteral("Copier"), QStringLiteral("Unknown program"), QStringLiteral("rg"), QStringLiteral("org.example.Term")}));
        QCOMPARE(states,
                 QStringList({QStringLiteral("Blocked after 30 files"),
                              QStringLiteral("Blocked: it couldn't be identified"),
                              QStringLiteral("Allowed for this session"),
                              QStringLiteral("Always allowed")}));
        QCOMPARE(shownButtons(shown.at(0)), QStringList({QStringLiteral("allowSessionButton"), QStringLiteral("allowAlwaysButton"), QStringLiteral("moreButton")}));
        QCOMPARE(shownButtons(shown.at(1)), QStringList());
        QCOMPARE(shownButtons(shown.at(2)), QStringList(QStringLiteral("removeButton")));
        QCOMPARE(shownButtons(shown.at(3)), QStringList(QStringLiteral("removeButton")));

        auto &calls = fake.guard->calls;
        const auto blockAgain = [&] {
            fake.guard->apps.replace(0, blocked(Copier, QStringLiteral("org.example.Copier")));
            fake.guard->sendAppsChanged();
        };
        QMetaObject::invokeMethod(named(rowFor(card, Copier), QStringLiteral("allowSessionButton")), "clicked");
        QTRY_VERIFY(calls.contains(QStringLiteral("AllowApp:%1:session").arg(Copier)));
        QTRY_COMPARE(shownButtons(rowFor(card, Copier)), QStringList(QStringLiteral("removeButton")));

        blockAgain();
        QTRY_COMPARE(shownButtons(rowFor(card, Copier)).size(), 3);
        QMetaObject::invokeMethod(named(rowFor(card, Copier), QStringLiteral("allowAlwaysButton")), "clicked");
        QTRY_VERIFY(calls.contains(QStringLiteral("AllowApp:%1:always").arg(Copier)));

        blockAgain();
        QTRY_COMPARE(shownButtons(rowFor(card, Copier)).size(), 3);
        auto *alwaysBlock = rowFor(card, Copier)->findChild<QObject *>(QStringLiteral("alwaysBlockItem"));
        QVERIFY(alwaysBlock);
        QMetaObject::invokeMethod(alwaysBlock, "triggered");
        QTRY_VERIFY(calls.contains(QStringLiteral("BlockApp:") + Copier));
        QTRY_COMPARE(named(rowFor(card, Copier), QStringLiteral("guardAppState"))->property("text").toString(), QStringLiteral("Always blocked"));

        blockAgain();
        QTRY_COMPARE(shownButtons(rowFor(card, Copier)).size(), 3);
        fake.guard->freedFiles = 2;
        fake.guard->freedBytes = 4ULL << 20;
        auto *freeUp = rowFor(card, Copier)->findChild<QObject *>(QStringLiteral("freeUpItem"));
        QVERIFY(freeUp);
        QMetaObject::invokeMethod(freeUp, "triggered");
        QTRY_VERIFY(calls.contains(QStringLiteral("FreeUpAppDownloads:") + Copier));
        auto *message = app.find("guardFreeUpMessage");
        QVERIFY(message);
        QTRY_VERIFY(message->isVisible());
        QCOMPARE(message->property("text").toString(), QStringLiteral("Freed 2 files (%1).").arg(QLocale().formattedDataSize(4LL << 20)));

        QMetaObject::invokeMethod(named(rowFor(card, Rg), QStringLiteral("removeButton")), "clicked");
        QTRY_VERIFY(calls.contains(QStringLiteral("ResetApp:") + Rg));
        QTRY_VERIFY(!rowFor(card, Rg));
    }

    /// The limits dialog: Save only for numbers in range that differ from the daemon's; all
    /// three sent together; what was typed and not saved is gone when it opens again.
    void theLimitsDialog()
    {
        FakeDaemon fake;
        prepare(fake);
        QVERIFY(fake.start());
        App app;
        QVERIFY(app.load());
        QTRY_VERIFY(app.guard.serviceAvailable());
        app.showPage(QStringLiteral("settings"));
        auto *dialog = app.find<QObject>("limitsDialog");
        auto *save = app.find<QObject>("saveLimits");
        auto *files = app.find("maxFilesBox");
        auto *gib = app.find("maxGiBBox");
        auto *minutes = app.find("windowMinutesBox");
        QVERIFY(dialog && save && files && gib && minutes);
        const auto typeInto = [](QQuickItem *box, const QString &text) {
            box->property("contentItem").value<QObject *>()->setProperty("text", text);
        };
        const auto boxText = [](QQuickItem *box) {
            return box->property("contentItem").value<QObject *>()->property("text").toString();
        };

        QTRY_VERIFY(app.find("changeLimitsButton")->isVisible());
        QMetaObject::invokeMethod(app.find("changeLimitsButton"), "clicked");
        QTRY_VERIFY(dialog->property("visible").toBool());
        QCOMPARE(files->property("value").toInt(), 30);
        QCOMPARE(gib->property("value").toInt(), 2);
        QCOMPARE(minutes->property("value").toInt(), 10);
        QVERIFY(!save->property("enabled").toBool());

        typeInto(files, QStringLiteral("50"));
        QVERIFY(save->property("enabled").toBool());
        for (const QString &bad : {QStringLiteral("0"), QStringLiteral("10001"), QStringLiteral("abc"), QString()}) {
            typeInto(files, bad);
            QVERIFY2(!save->property("enabled").toBool(), qPrintable(bad));
        }
        typeInto(files, QStringLiteral("30"));
        QVERIFY(!save->property("enabled").toBool());
        typeInto(gib, QStringLiteral("1001"));
        QVERIFY(!save->property("enabled").toBool());

        // Typed and closed without Save: nothing sent, and gone next time.
        typeInto(gib, QStringLiteral("5"));
        QVERIFY(save->property("enabled").toBool());
        QMetaObject::invokeMethod(dialog, "close");
        QTRY_VERIFY(!dialog->property("visible").toBool());
        QMetaObject::invokeMethod(app.find("changeLimitsButton"), "clicked");
        QTRY_VERIFY(dialog->property("visible").toBool());
        QCOMPARE(boxText(gib), QStringLiteral("2"));
        QVERIFY(fake.guard->calls.filter(QStringLiteral("SetThresholds")).isEmpty());

        typeInto(files, QStringLiteral("50"));
        typeInto(gib, QStringLiteral("3"));
        QMetaObject::invokeMethod(save, "trigger");
        QTRY_VERIFY(fake.guard->calls.contains(QStringLiteral("SetThresholds:50:%1:600").arg(3 * GiB)));
        QTRY_VERIFY(!dialog->property("visible").toBool());
        QTRY_COMPARE(app.guard.maxFiles(), 50u);
        QTRY_VERIFY(app.find("guardSwitch")->property("description").toString().contains(QStringLiteral("more than 50 files or ")));

        // Escape closes it, as Cancel does.
        QMetaObject::invokeMethod(app.find("changeLimitsButton"), "clicked");
        QTRY_VERIFY(dialog->property("visible").toBool());
        QCOMPARE(files->property("value").toInt(), 50);
        app.window->requestActivate();
        QVERIFY(QTest::qWaitForWindowActive(app.window));
        QTest::keyClick(app.window, Qt::Key_Escape);
        QTRY_VERIFY(!dialog->property("visible").toBool());
    }

    void savingOneLimitPreservesUntouchedCliValues()
    {
        FakeDaemon fake;
        prepare(fake);
        fake.guard->set({{QStringLiteral("MaxFiles"), 10001u},
                         {QStringLiteral("MaxBytes"), QVariant::fromValue(1001 * GiB + 123)},
                         {QStringLiteral("WindowSeconds"), 1441u * 60 + 7}});
        QVERIFY(fake.start());
        App app;
        QVERIFY(app.load());
        QTRY_VERIFY(app.guard.serviceAvailable());
        app.showPage(QStringLiteral("settings"));
        auto *dialog = app.find<QObject>("limitsDialog");
        auto *save = app.find<QObject>("saveLimits");
        QVERIFY(dialog && save);
        QTRY_VERIFY(app.find("changeLimitsButton")->isVisible());
        QMetaObject::invokeMethod(app.find("changeLimitsButton"), "clicked");
        QTRY_VERIFY(dialog->property("visible").toBool());
        QVERIFY(!save->property("enabled").toBool());

        // Change just files; the exact CLI byte and second values must survive.
        app.find("maxFilesBox")->property("contentItem").value<QObject *>()->setProperty("text", QStringLiteral("50"));
        QVERIFY(save->property("enabled").toBool());
        QMetaObject::invokeMethod(save, "trigger");
        QTRY_VERIFY(fake.guard->calls.contains(QStringLiteral("SetThresholds:50:%1:%2").arg(1001 * GiB + 123).arg(1441u * 60 + 7)));

        // Reopen and change just GiB; an out-of-range file count must also survive.
        fake.guard->set({{QStringLiteral("MaxFiles"), 10001u}});
        QTRY_COMPARE(app.guard.maxFiles(), 10001u);
        QTRY_VERIFY(!dialog->property("visible").toBool());
        QMetaObject::invokeMethod(app.find("changeLimitsButton"), "clicked");
        QTRY_VERIFY(dialog->property("visible").toBool());
        app.find("maxGiBBox")->property("contentItem").value<QObject *>()->setProperty("text", QStringLiteral("3"));
        QMetaObject::invokeMethod(save, "trigger");
        QTRY_VERIFY(fake.guard->calls.contains(QStringLiteral("SetThresholds:10001:%1:%2").arg(3 * GiB).arg(1441u * 60 + 7)));
    }

    /// Status: the apps blocked that wait for the user, after their notification has gone,
    /// with the same buttons; never the rules.
    void statusShowsTheBlockedApps()
    {
        FakeDaemon fake;
        prepare(fake,
                {blocked(Copier, QStringLiteral("org.example.Copier")),
                 {Rg, QStringLiteral("rg"), QStringLiteral("allowed-session")},
                 {DownloadGuardController::Unidentified, QStringLiteral("Unknown process"), QStringLiteral("automatic"), true, QStringLiteral("unidentified")}});
        QVERIFY(fake.start());
        App app;
        QVERIFY(app.load());
        QTRY_VERIFY(app.guard.listed().size() == 3);
        app.showPage(QStringLiteral("status"));
        QTRY_COMPARE(app.pageShown(), QStringLiteral("statusPage"));
        auto *card = app.find("blockedAppsCard");
        QVERIFY(card);
        QTRY_VERIFY(card->isVisible());
        QTRY_COMPARE(rows(card).size(), 2);
        QCOMPARE(identityOf(rows(card).at(0)), Copier);
        QCOMPARE(identityOf(rows(card).at(1)), DownloadGuardController::Unidentified);
        QMetaObject::invokeMethod(named(rows(card).at(0), QStringLiteral("allowAlwaysButton")), "clicked");
        QTRY_VERIFY(fake.guard->calls.contains(QStringLiteral("AllowApp:%1:always").arg(Copier)));
        QTRY_COMPARE(rows(card).size(), 1);

        // Off, nothing is blocked for want of the user.
        fake.guard->set({{QStringLiteral("Enabled"), false}});
        QTRY_VERIFY(!card->isVisible());
    }

    /// A terminal blocked: Status offers Allow for This Session and More, never Always Allow,
    /// which would let every program run in it download freely; Settings still offers it.
    void aTerminalIsAlwaysAllowedOnlyFromSettings()
    {
        const QString konsole = QStringLiteral("app:org.kde.konsole");
        FakeDaemon fake;
        prepare(fake, {blocked(konsole, QStringLiteral("Konsole"))});
        QVERIFY(fake.start());
        App app;
        QVERIFY(app.load());
        QTRY_VERIFY(app.guard.listed().size() == 1);
        app.showPage(QStringLiteral("status"));
        QTRY_COMPARE(app.pageShown(), QStringLiteral("statusPage"));
        auto *card = app.find("blockedAppsCard");
        QVERIFY(card);
        QTRY_VERIFY(card->isVisible());
        QTRY_COMPARE(rows(card).size(), 1);
        QCOMPARE(identityOf(rows(card).constFirst()), konsole);
        QCOMPARE(shownButtons(rows(card).constFirst()), QStringList({QStringLiteral("allowSessionButton"), QStringLiteral("moreButton")}));

        app.showPage(QStringLiteral("settings"));
        QTRY_COMPARE(app.pageShown(), QStringLiteral("settingsPage"));
        auto *list = app.find("guardAppsCard");
        QVERIFY(list);
        QTRY_COMPARE(rows(list).size(), 1);
        QCOMPARE(shownButtons(rows(list).constFirst()),
                 QStringList({QStringLiteral("allowSessionButton"), QStringLiteral("allowAlwaysButton"), QStringLiteral("moreButton")}));
        QMetaObject::invokeMethod(named(rows(list).constFirst(), QStringLiteral("allowAlwaysButton")), "clicked");
        QTRY_VERIFY(fake.guard->calls.contains(QStringLiteral("AllowApp:%1:always").arg(konsole)));
    }

    /// The previews' warning on OneDrive Settings: only while the guard is off, since with it
    /// on Dolphin's thumbnailer is refused rather than downloading.
    void previewsWarnOnlyWithoutTheGuard()
    {
        FakeDaemon fake;
        prepare(fake);
        fake.sync->folder->set({{QStringLiteral("Thumbnails"), false}});
        QVERIFY(fake.start());
        App app;
        QVERIFY(app.load());
        QTRY_COMPARE(app.accounts.count(), 1);
        QTRY_VERIFY(app.accounts.at(0)->sync()->serviceAvailable() && app.guard.serviceAvailable());
        app.showPage(QStringLiteral("oneDriveSettings"));
        auto *thumbnails = app.find("thumbnailsSwitch");
        auto *offLine = app.find("thumbnailsOffLine");
        QVERIFY(thumbnails && offLine);
        QTRY_VERIFY(thumbnails->isVisible());
        QVERIFY(!thumbnails->property("checked").toBool());
        QVERIFY(!offLine->isVisible());
        fake.guard->set({{QStringLiteral("Enabled"), false}});
        QTRY_VERIFY(offLine->isVisible());
        fake.guard->set({{QStringLiteral("Enabled"), true}});
        QTRY_VERIFY(!offLine->isVisible());
    }

    /// A click on the notification opens Status at its blocked apps, which have the same
    /// buttons; with none blocked any more, Settings, brought to its list once the page has
    /// settled. A name is shown as plain text.
    void theNotificationLeadsToTheBlockedApps()
    {
        FakeDaemon fake;
        prepare(fake, {blocked(QStringLiteral("exe:/tmp/x"), QStringLiteral("<b>Evil</b>"))});
        QVERIFY(fake.start());
        App app;
        QVERIFY(app.load());
        QTRY_VERIFY(app.guard.listed().size() == 1);
        QTRY_COMPARE(app.pageShown(), QStringLiteral("statusPage"));
        app.showPage(QStringLiteral("settings"));
        QTRY_COMPARE(app.pageShown(), QStringLiteral("settingsPage"));
        QMetaObject::invokeMethod(app.window, "showSection", Q_ARG(QVariant, sectionName(WindowSection::GuardApps)));
        QTRY_COMPARE(app.pageShown(), QStringLiteral("statusPage"));
        auto *card = app.find("blockedAppsCard");
        QTRY_VERIFY(card && card->isVisible());
        QTRY_COMPARE(rows(card).size(), 1);
        QQuickItem *name = named(rows(card).constFirst(), QStringLiteral("guardAppName"));
        QCOMPARE(name->property("text").toString(), QStringLiteral("<b>Evil</b>"));
        QCOMPARE(name->property("textFormat").toInt(), 0); // Text.PlainText
        QCOMPARE(shownButtons(rows(card).constFirst()),
                 QStringList({QStringLiteral("allowSessionButton"), QStringLiteral("allowAlwaysButton"), QStringLiteral("moreButton")}));

        // Allowed meanwhile: nothing waits on Status, and Settings has the app's rule.
        fake.guard->apps = {{QStringLiteral("exe:/tmp/x"), QStringLiteral("<b>Evil</b>"), QStringLiteral("allowed-session")}};
        fake.guard->sendAppsChanged();
        QTRY_VERIFY(!card->isVisible());
        app.window->resize(828, 648);
        QMetaObject::invokeMethod(app.window, "showSection", Q_ARG(QVariant, sectionName(WindowSection::GuardApps)));
        QTRY_COMPARE(app.pageShown(), QStringLiteral("settingsPage"));
        auto *list = app.find("guardAppsCard");
        QTRY_COMPARE(rows(list).size(), 1);
        auto *flickable = app.find<QObject>("settingsPage")->property("flickable").value<QQuickItem *>();
        auto *header = app.find("guardAppsHeader");
        QVERIFY(flickable && header);
        const auto wanted = [&] {
            const qreal y = header->mapToItem(flickable->property("contentItem").value<QQuickItem *>(), QPointF()).y();
            return qRound(std::min(y, flickable->property("contentHeight").toReal() - flickable->height()));
        };
        QTRY_VERIFY(wanted() > 0);
        QTRY_COMPARE(qRound(flickable->property("contentY").toReal()), wanted());
    }
};

QTEST_MAIN(GuardPagesTest)

#include "guardpagestest.moc"
