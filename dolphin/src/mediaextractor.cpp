// OneDrive's stored facets, without ever opening the file. The daemon serves
// the media attribute only while content is off disk; the visual state (which
// can say "hydrating" for on-disk content) must not be consulted here.
#include <KFileMetaData/ExtractorPlugin>

#include <QFile>
#include <QJsonDocument>
#include <QJsonObject>
#include <QRegularExpression>

#include <cmath>
#include <limits>
#include <sys/xattr.h>

using namespace KFileMetaData;

namespace
{
constexpr auto MediaAttribute = "user.konedrive.media";

void addInteger(ExtractionResult *result, const QJsonValue &value, Property::Property property)
{
    if (value.isDouble()) {
        const double number = value.toDouble();
        if (number >= 0 && number <= std::numeric_limits<int>::max() && std::floor(number) == number) {
            result->add(property, static_cast<int>(number));
        }
    }
}

void addDouble(ExtractionResult *result, const QJsonValue &value, Property::Property property, double divisor = 1.0)
{
    if (value.isDouble()) {
        const double number = value.toDouble() / divisor;
        if (std::isfinite(number) && number >= 0) {
            result->add(property, number);
        }
    }
}

void addString(ExtractionResult *result, const QJsonValue &value, Property::Property property)
{
    if (value.isString() && !value.toString().isEmpty()) {
        result->add(property, value.toString());
    }
}

void addCameraClock(ExtractionResult *result, const QJsonValue &value)
{
    // Graph appends Z to the camera's clock reading, rather than converting
    // it to UTC. Match that shape, discard subseconds, and parse as local.
    static const QRegularExpression shape(QStringLiteral("^[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}(?:\\.[0-9]+)?Z$"));
    const QString text = value.toString();
    if (!shape.match(text).hasMatch()) {
        return;
    }
    const QDateTime clock = QDateTime::fromString(text.first(19), QStringLiteral("yyyy-MM-dd'T'HH:mm:ss"));
    if (clock.isValid()) {
        result->add(Property::PhotoDateTimeOriginal, clock);
    }
}
} // namespace

class KonedriveMediaExtractor : public ExtractorPlugin
{
    Q_OBJECT
    Q_PLUGIN_METADATA(IID kfilemetadata_extractor_iid FILE "konedrive_mediaextractor.json")
    Q_INTERFACES(KFileMetaData::ExtractorPlugin)

public:
    KonedriveMediaExtractor()
        : ExtractorPlugin(nullptr)
    {
    }

    QStringList mimetypes() const override
    {
        // Union of KF6 6.24's exiv2 and ffmpeg lists, including BMFF types.
        return {QStringLiteral("image/avif"), QStringLiteral("image/bmp"), QStringLiteral("image/gif"),
                QStringLiteral("image/heif"), QStringLiteral("image/jp2"), QStringLiteral("image/jpeg"),
                QStringLiteral("image/jxl"), QStringLiteral("image/pgf"), QStringLiteral("image/png"),
                QStringLiteral("image/tiff"), QStringLiteral("image/webp"), QStringLiteral("image/x-canon-cr2"),
                QStringLiteral("image/x-canon-cr3"), QStringLiteral("image/x-canon-crw"), QStringLiteral("image/x-exv"),
                QStringLiteral("image/x-fuji-raf"), QStringLiteral("image/x-minolta-mrw"), QStringLiteral("image/x-nikon-nef"),
                QStringLiteral("image/x-olympus-orf"), QStringLiteral("image/x-panasonic-rw2"), QStringLiteral("image/x-pentax-pef"),
                QStringLiteral("image/x-photoshop"), QStringLiteral("image/x-samsung-srw"), QStringLiteral("image/x-tga"),
                QStringLiteral("video/mp2t"), QStringLiteral("video/mp4"), QStringLiteral("video/mpeg"),
                QStringLiteral("video/ogg"), QStringLiteral("video/quicktime"), QStringLiteral("video/vnd.avi"),
                QStringLiteral("video/webm"), QStringLiteral("video/x-flv"), QStringLiteral("video/x-matroska"),
                QStringLiteral("video/x-ms-asf"), QStringLiteral("video/x-ms-wmv"), QStringLiteral("video/x-msvideo")};
    }

    void extract(ExtractionResult *result) override
    {
        if (!(result->inputFlags() & ExtractionResult::ExtractMetaData)) {
            return;
        }
        const QByteArray path = QFile::encodeName(result->inputUrl());
        // Linux's xattr limit. One syscall, no content open, no symlink
        // traversal. Missing/unsupported attributes return before parsing
        // or allocating metadata (ordinary files and on-disk mount files).
        char buffer[65536];
        const ssize_t size = ::lgetxattr(path.constData(), MediaAttribute, buffer, sizeof buffer);
        if (size <= 0) {
            return;
        }
        const QJsonDocument document = QJsonDocument::fromJson(QByteArray::fromRawData(buffer, size));
        if (!document.isObject()) {
            return;
        }
        const QJsonObject media = document.object();
        const auto version = media.value(QStringLiteral("v"));
        if (!version.isDouble() || version.toDouble() != 1.0) {
            return;
        }
        const QString kind = media.value(QStringLiteral("kind")).toString();
        if (kind != QLatin1String("photo") && kind != QLatin1String("video")) {
            return;
        }
        const auto field = [&media](const char *name) { return media.value(QLatin1String(name)); };
        const int orientation = field("orientation").toInt();
        const bool swap = kind == QLatin1String("photo") && orientation >= 5 && orientation <= 8;
        addInteger(result, field(swap ? "height" : "width"), Property::Width);
        addInteger(result, field(swap ? "width" : "height"), Property::Height);
        if (kind == QLatin1String("video")) {
            result->addType(Type::Video);
            const auto duration = field("durationMs");
            if (duration.isDouble()) {
                // KF6 Duration is Int, as emitted by its ffmpeg extractor.
                addInteger(result, std::floor(duration.toDouble() / 1000.0), Property::Duration);
            }
            addDouble(result, field("frameRate"), Property::FrameRate);
            return;
        }
        result->addType(Type::Image);
        if (orientation >= 1 && orientation <= 8) {
            result->add(Property::ImageOrientation, orientation);
        }
        addString(result, field("cameraMake"), Property::Manufacturer);
        addString(result, field("cameraModel"), Property::Model);
        addDouble(result, field("fNumber"), Property::PhotoFNumber);
        addDouble(result, field("focalLength"), Property::PhotoFocalLength);
        addInteger(result, field("iso"), Property::PhotoISOSpeedRatings);
        const auto numerator = field("exposureNumerator");
        const auto denominator = field("exposureDenominator");
        if (numerator.isDouble() && denominator.isDouble() && denominator.toDouble() > 0) {
            addDouble(result, numerator, Property::PhotoExposureTime, denominator.toDouble());
        }
        addCameraClock(result, field("takenDateTime"));
    }
};

#include "mediaextractor.moc"
