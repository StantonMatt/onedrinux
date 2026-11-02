// Real xattrs, a real killable child, and KPageWidget's page model. The
// controller is also tested apart from KIO's other installed plugins so
// local test files do not cause Baloo to add a competing native page.
#include "testsupport.h"
#include "detailstab.h"
#include "filestate.h"
#include "detailsrows.h"
#include "mediafacts.h"
#include <KConfig>
#include <KConfigGroup>
#include <KFileMetaData/SimpleExtractionResult>
#include <KLocalizedString>
#include <KPageWidget>
#include <KPluginMetaData>
#include <KPropertiesDialog>
#include <QElapsedTimer>
#include <QLabel>
#include <QJsonArray>
#include <QImage>
#include <QRegularExpression>
#include <QSignalSpy>
#include <sys/stat.h>
#include <memory>
using namespace testsupport;
using namespace KFileMetaData;
using namespace konedrive;

namespace {
const QByteArray Photo(R"({"v":1,"kind":"photo","width":4080,"height":3072,"orientation":1,"cameraMake":"Google","cameraModel":"Pixel 8 Pro","fNumber":1.7,"exposureNumerator":1,"exposureDenominator":1052,"iso":41,"focalLength":6.9,"takenDateTime":"2024-03-16T14:35:00Z"})");
KFileItemList selection(const QString &path)
{
    return {KFileItem(url(path), QStringLiteral("application/octet-stream"), S_IFREG)};
}
QWidget *page(KPropertiesDialog &dialog) { return dialog.findChild<QWidget *>(QStringLiteral("konedrive_details")); }
QLabel *value(KPropertiesDialog &dialog, const char *key) { return dialog.findChild<QLabel *>(QStringLiteral("V_") + QLatin1String(key)); }
QString detailsTitle() { return i18ndc("baloowidgets5", "Tab page with file meta data", "&Details"); }
}

class DetailsTabTest : public QObject
{
    Q_OBJECT
    QTemporaryDir m_logDir;
    QString m_log;
    QString helperLog() const
    {
        QFile log(m_log);
        return log.open(QIODevice::ReadOnly) ? QString::fromUtf8(log.readAll()) : QString();
    }
    DetailsTab *tab(KPropertiesDialog &dialog, const KFileItemList &items, int timeout = 2000)
    {
        return new DetailsTab(&dialog, items, QStringLiteral(KONEDRIVE_DETAILS_PROBE), QStringLiteral(KONEDRIVE_DETAILS_FIXTURE), timeout);
    }
private Q_SLOTS:
    void init()
    {
        QVERIFY(m_logDir.isValid());
        m_log = m_logDir.filePath(QStringLiteral("child.log"));
        QFile::remove(m_log);
        qputenv("KONEDRIVE_TEST_HELPER_LOG", QFile::encodeName(m_log));
        KConfig config(QStringLiteral("baloofileinformationrc"), KConfig::NoGlobals);
        config.deleteGroup(QStringLiteral("Show"));
        config.sync();
    }
    void cleanup() { QFile::remove(m_log); }

    void pluginRegistration()
    {
        const KPluginMetaData plugin(QStringLiteral(KONEDRIVE_DETAILS_PLUGIN));
        QVERIFY(plugin.isValid());
        QCOMPARE(plugin.rawData().value(QStringLiteral("X-KDE-Protocols")).toArray().first().toString(), QStringLiteral("file"));
    }

