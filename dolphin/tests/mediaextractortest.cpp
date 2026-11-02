// The real plugin, with real xattrs, and an inotify watch over readable
// canaries: even when content can be opened, it must never be opened.
#include "testsupport.h"

#include <KFileMetaData/ExtractorPlugin>
#include <KFileMetaData/SimpleExtractionResult>

#include <QDateTime>
#include <QJsonObject>
#include <QPluginLoader>
#include <QStandardPaths>
#include <QTimeZone>

using namespace testsupport;
using namespace KFileMetaData;

class MediaExtractorTest : public QObject
{
    Q_OBJECT

    QPluginLoader m_loader{QStringLiteral(KONEDRIVE_MEDIA_PLUGIN)};
    ExtractorPlugin *m_plugin = nullptr;

    SimpleExtractionResult extract(const QString &path, const QString &mime = QStringLiteral("image/jpeg"))
    {
        SimpleExtractionResult result(path, mime, ExtractionResult::ExtractMetaData);
        m_plugin->extract(&result);
        return result;
    }

private Q_SLOTS:
    void initTestCase()
    {
        QStandardPaths::setTestModeEnabled(true);
        m_plugin = qobject_cast<ExtractorPlugin *>(m_loader.instance());
        QVERIFY2(m_plugin, qPrintable(m_loader.errorString()));
    }

    void registeredMimeTypes()
    {
        const auto metadata = m_loader.metaData();
        QCOMPARE(metadata.value(QStringLiteral("IID")).toString(), QStringLiteral("org.kde.kf5.kfilemetadata.ExtractorPlugin"));
        const auto embedded = metadata.value(QStringLiteral("MetaData")).toObject();
        QVERIFY(!embedded.value(QStringLiteral("Name")).toString().isEmpty());
        auto declared = embedded.value(QStringLiteral("MimeTypes")).toObject().keys();
        auto supported = m_plugin->mimetypes();
        declared.sort();
        supported.sort();
        QCOMPARE(supported, declared);
        QCOMPARE(supported.size(), 36);
        for (const auto *mime : {"image/jpeg", "image/png", "image/avif", "image/heif", "image/jxl", "image/x-canon-cr3", "video/mp4", "video/x-msvideo"}) {
            QVERIFY(supported.contains(QString::fromLatin1(mime)));
        }
    }

    void photoMapping()
    {
        Tree tree;
        QVERIFY(tree.file(QStringLiteral("photo.jpg")));
        const auto path = tree.path(QStringLiteral("photo.jpg"));
        QVERIFY(setAttribute(path, "user.konedrive.media", R"({"v":1,"kind":"photo","width":4080,"height":3072,"orientation":1,"cameraMake":"Google","cameraModel":"Pixel 8 Pro","fNumber":1.7,"exposureNumerator":1,"exposureDenominator":1052,"iso":41,"focalLength":6.9,"takenDateTime":"2024-03-16T14:35:00Z","location":{"latitude":1},"bitrate":123})"));
        const auto result = extract(path);
        const PropertyMultiMap expected{
            {Property::Width, 4080}, {Property::Height, 3072}, {Property::ImageOrientation, 1},
            {Property::Manufacturer, QStringLiteral("Google")}, {Property::Model, QStringLiteral("Pixel 8 Pro")},
            {Property::PhotoFNumber, 1.7}, {Property::PhotoExposureTime, 1.0 / 1052.0},
            {Property::PhotoISOSpeedRatings, 41}, {Property::PhotoFocalLength, 6.9},
            {Property::PhotoDateTimeOriginal, QDateTime(QDate(2024, 3, 16), QTime(14, 35), QTimeZone::LocalTime)}};
        QCOMPARE(result.properties(), expected);
        QCOMPARE(result.types(), QList<Type::Type>{Type::Image});
        QVERIFY(result.text().isEmpty());
    }

    void orientation_data()
    {
        QTest::addColumn<int>("orientation");
        QTest::addColumn<int>("width");
        QTest::addColumn<int>("height");
        for (int orientation : {1, 3, 5, 6, 7, 8}) {
            const bool swap = orientation >= 5;
            QTest::newRow(qPrintable(QString::number(orientation))) << orientation << (swap ? 4608 : 2112) << (swap ? 2112 : 4608);
        }
    }

