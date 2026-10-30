#include "downloadguardcontroller.h"
#include "fakedaemon.h"
#include "notifier.h"

#include <KLocalizedString>

#include <QDir>
#include <QFile>
#include <QSignalSpy>
#include <QTest>

/// Plasma's notification service on the private test bus. KNotification must actually
/// call Notify: a RecordingSink alone cannot prove that the desktop receives a popup.
class FakeNotifications : public QObject
{
    Q_OBJECT
    Q_CLASSINFO("D-Bus Interface", "org.freedesktop.Notifications")
public:
    QStringList titles;
    QStringList bodies;

public Q_SLOTS:
    QStringList GetCapabilities() { return {QStringLiteral("actions"), QStringLiteral("body"), QStringLiteral("body-markup")}; }
    QString GetServerInformation(QString &vendor, QString &version, QString &spec)
    {
        vendor = QStringLiteral("KOneDrive test");
        version = QStringLiteral("1");
        spec = QStringLiteral("1.2");
        return QStringLiteral("Test notifications");
    }
    uint Notify(const QString &, uint, const QString &, const QString &title, const QString &body, const QStringList &, const QVariantMap &, int)
    {
        titles << title;
        bodies << body;
        return uint(titles.size());
    }
    void CloseNotification(uint) {}
};

class NotificationDeliveryTest : public QObject
{
    Q_OBJECT
private Q_SLOTS:
    void aConnectedBulkBlockReachesTheDesktop()
    {
        KLocalizedString::setApplicationDomain("konedrive");
        // Use this checkout's event config, not an installed version's. Nothing is installed.
        const QString events = qEnvironmentVariable("XDG_DATA_HOME") + QStringLiteral("/knotifications6");
        QVERIFY(QDir().mkpath(events));
        const QString config = events + QStringLiteral("/konedrive.notifyrc");
        if (!QFile::exists(config)) {
            QVERIFY(QFile::copy(QStringLiteral(KONEDRIVE_NOTIFYRC), config));
        }
        FakeNotifications desktop;
        auto bus = fake::bus();
        QVERIFY(bus.registerService(QStringLiteral("org.freedesktop.Notifications")));
        QVERIFY(bus.registerObject(QStringLiteral("/org/freedesktop/Notifications"), &desktop, QDBusConnection::ExportAllSlots));
        FakeDaemon daemon;
        QVERIFY(daemon.start());
        DownloadGuardController guard;
        KNotificationSink sink;
        GuardNotifier notifier(&guard, &sink);
        QTRY_VERIFY(guard.serviceAvailable());
        daemon.guard->block(QStringLiteral("app:org.kde.gwenview"), QStringLiteral("Gwenview"), 30, 4758024);
        QTRY_COMPARE(desktop.titles.size(), 1);
        QCOMPARE(desktop.titles.first(), QStringLiteral("Too many downloads by Gwenview"));
        QVERIFY(desktop.bodies.first().contains(QStringLiteral("30 files")));
        // A block the signal missed must reach KNotification through ListApps too.
        daemon.guard->apps << FakeGuardApp{QStringLiteral("exe:/usr/bin/rg"), QStringLiteral("rg"), QStringLiteral("automatic"), true, QStringLiteral("bulk"), 30, 0};
        daemon.guard->sendAppsChanged();
        QTRY_COMPARE(desktop.titles.size(), 2);
        QCOMPARE(desktop.titles.last(), QStringLiteral("Too many downloads by rg"));
        daemon.guard->sendAppsChanged();
        QTest::qWait(100);
        QCOMPARE(desktop.titles.size(), 2);
        // A block recovered while disabled is delivered once when enforcement resumes.
        daemon.guard->set({{QStringLiteral("Enabled"), false}});
        QTRY_VERIFY(!guard.enabled());
        const QString agent = QStringLiteral("exe:/opt/test/agent");
        daemon.guard->apps << FakeGuardApp{agent, QStringLiteral("agent"), QStringLiteral("automatic"), true, QStringLiteral("bulk"), 30, 0, false, 7};
        daemon.guard->sendAppsChanged();
        QTRY_VERIFY(guard.app(agent) && guard.app(agent)->blocked);
        QCOMPARE(desktop.titles.size(), 2);
        daemon.guard->set({{QStringLiteral("Enabled"), true}});
        QTRY_COMPARE(desktop.titles.size(), 3);
        QCOMPARE(desktop.titles.last(), QStringLiteral("Too many downloads by agent"));
        daemon.guard->sendBlocked(agent, QStringLiteral("agent"), 30, 0, QStringLiteral("bulk"), false, 7);
        QTest::qWait(100);
        QCOMPARE(desktop.titles.size(), 3);
        daemon.stop();
        bus.unregisterObject(QStringLiteral("/org/freedesktop/Notifications"));
        bus.unregisterService(QStringLiteral("org.freedesktop.Notifications"));
    }
};

QTEST_MAIN(NotificationDeliveryTest)
#include "notificationdeliverytest.moc"