    void cloudPhotoAddsDetails()
    {
        Tree tree;
        QVERIFY(tree.root(QStringLiteral("cloud")));
        QVERIFY(tree.file(QStringLiteral("cloud/photo.jpg"), "online-only"));
        const QString path = tree.path(QStringLiteral("cloud/photo.jpg"));
        QVERIFY(setAttribute(path, "user.konedrive.media", Photo));
        KPropertiesDialog dialog(QStringLiteral("Properties"));
        auto general = dialog.addPage(new QWidget, QStringLiteral("General"));
        dialog.addPage(new QWidget, QStringLiteral("Checksums"));
        OpenWatch watch({tree.path(QStringLiteral("cloud"))});
        QVERIFY(watch.isValid());
        QSignalSpy settled(tab(dialog, selection(path)), &DetailsTab::settled);
        QTRY_COMPARE(settled.size(), 1);
        QVERIFY(page(dialog));
        QCOMPARE(dialog.currentPage(), general);
        auto pages = dialog.findChild<KPageWidget *>();
        auto model = qobject_cast<KPageWidgetModel *>(pages->model());
        QCOMPARE(model->item(model->index(model->rowCount() - 1, 0))->widget(), page(dialog));
        QCOMPARE(page(dialog)->findChildren<QLabel *>(QRegularExpression(QStringLiteral("^L_"))).size(), 9);
        QCOMPARE(value(dialog, "model")->text(), QStringLiteral("Pixel 8 Pro"));
        QCOMPARE(value(dialog, "dimensions")->text(), QStringLiteral("4,080 × 3,072"));
        QVERIFY(helperLog().isEmpty());
        QCOMPARE(watch.take(), QStringList());
        QFile canary(path);
        QVERIFY(canary.open(QIODevice::ReadOnly));
        QVERIFY(!watch.take().isEmpty()); // positive control
    }

    void authoritativeFacts_data()
    {
        QTest::addColumn<QByteArray>("state");
        for (const auto state : {"online-only", "hydrating", "hydrated", "failed", "future", ""})
            QTest::newRow(*state ? state : "missing") << QByteArray(state);
    }
    void authoritativeFacts()
    {
        QFETCH(QByteArray, state);
        Tree tree;
        QVERIFY(tree.root(QStringLiteral("cloud")));
        QVERIFY(tree.file(QStringLiteral("cloud/photo.jpg"), state));
        const auto path = tree.path(QStringLiteral("cloud/photo.jpg"));
        QVERIFY(setAttribute(path, "user.konedrive.media", Photo));
        KPropertiesDialog dialog(QStringLiteral("Properties"));
        QSignalSpy settled(tab(dialog, selection(path)), &DetailsTab::settled);
        QTRY_COMPARE(settled.size(), 1);
        QVERIFY(page(dialog));
        QVERIFY(helperLog().isEmpty());
    }

    void noFactsMustNotExtract_data()
    {
        QTest::addColumn<QByteArray>("state");
        for (const auto state : {"online-only", "hydrating", "failed", "future", "", "a-state-longer-than-the-probe-buffer-size"})
            QTest::newRow(*state ? state : "present-empty") << QByteArray(state);
    }
    void noFactsMustNotExtract()
    {
        QFETCH(QByteArray, state);
        Tree tree;
        QVERIFY(tree.root(QStringLiteral("cloud")));
        QVERIFY(tree.file(QStringLiteral("cloud/photo.jpg"), state));
        const auto path = tree.path(QStringLiteral("cloud/photo.jpg"));
        QVERIFY(setState(path, state)); // Empty is present, not ENODATA.
        for (const auto &payload : {QByteArray(), QByteArray("{"), QByteArray(R"({"v":2,"kind":"photo"})")}) {
            QVERIFY(setAttribute(path, "user.konedrive.media", payload));
            KPropertiesDialog dialog(QStringLiteral("Properties"));
            QSignalSpy settled(tab(dialog, selection(path)), &DetailsTab::settled);
            QTRY_COMPARE(settled.size(), 1);
            QVERIFY(!page(dialog));
        }
        QVERIFY(helperLog().isEmpty());
    }

