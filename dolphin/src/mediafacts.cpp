// The media-details.md contract, shared by the extractor and Properties tab.
#include "mediafacts.h"
#include <QFile>
#include <QJsonDocument>
#include <QJsonObject>
#include <QRegularExpression>
#include <cmath>
#include <limits>
#include <sys/xattr.h>
using namespace KFileMetaData;
namespace {
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
namespace konedrive {
QByteArray readMediaAttribute(const QString &path)
{
    // Linux's xattr limit: one syscall, no open or final-symlink traversal.
    char buffer[65536];
    const auto size = ::lgetxattr(QFile::encodeName(path).constData(), "user.konedrive.media", buffer, sizeof buffer);
    return size > 0 ? QByteArray(buffer, size) : QByteArray();
}
void addMediaFacts(const QByteArray &payload, ExtractionResult *result)
{
    const QJsonDocument document = QJsonDocument::fromJson(payload);
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
} // namespace konedrive