    void orientation()
    {
        QFETCH(int, orientation);
        QFETCH(int, width);
        QFETCH(int, height);
        Tree tree;
        QVERIFY(tree.file(QStringLiteral("portrait.jpg")));
        const auto path = tree.path(QStringLiteral("portrait.jpg"));
        QVERIFY(setAttribute(path, "user.konedrive.media", QByteArray(R"({"v":1,"kind":"photo","width":2112,"height":4608,"orientation":)") + QByteArray::number(orientation) + '}'));
        const auto properties = extract(path).properties();
        QCOMPARE(properties.value(Property::Width).toInt(), width);
        QCOMPARE(properties.value(Property::Height).toInt(), height);
        QCOMPARE(properties.value(Property::ImageOrientation).toInt(), orientation);
    }

    void cameraClock_data()
    {
        QTest::addColumn<QByteArray>("date");
        QTest::addColumn<QTime>("time");
        QTest::newRow("whole-seconds") << QByteArray("2022-01-08T17:04:08Z") << QTime(17, 4, 8);
        QTest::newRow("fraction") << QByteArray("2022-01-08T18:05:59.61Z") << QTime(18, 5, 59);
    }

    void cameraClock()
    {
        QFETCH(QByteArray, date);
        QFETCH(QTime, time);
        QVERIFY(QTimeZone::systemTimeZone().offsetFromUtc(QDateTime::currentDateTime()) != 0);
        Tree tree;
        QVERIFY(tree.file(QStringLiteral("photo.jpg")));
        const auto path = tree.path(QStringLiteral("photo.jpg"));
        QVERIFY(setAttribute(path, "user.konedrive.media", QByteArray(R"({"v":1,"kind":"photo","takenDateTime":")") + date + "\"}"));
        const auto properties = extract(path).properties();
        QCOMPARE(properties.size(), 1);
        const auto value = properties.value(Property::PhotoDateTimeOriginal).toDateTime();
        QCOMPARE(value.date(), QDate(2022, 1, 8));
        QCOMPARE(value.time(), time);
        QCOMPARE(value.timeSpec(), Qt::LocalTime);
        QVERIFY(!properties.contains(Property::ImageDateTime));
    }

    void exposureRequiresBothPartsAndPositiveDenominator_data()
    {
        QTest::addColumn<QByteArray>("fields");
        QTest::newRow("no-numerator") << QByteArray("\"exposureDenominator\":25");
        QTest::newRow("no-denominator") << QByteArray("\"exposureNumerator\":1");
        QTest::newRow("zero") << QByteArray("\"exposureNumerator\":1,\"exposureDenominator\":0");
        QTest::newRow("negative") << QByteArray("\"exposureNumerator\":1,\"exposureDenominator\":-25");
        QTest::newRow("overflow") << QByteArray("\"exposureNumerator\":1e308,\"exposureDenominator\":1e-308");
    }

    void exposureRequiresBothPartsAndPositiveDenominator()
    {
        QFETCH(QByteArray, fields);
        Tree tree;
        QVERIFY(tree.file(QStringLiteral("photo.jpg")));
        const auto path = tree.path(QStringLiteral("photo.jpg"));
        QVERIFY(setAttribute(path, "user.konedrive.media", QByteArray(R"({"v":1,"kind":"photo",)") + fields + '}'));
        QVERIFY(extract(path).properties().isEmpty());
    }

    void videoMapping_data()
    {
        QTest::addColumn<QByteArray>("frameRate");
        QTest::newRow("present") << QByteArray(",\"frameRate\":29.97");
        QTest::newRow("absent") << QByteArray();
    }