    void unreadableStateMustNotExtract()
    {
        Tree tree;
        QVERIFY(tree.root(QStringLiteral("cloud")));
        QVERIFY(tree.file(QStringLiteral("cloud/photo.jpg"), "hydrated"));
        const auto path = tree.path(QStringLiteral("cloud/photo.jpg"));
        QVERIFY(QFile::setPermissions(path, {}));
        const auto native = QFile::encodeName(path);
        QCOMPARE(::lgetxattr(native.constData(), StateAttribute, nullptr, 0), -1);
        QCOMPARE(errno, EACCES);
        KPropertiesDialog dialog(QStringLiteral("Properties"));
        QSignalSpy settled(tab(dialog, selection(path)), &DetailsTab::settled);
        QTRY_COMPARE(settled.size(), 1);
        QVERIFY(!page(dialog));
        QVERIFY(helperLog().isEmpty());
    }

    void hydratedUsesOwnValuesWithoutOpeningInHost()
    {
        Tree tree;
        QVERIFY(tree.root(QStringLiteral("cloud")));
        QVERIFY(tree.file(QStringLiteral("cloud/local.jpg"), "hydrated"));
        const auto path = tree.path(QStringLiteral("cloud/local.jpg"));
        QVERIFY(setAttribute(path, "user.konedrive.media", "{")); // invalid facts permit hydrated fallback
        OpenWatch watch({tree.path(QStringLiteral("cloud"))});
        QVERIFY(watch.isValid());
        KPropertiesDialog dialog(QStringLiteral("Properties"));
        QSignalSpy settled(tab(dialog, selection(path)), &DetailsTab::settled);
        QTRY_COMPARE(settled.size(), 1);
        QVERIFY(page(dialog));
        QCOMPARE(value(dialog, "model")->text(), QStringLiteral("Current EXIF"));
        QCOMPARE(value(dialog, "artist")->text(), QStringLiteral("Local author"));
        QCOMPARE(value(dialog, "manufacturer")->textFormat(), Qt::PlainText);
        QCOMPARE(value(dialog, "manufacturer")->text(), QStringLiteral("<b>literal</b>"));
        QVERIFY(!value(dialog, "rating"));
        QVERIFY(!value(dialog, "comment"));
        QCOMPARE(helperLog().count(QLatin1Char('\n')), 1);
        QVERIFY(!helperLog().startsWith(QString::number(::getpid()) + QLatin1Char(' ')));
        QCOMPARE(watch.take(), QStringList()); // helper fixture does not open content either
    }

    void nativeHelperReadsHydratedPng_data()
    {
        QTest::addColumn<QByteArray>("state");
        QTest::newRow("hydrated") << QByteArray("hydrated");
        QTest::newRow("local-only-no-state") << QByteArray();
    }
    void nativeHelperReadsHydratedPng()
    {
        QFETCH(QByteArray, state);
        const QString helper = DetailsTab::extractorExecutable();
        if (helper.isEmpty()) QSKIP("Baloo's native helper is not installed");
        Tree tree;
        QVERIFY(tree.root(QStringLiteral("cloud")));
        const auto path = tree.path(QStringLiteral("cloud/local.png"));
        QImage image(80, 50, QImage::Format_RGB32);
        image.fill(Qt::red);
        QVERIFY(image.save(path));
        if (!state.isEmpty()) QVERIFY(setState(path, state));
        KPropertiesDialog dialog(QStringLiteral("Properties"));
        auto worker = new DetailsTab(&dialog, selection(path), QStringLiteral(KONEDRIVE_DETAILS_PROBE), helper);
        QSignalSpy settled(worker, &DetailsTab::settled);
        QTRY_COMPARE(settled.size(), 1);
        QVERIFY(page(dialog));
        QVERIFY(value(dialog, "dimensions"));
        QCOMPARE(value(dialog, "dimensions")->text(), QStringLiteral("80 × 50"));
    }

    void ineligibleSelections_data()
    {
        QTest::addColumn<QString>("kind");
        for (const auto kind : {"outside", "directory", "symlink", "escape", "multiple", "remote", "text", "audio", "unknown", "missing"})
            QTest::newRow(kind) << QString::fromLatin1(kind);
    }
    void ineligibleSelections()
    {
        QFETCH(QString, kind);
        Tree tree;
        QVERIFY(tree.root(QStringLiteral("cloud")));
        QVERIFY(tree.dir(QStringLiteral("elsewhere")));
        QVERIFY(tree.file(QStringLiteral("cloud/a.jpg"), "hydrated"));
        QVERIFY(tree.file(QStringLiteral("elsewhere/a.jpg"), "hydrated"));
        QVERIFY(tree.symlink(QStringLiteral("elsewhere/a.jpg"), QStringLiteral("cloud/link.jpg")));
        QVERIFY(tree.symlink(QStringLiteral("elsewhere"), QStringLiteral("cloud/escape")));
        QString path = tree.path(QStringLiteral("cloud/a.jpg"));
        KFileItemList items = selection(path);
        if (kind == QLatin1String("outside")) items = selection(tree.path(QStringLiteral("elsewhere/a.jpg")));
        if (kind == QLatin1String("directory")) items = {KFileItem(url(tree.path(QStringLiteral("cloud"))), QStringLiteral("inode/directory"), S_IFDIR)};
        if (kind == QLatin1String("symlink")) items = selection(tree.path(QStringLiteral("cloud/link.jpg")));
        if (kind == QLatin1String("escape")) items = selection(tree.path(QStringLiteral("cloud/escape/a.jpg")));
        if (kind == QLatin1String("multiple")) items.append(items.first());
        if (kind == QLatin1String("remote")) items = {KFileItem(QUrl(QStringLiteral("https://example.invalid/a.jpg")), QStringLiteral("image/jpeg"), S_IFREG)};
        if (kind == QLatin1String("missing")) items = selection(tree.path(QStringLiteral("cloud/missing.jpg")));
        for (const auto ext : {"text", "audio", "unknown"}) {
            if (kind == QLatin1String(ext)) {
                const auto name = QStringLiteral("cloud/a.") + (kind == QLatin1String("text") ? QStringLiteral("txt") : kind == QLatin1String("audio") ? QStringLiteral("mp3") : QStringLiteral("unknownextension"));
                QVERIFY(tree.file(name, "hydrated"));
                items = selection(tree.path(name));
            }
        }
        KPropertiesDialog dialog(QStringLiteral("Properties"));
        QSignalSpy settled(tab(dialog, items), &DetailsTab::settled);
        QTRY_COMPARE(settled.size(), 1);
        QVERIFY(!page(dialog));
        QVERIFY(helperLog().isEmpty());
    }

    void failedExtraction_data()
    {
        QTest::addColumn<QString>("mode");
        for (const auto mode : {"empty", "garbage", "oversized", "failure", "crash", "timeout", "unknown-property"}) QTest::newRow(mode) << QString::fromLatin1(mode);
    }
    void failedExtraction()
    {
        QFETCH(QString, mode);
        Tree tree;
        QVERIFY(tree.root(QStringLiteral("cloud")));
        const auto name = QStringLiteral("cloud/") + mode + QStringLiteral(".jpg");
        QVERIFY(tree.file(name, "hydrated"));
        KPropertiesDialog dialog(QStringLiteral("Properties"));
        QSignalSpy settled(tab(dialog, selection(tree.path(name)), 300), &DetailsTab::settled);
        QTRY_COMPARE(settled.size(), 1);
        QVERIFY(!page(dialog));
        QVERIFY(!helperLog().isEmpty());
        const int pid = helperLog().section(QLatin1Char(' '), 0, 0).toInt();
        QTRY_VERIFY(::kill(pid, 0) == -1 && errno == ESRCH);
    }