    void videoMapping()
    {
        QFETCH(QByteArray, frameRate);
        Tree tree;
        QVERIFY(tree.file(QStringLiteral("clip.mp4")));
        const auto path = tree.path(QStringLiteral("clip.mp4"));
        QVERIFY(setAttribute(path, "user.konedrive.media", QByteArray(R"({"v":1,"kind":"video","width":1080,"height":1920,"durationMs":2323,"orientation":6,"cameraMake":"Ignored","cameraModel":"Ignored","takenDateTime":"2022-01-08T17:04:08Z","fNumber":1.7,"exposureNumerator":1,"exposureDenominator":25,"iso":500,"focalLength":4.25,"bitrate":100,"fourCC":"avc1","location":{"latitude":1})") + frameRate + '}'));
        const auto result = extract(path, QStringLiteral("video/mp4"));
        PropertyMultiMap expected{{Property::Width, 1080}, {Property::Height, 1920}, {Property::Duration, 2}};
        if (!frameRate.isEmpty()) {
            expected.insert(Property::FrameRate, 29.97);
        }
        QCOMPARE(result.properties(), expected);
        QCOMPARE(result.types(), QList<Type::Type>{Type::Video});
    }

    void durationIsTruncatedAndRangeChecked_data()
    {
        QTest::addColumn<QByteArray>("duration");
        QTest::addColumn<int>("seconds");
        QTest::newRow("minute-boundary") << QByteArray("59900") << 59;
        QTest::newRow("subsecond") << QByteArray("999") << 0;
        QTest::newRow("zero") << QByteArray("0") << 0;
        QTest::newRow("max-int-seconds") << QByteArray("2147483647999") << 2147483647;
        QTest::newRow("too-large") << QByteArray("2147483648000") << -1;
        QTest::newRow("huge") << QByteArray("1e308") << -1;
        QTest::newRow("negative") << QByteArray("-1") << -1;
        QTest::newRow("string") << QByteArray("\"59900\"") << -1;
    }

    void durationIsTruncatedAndRangeChecked()
    {
        QFETCH(QByteArray, duration);
        QFETCH(int, seconds);
        Tree tree;
        QVERIFY(tree.file(QStringLiteral("clip.mp4")));
        const auto path = tree.path(QStringLiteral("clip.mp4"));
        QVERIFY(setAttribute(path, "user.konedrive.media", QByteArray(R"({"v":1,"kind":"video","durationMs":)") + duration + '}'));
        const auto properties = extract(path, QStringLiteral("video/mp4")).properties();
        if (seconds < 0) {
            QVERIFY(!properties.contains(Property::Duration));
        } else {
            const auto value = properties.value(Property::Duration);
            QCOMPARE(value.toInt(), seconds);
            QCOMPARE(value.metaType().id(), QMetaType::Int);
        }
    }

    void invalidPayload_data()
    {
        QTest::addColumn<QByteArray>("json");
        QTest::newRow("bad-json") << QByteArray("{");
        QTest::newRow("array") << QByteArray("[]");
        QTest::newRow("unknown-version") << QByteArray(R"({"v":2,"kind":"photo","width":100})");
        QTest::newRow("fractional-version") << QByteArray(R"({"v":1.5,"kind":"photo","width":100})");
        QTest::newRow("string-version") << QByteArray(R"({"v":"1","kind":"photo","width":100})");
        QTest::newRow("missing-version") << QByteArray(R"({"kind":"photo","width":100})");
        QTest::newRow("unknown-kind") << QByteArray(R"({"v":1,"kind":"audio","width":100})");
        QTest::newRow("empty") << QByteArray();
    }

    void invalidPayload()
    {
        QFETCH(QByteArray, json);
        Tree tree;
        QVERIFY(tree.file(QStringLiteral("file.jpg")));
        const auto path = tree.path(QStringLiteral("file.jpg"));
        QVERIFY(setAttribute(path, "user.konedrive.media", json));
        const auto result = extract(path);
        QVERIFY(result.properties().isEmpty());
        QVERIFY(result.types().isEmpty());
    }

    void missingAttributeAndOnDiskFiles()
    {
        Tree tree;
        for (const auto &state : {QByteArray(), QByteArray("hydrated"), QByteArray("hydrating"), QByteArray("online-only")}) {
            const auto name = QString::fromLatin1(state) + QStringLiteral(".jpg");
            QVERIFY(tree.file(name, state));
            const auto result = extract(tree.path(name));
            QVERIFY(result.properties().isEmpty());
            QVERIFY(result.types().isEmpty());
        }
        QVERIFY(extract(tree.path(QStringLiteral("missing.jpg"))).properties().isEmpty());
    }