    void missingChild_data()
    {
        QTest::addColumn<bool>("probe");
        QTest::newRow("missing-probe") << true;
        QTest::newRow("missing-extractor") << false;
    }
    void missingChild()
    {
        QFETCH(bool, probe);
        Tree tree;
        QVERIFY(tree.root(QStringLiteral("cloud")));
        QVERIFY(tree.file(QStringLiteral("cloud/local.jpg"), "hydrated"));
        KPropertiesDialog dialog(QStringLiteral("Properties"));
        auto worker = new DetailsTab(&dialog, selection(tree.path(QStringLiteral("cloud/local.jpg"))),
            probe ? QStringLiteral("/nonexistent/konedrive_details_probe") : QStringLiteral(KONEDRIVE_DETAILS_PROBE),
            probe ? QStringLiteral(KONEDRIVE_DETAILS_FIXTURE) : QStringLiteral("/nonexistent/baloo_filemetadata_temp_extractor"));
        QSignalSpy settled(worker, &DetailsTab::settled);
        QTRY_COMPARE(settled.size(), 1);
        QVERIFY(!page(dialog));
        QVERIFY(helperLog().isEmpty());
    }

    void closeKillsChild_data()
    {
        QTest::addColumn<bool>("probe");
        QTest::newRow("probe") << true;
        QTest::newRow("extractor") << false;
    }
    void closeKillsChild()
    {
        QFETCH(bool, probe);
        Tree tree;
        QVERIFY(tree.root(QStringLiteral("cloud")));
        QVERIFY(tree.file(QStringLiteral("cloud/timeout-close.jpg"), "hydrated"));
        QPointer<KPropertiesDialog> dialog = new KPropertiesDialog(QStringLiteral("Properties"));
        auto worker = new DetailsTab(dialog, selection(tree.path(QStringLiteral("cloud/timeout-close.jpg"))),
            probe ? QStringLiteral(KONEDRIVE_DETAILS_FIXTURE) : QStringLiteral(KONEDRIVE_DETAILS_PROBE), QStringLiteral(KONEDRIVE_DETAILS_FIXTURE));
        QSignalSpy settled(worker, &DetailsTab::settled);
        QTRY_VERIFY(!helperLog().isEmpty());
        const int pid = helperLog().section(QLatin1Char(' '), 0, 0).toInt();
        QElapsedTimer elapsed;
        elapsed.start();
        dialog->reject(); // KPropertiesDialog also schedules its own deletion
        QVERIFY(elapsed.elapsed() < 100);
        QTRY_VERIFY(::kill(pid, 0) == -1 && errno == ESRCH);
        QTRY_VERIFY(dialog.isNull());
        QCOMPARE(settled.size(), 1);
    }

    void probeTimeoutDoesNotBlock()
    {
        Tree tree;
        QVERIFY(tree.file(QStringLiteral("timeout-probe.jpg")));
        KPropertiesDialog dialog(QStringLiteral("Properties"));
        auto worker = new DetailsTab(&dialog, selection(tree.path(QStringLiteral("timeout-probe.jpg"))),
            QStringLiteral(KONEDRIVE_DETAILS_FIXTURE), QStringLiteral(KONEDRIVE_DETAILS_FIXTURE), 150);
        bool ticked = false;
        QTimer::singleShot(20, &dialog, [&ticked] { ticked = true; });
        QSignalSpy settled(worker, &DetailsTab::settled);
        QTRY_COMPARE(settled.size(), 1);
        QVERIFY(ticked);
        QVERIFY(!page(dialog));
        const int pid = helperLog().section(QLatin1Char(' '), 0, 0).toInt();
        QVERIFY(pid > 0);
        QTRY_VERIFY(::kill(pid, 0) == -1 && errno == ESRCH);
    }

    void validEmptyFactsDoNotFallBack()
    {
        Tree tree;
        QVERIFY(tree.root(QStringLiteral("cloud")));
        QVERIFY(tree.file(QStringLiteral("cloud/local.jpg"), "hydrated"));
        const auto path = tree.path(QStringLiteral("cloud/local.jpg"));
        QVERIFY(setAttribute(path, "user.konedrive.media", R"({"v":1,"kind":"photo"})"));
        KPropertiesDialog dialog(QStringLiteral("Properties"));
        QSignalSpy settled(tab(dialog, selection(path)), &DetailsTab::settled);
        QTRY_COMPARE(settled.size(), 1);
        QVERIFY(!page(dialog));
        QVERIFY(helperLog().isEmpty());
    }

    void nativePageCancelsExtraction()
    {
        Tree tree;
        QVERIFY(tree.root(QStringLiteral("cloud")));
        QVERIFY(tree.file(QStringLiteral("cloud/delayed.jpg"), "hydrated"));
        KPropertiesDialog dialog(QStringLiteral("Properties"));
        dialog.addPage(new QWidget, QStringLiteral("General"));
        QSignalSpy settled(tab(dialog, selection(tree.path(QStringLiteral("cloud/delayed.jpg")))), &DetailsTab::settled);
        QTRY_VERIFY(!helperLog().isEmpty());
        const int pid = helperLog().section(QLatin1Char(' '), 0, 0).toInt();
        dialog.addPage(new QWidget, detailsTitle());
        QTRY_COMPARE(settled.size(), 1);
        QTRY_VERIFY(::kill(pid, 0) == -1 && errno == ESRCH);
        QVERIFY(!page(dialog));
        QCOMPARE(dialog.findChild<KPageWidget *>()->model()->rowCount(), 2);
    }

    void repeatedValuesKeepBalooOrder()
    {
        PropertyMultiMap facts;
        facts.insert(Property::Artist, QStringLiteral("first"));
        facts.insert(Property::Artist, QStringLiteral("second"));
        facts.insert(Property::Artist, QStringLiteral("third"));
        QVariantList expected;
        for (auto it = facts.constBegin(); it != facts.constEnd(); ++it) expected.append(it.value());
        QCOMPARE(visibleDetails(facts).value(QStringLiteral("artist")).toList(), expected);
    }

    void largeRepeatedPropertyIsResponsive()
    {
        PropertyMultiMap facts;
        constexpr int count = 12000;
        for (int i = 0; i < count; ++i) facts.insert(Property::Artist, QString::number(i));
        QElapsedTimer timer;
        timer.start();
        const auto data = visibleDetails(facts);
        const auto elapsed = timer.elapsed();
        qInfo("12000 equal-key values: %lld ms", static_cast<long long>(elapsed));
        QCOMPARE(data.value(QStringLiteral("artist")).toList().size(), count);
        QVERIFY2(elapsed < 2000, qPrintable(QStringLiteral("12000 equal-key values took %1 ms (limit 2000 ms)").arg(elapsed)));
    }

    void localGpsRowsMirrorBaloo()
    {
        const PropertyMultiMap facts{{Property::PhotoGpsLatitude, -33.5}, {Property::PhotoGpsLongitude, -70.5}};
        auto data = visibleDetails(facts);
        QVERIFY(data.contains(QStringLiteral("gpsLocation")));
        QVERIFY(!data.contains(QStringLiteral("photoGpsLatitude")));
        QVERIFY(!data.contains(QStringLiteral("photoGpsLongitude")));
        std::unique_ptr<QWidget> rows(detailsRows(data));
        const auto label = rows->findChild<QLabel *>(QStringLiteral("L_gpsLocation"));
        const auto text = rows->findChild<QLabel *>(QStringLiteral("V_gpsLocation"));
        QVERIFY(label && text);
        QCOMPARE(label->text(), QStringLiteral("GPS Location:"));
        QCOMPARE(text->text(), QStringLiteral("33.5°S 70.5°W"));
        KConfig config(QStringLiteral("baloofileinformationrc"), KConfig::NoGlobals);
        config.group(QStringLiteral("Show")).writeEntry("gpsLocation", false);
        config.sync();
        QVERIFY(visibleDetails(facts).isEmpty());
    }