    void unsupportedAttributes()
    {
        // procfs supplies a real ENOTSUP, rather than a mocked error.
        char byte;
        QCOMPARE(::lgetxattr("/proc/self/status", "user.konedrive.media", &byte, 1), -1);
        QCOMPARE(errno, ENOTSUP);
        const auto result = extract(QStringLiteral("/proc/self/status"));
        QVERIFY(result.properties().isEmpty());
        QVERIFY(result.types().isEmpty());
    }

    void missingOrInvalidOptionalFields()
    {
        Tree tree;
        QVERIFY(tree.file(QStringLiteral("screenshot.png")));
        const auto path = tree.path(QStringLiteral("screenshot.png"));
        QVERIFY(setAttribute(path, "user.konedrive.media", R"({"v":1,"kind":"photo","width":1920,"height":1080})"));
        QCOMPARE(extract(path).properties(), (PropertyMultiMap{{Property::Width, 1920}, {Property::Height, 1080}}));
        QVERIFY(setAttribute(path, "user.konedrive.media", R"({"v":1,"kind":"photo","width":0,"height":0})"));
        QCOMPARE(extract(path).properties(), (PropertyMultiMap{{Property::Width, 0}, {Property::Height, 0}}));
        QVERIFY(setAttribute(path, "user.konedrive.media", R"({"v":1,"kind":"photo","width":true,"height":"1080","orientation":1.5,"cameraMake":42,"cameraModel":null,"takenDateTime":"2022-99-08T17:04:08Z","fNumber":"1.7","focalLength":false,"iso":-1})"));
        QVERIFY(extract(path).properties().isEmpty());
        SimpleExtractionResult noMetadata(path, QStringLiteral("image/png"), ExtractionResult::ExtractPlainText);
        m_plugin->extract(&noMetadata);
        QVERIFY(noMetadata.properties().isEmpty());
        QVERIFY(noMetadata.types().isEmpty());
        QVERIFY(noMetadata.text().isEmpty());
    }

    void attributeIsAuthoritativeAndContentIsNeverOpened()
    {
        Tree tree;
        const QByteArray media(R"({"v":1,"kind":"photo","width":4080,"height":3072})");
        QStringList paths;
        for (const auto &state : {QByteArray(), QByteArray("hydrated"), QByteArray("hydrating"), QByteArray("online-only"), QByteArray("future-state")}) {
            const auto name = QString::fromLatin1(state) + QStringLiteral(".jpg");
            QVERIFY(tree.file(name, state));
            const auto path = tree.path(name);
            QVERIFY(setAttribute(path, "user.konedrive.media", media));
            paths.append(path);
        }
        QVERIFY(tree.file(QStringLiteral("clip.mp4")));
        const auto video = tree.path(QStringLiteral("clip.mp4"));
        QVERIFY(setAttribute(video, "user.konedrive.media", R"({"v":1,"kind":"video","durationMs":2323})"));
        QVERIFY(tree.file(QStringLiteral("bad.jpg")));
        const auto bad = tree.path(QStringLiteral("bad.jpg"));
        QVERIFY(setAttribute(bad, "user.konedrive.media", "{"));
        QVERIFY(tree.file(QStringLiteral("on-disk.jpg")));
        OpenWatch watch({tree.path(QString())});
        QVERIFY(watch.isValid());
        for (const auto &path : paths) {
            const auto result = extract(path);
            QCOMPARE(result.properties().value(Property::Width).toInt(), 4080);
            QCOMPARE(result.properties().value(Property::Height).toInt(), 3072);
        }
        QCOMPARE(extract(video, QStringLiteral("video/mp4")).properties().value(Property::Duration).toInt(), 2);
        QVERIFY(extract(bad).properties().isEmpty());
        QVERIFY(extract(tree.path(QStringLiteral("on-disk.jpg"))).properties().isEmpty());
        QCOMPARE(watch.take(), QStringList());
        // Prove the watch detects the prohibited operation on the same canary.
        QFile canary(paths.first());
        QVERIFY(canary.open(QIODevice::ReadOnly));
        QVERIFY(!watch.take().isEmpty());
    }
};

QTEST_MAIN(MediaExtractorTest)
#include "mediaextractortest.moc"