    void wrappedLabelsAreNotClipped_data()
    {
        QTest::addColumn<int>("width");
        for (int width : {379, 416, 600}) QTest::newRow(qPrintable(QString::number(width))) << width;
    }
    void wrappedLabelsAreNotClipped()
    {
        QFETCH(int, width);
        const PropertyMultiMap facts{{Property::Generator, QStringLiteral("HDR+ 1.0.612345678zd")},
            {Property::Model, QStringLiteral("Pixel 8 Pro")}, {Property::Width, 4080}, {Property::Height, 3072}};
        std::unique_ptr<QWidget> rows(detailsRows(visibleDetails(facts)));
        rows->setFont(QFont(QStringLiteral("Noto Sans"), 10));
        rows->resize(width, 344);
        rows->show();
        const auto label = rows->findChild<QLabel *>(QStringLiteral("L_generator"));
        QVERIFY(label);
        QTRY_VERIFY(label->isVisible() && label->width() > 0);
        QCoreApplication::processEvents();
        const int needed = label->heightForWidth(label->width());
        QVERIFY2(label->height() >= needed, qPrintable(QStringLiteral("label %1×%2 needs height %3")
            .arg(label->width()).arg(label->height()).arg(needed)));
    }

    void hiddenRowsAndDefaults()
    {
        const PropertyMultiMap facts{{Property::Width, 100}, {Property::Height, 80}, {Property::Model, QStringLiteral("Camera")},
            {Property::Channels, 2}, {Property::Rating, 5}, {Property::Comment, QStringLiteral("not editable")}};
        auto data = visibleDetails(facts);
        QVERIFY(data.contains(QStringLiteral("dimensions")));
        QVERIFY(!data.contains(QStringLiteral("width")));
        QVERIFY(!data.contains(QStringLiteral("channels")));
        KConfig config(QStringLiteral("baloofileinformationrc"), KConfig::NoGlobals);
        auto show = config.group(QStringLiteral("Show"));
        for (const auto key : {"dimensions", "model"}) show.writeEntry(key, false);
        for (const auto key : {"width", "height", "channels", "rating", "comment"}) show.writeEntry(key, true);
        config.sync();
        data = visibleDetails(facts);
        QVERIFY(!data.contains(QStringLiteral("dimensions")));
        QVERIFY(!data.contains(QStringLiteral("model")));
        QVERIFY(data.contains(QStringLiteral("width")));
        QVERIFY(data.contains(QStringLiteral("height")));
        QVERIFY(data.contains(QStringLiteral("channels")));
        QVERIFY(!data.contains(QStringLiteral("rating")));
        QVERIFY(!data.contains(QStringLiteral("comment")));
        SimpleExtractionResult result(QString(), QString(), ExtractionResult::ExtractMetaData);
        addMediaFacts(R"({"v":1,"kind":"photo","width":100,"height":80,"cameraModel":"Camera"})", &result);
        show.writeEntry("width", false); show.writeEntry("height", false); config.sync();
        QVERIFY(visibleDetails(result.properties()).isEmpty());
    }

    void coexistence_data()
    {
        QTest::addColumn<bool>("late");
        QTest::newRow("existing-native") << false;
        QTest::newRow("later-native") << true;
    }
    void coexistence()
    {
        QFETCH(bool, late);
        Tree tree;
        QVERIFY(tree.root(QStringLiteral("cloud")));
        QVERIFY(tree.file(QStringLiteral("cloud/photo.jpg"), "online-only"));
        const auto path = tree.path(QStringLiteral("cloud/photo.jpg"));
        QVERIFY(setAttribute(path, "user.konedrive.media", Photo));
        KPropertiesDialog dialog(QStringLiteral("Properties"));
        dialog.addPage(new QWidget, QStringLiteral("General"));
        if (!late) dialog.addPage(new QWidget, detailsTitle());
        QSignalSpy settled(tab(dialog, selection(path)), &DetailsTab::settled);
        QTRY_COMPARE(settled.size(), 1);
        QCOMPARE(bool(page(dialog)), late);
        if (late) {
            auto native = dialog.addPage(new QWidget, detailsTitle());
            QTRY_VERIFY(!page(dialog));
            auto model = dialog.findChild<KPageWidget *>()->model();
            QCOMPARE(model->rowCount(), 2);
            QVERIFY(native->widget());
        }
        QVERIFY(helperLog().isEmpty());
    }
};
QTEST_MAIN(DetailsTabTest)
#include "detailstabtest.moc"
